use crate::ir::{
    is_nan_check, is_undefined_expr, AssignTarget, Binding, Expression, Statement, Value,
};

use super::esm_cleanup::{inline_hoisted_aliases_and_trim, remove_esm_boilerplate};

fn is_self_assign_value(var: &crate::ir::Var, value: &Expression) -> bool {
    let name = var.name.as_str();
    match value {
        // One spelling, two variables: a store into an environment slot of a
        // value named after it (`sum = sum` for `slot = local`) is the store. The
        // other way round, a local given an enclosing variable's value
        // (`num = num` for `local = slot`), is a read of another variable: dropped,
        // `num = 0; if (c) num = <outer num>` lost its `if`.
        //
        // Two registers' locals of one spelling are two variables too, and
        // `x = x` between them is a copy; but it prints as `x = x` until names
        // come from identities (IDENTITY_PLAN.md A3 step 5). Dropped as before.
        Expression::Value(Value::Binding(Binding::Variable(val))) => {
            name == val.name
                && !matches!((var.slot, val.slot), (Some(a), Some(b)) if a != b)
                && !(var.slot.is_none() && val.slot.is_some())
        }
        Expression::Value(Value::Parameter(idx)) => name == format!("arg{idx}"),
        Expression::Value(Value::Constant(crate::ir::Constant::Bool(b))) => {
            (*b && name == "true") || (!*b && name == "false")
        }
        Expression::Value(Value::Constant(crate::ir::Constant::Null)) => name == "null",
        Expression::Value(Value::Constant(crate::ir::Constant::Undefined)) => name == "undefined",
        Expression::Value(Value::Global) => name == "globalThis",
        // `Error = globalThis.Error`, the Babel pattern that captures a global
        // BUILTIN into a same-named local (redundant, since bare `Error` already
        // resolves to the global). Only safe for actual builtins: for a user-local
        // like `f`, `f = globalThis.f` reads the global property into the local and
        // must NOT be dropped (it is a real, possibly-conditional assignment).
        Expression::Member {
            object,
            property: crate::ir::PropertyKey::Ident(prop),
            ..
        } => {
            prop == name
                && crate::ir::expr::display::is_builtin_global(name)
                && match &**object {
                    Expression::Value(Value::Global) => true,
                    Expression::Value(Value::Binding(Binding::Variable(v))) => v == "globalThis",
                    _ => false,
                }
        }
        _ => false,
    }
}

// What one top-level `cleanup_noise` call shares with its recursion.
#[derive(Default)]
struct Merge {
    depth: u32,
    // Whether the end of the list being cleaned is the end of the function. A
    // trailing `return;` is only noise there: at the end of an `if` arm with code
    // after the `if`, of a loop body or of a `case`, it is what stops that code.
    tail: bool,
    // Names the whole body writes without a stamp: its own locals. By name,
    // like the merge within one function (see `stamp_merged`).
    local_writes: std::collections::HashSet<String>,
    // Stores into a slot dropped as `x = x` where only the target is stamped:
    // the name, the slot and the local merged into it.
    dropped: Vec<(String, crate::ir::SlotRef, Option<crate::ir::LocalRef>)>,
}

thread_local! {
    static MERGE: std::cell::RefCell<Merge> = std::cell::RefCell::new(Merge::default());
}

pub fn cleanup_noise(stmts: Vec<Statement>) -> Vec<Statement> {
    MERGE.with(|m| {
        let mut m = m.borrow_mut();
        if m.depth == 0 {
            m.local_writes = unstamped_writes(&stmts);
            m.tail = true;
        }
        m.depth += 1;
    });
    let mut result = cleanup_noise_here(stmts);
    let dropped = MERGE.with(|m| {
        let mut m = m.borrow_mut();
        m.depth -= 1;
        if m.depth == 0 {
            m.local_writes.clear();
            std::mem::take(&mut m.dropped)
        } else {
            Vec::new()
        }
    });
    if !dropped.is_empty() {
        stamp_merged(&mut result, &dropped);
    }
    result
}

fn unstamped_writes(stmts: &[Statement]) -> std::collections::HashSet<String> {
    use crate::ir::Visitor;
    struct W(std::collections::HashSet<String>);
    impl<'a> Visitor<'a> for W {
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                if name.slot.is_none() {
                    self.0.insert(name.name.clone());
                }
            }
            self.walk_statement(s);
        }
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(v)) = t {
                if v.slot.is_none() {
                    self.0.insert(v.name.clone());
                }
            }
            self.walk_assign_target(t);
        }
    }
    let mut w = W(std::collections::HashSet::new());
    for s in stmts {
        w.visit_statement(s);
    }
    w.0
}

// `slot = local` printed `x = x`: the naming gave a value the name of the slot
// it is stored into, so the two are one variable from here on. Dropping the
// store is right only if the local's other references are the slot's too:
// stamp them, or a later pass takes the unstamped write before the store
// (`x = x + 1`) for a local of its own and the store is lost for good.
//
// The local's references are those of its spelling from its own function: a
// pass may have rebuilt some without an identity, and another register of that
// function under the name cannot be printed apart from it until names come from
// identities (A3 step 5). A local of another function, inlined beside it, is
// another variable and keeps no stamp. A name merged into two slots stamps
// nothing, and a merged value with no identity stamps by name, as before.
fn stamp_merged(
    stmts: &mut [Statement],
    dropped: &[(String, crate::ir::SlotRef, Option<crate::ir::LocalRef>)],
) {
    use crate::ir::{MutVisitor, SlotRef};
    use std::collections::HashMap;
    fn put<K: std::hash::Hash + Eq>(m: &mut HashMap<K, Option<SlotRef>>, k: K, s: SlotRef) {
        m.entry(k)
            .and_modify(|e| {
                if *e != Some(s) {
                    *e = None;
                }
            })
            .or_insert(Some(s));
    }
    // Each merged spelling's slot,
    let mut by_name: HashMap<&str, Option<SlotRef>> = HashMap::new();
    // and the functions whose locals were merged under it; none when a merged
    // value had no identity.
    let mut functions: HashMap<&str, Option<std::collections::HashSet<u32>>> = HashMap::new();
    for (n, s, l) in dropped {
        put(&mut by_name, n.as_str(), *s);
        let f = functions
            .entry(n.as_str())
            .or_insert_with(|| Some(Default::default()));
        match (f.as_mut(), l) {
            (Some(f), Some(l)) => {
                f.insert(l.function);
            }
            _ => *f = None,
        }
    }
    struct Stamp<'a> {
        by_name: &'a HashMap<&'a str, Option<SlotRef>>,
        functions: &'a HashMap<&'a str, Option<std::collections::HashSet<u32>>>,
    }
    impl Stamp<'_> {
        fn var(&self, v: &mut crate::ir::Var) {
            if v.slot.is_some() {
                return;
            }
            let other_function = match (v.local, self.functions.get(v.name.as_str())) {
                (Some(l), Some(Some(f))) => !f.contains(&l.function),
                _ => false,
            };
            if other_function {
                return;
            }
            let found = self.by_name.get(v.name.as_str());
            if let Some(Some(s)) = found {
                v.slot = Some(*s);
            }
        }
    }
    impl MutVisitor for Stamp<'_> {
        fn visit_statement(&mut self, s: &mut Statement) {
            if let Statement::Let { name, .. } = s {
                self.var(name);
            }
            self.walk_statement(s);
        }
        fn visit_expression(&mut self, e: &mut Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                self.var(v);
            }
            self.walk_expression(e);
        }
        fn visit_assign_target(&mut self, t: &mut AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(v)) = t {
                self.var(v);
            }
            self.walk_assign_target(t);
        }
    }
    let mut st = Stamp {
        by_name: &by_name,
        functions: &functions,
    };
    for s in stmts.iter_mut() {
        st.visit_statement(s);
    }
}

fn cleanup_noise_here(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();
    let tail = MERGE.with(|m| m.borrow().tail);

    for (i, stmt) in stmts.iter().enumerate() {
        match stmt {
            // Remove self-assignments: `x = x;` and `x = globalThis.x;` (Babel global captures)
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            } => {
                if is_self_assign_value(name, value) {
                    if let (Some(slot), Expression::Value(Value::Binding(Binding::Variable(v)))) =
                        (name.slot, value)
                    {
                        if v.slot.is_none() {
                            // Only the target is the slot. A value the body
                            // writes is its local, one variable with the slot
                            // from here on: stamp it so. One it only reads (a
                            // captured parameter) is the slot's binding as
                            // printed, and the store has nothing to add.
                            MERGE.with(|m| {
                                let mut m = m.borrow_mut();
                                if m.local_writes.contains(&v.name) {
                                    m.dropped.push((name.name.clone(), slot, v.local));
                                }
                            });
                        }
                    }
                    continue;
                }
                result.push(stmt.clone());
            }
            // Remove self-assignments in let declarations: `let Error = Error;` (Babel global captures)
            Statement::Let { name, value, .. } => {
                if is_self_assign_value(name, value) {
                    continue;
                }
                result.push(stmt.clone());
            }
            // `return undefined;` -> `return;` or remove if last statement
            Statement::Return(Some(expr)) => {
                if is_undefined_expr(expr) {
                    // Last in the function: drop it entirely
                    if tail && i == stmts.len() - 1 {
                        continue;
                    }
                    // Otherwise convert to bare return
                    result.push(Statement::Return(None));
                } else {
                    result.push(stmt.clone());
                }
            }
            // Remove trailing bare `return;` if it's the last statement
            Statement::Return(None) if tail && i == stmts.len() - 1 => {
                continue;
            }
            // Remove dead `while (x !== x)` loops (NaN check artifact from Hermes bytecode)
            // `x !== x` is only true when x is NaN, but these loops are always dead code
            Statement::While { condition, .. } if is_nan_check(condition) => {
                continue;
            }
            // Remove empty while/for loops (artifact of structure recovery)
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::For { body, .. }
            | Statement::ForIn { body, .. }
            | Statement::ForOf { body, .. }
                if body.is_empty() =>
            {
                continue;
            }
            // Remove ESM interop guard: `if (!this) { BODY } else { ... }` -> BODY
            // In ESM, `this` is always undefined, so the then-branch always executes
            Statement::If {
                condition,
                then_body,
                ..
            } if is_not_this(condition) => {
                for s in then_body {
                    result.push(s.clone());
                }
                continue;
            }
            _ => result.push(stmt.clone()),
        }
    }

    // Remove ESM boilerplate patterns
    result = remove_esm_boilerplate(result);

    // Inline hoisted parameter aliases and remove dead code after return/throw
    result = inline_hoisted_aliases_and_trim(result);

    // Recurse into sub-blocks
    let last = result.len().wrapping_sub(1);
    for (i, stmt) in result.iter_mut().enumerate() {
        let inner = tail
            && i == last
            && matches!(
                stmt,
                Statement::If { .. } | Statement::TryCatch { .. } | Statement::Block(_)
            );
        MERGE.with(|m| m.borrow_mut().tail = inner);
        cleanup_noise_recurse(stmt);
    }
    MERGE.with(|m| m.borrow_mut().tail = tail);

    // Post-recursion: remove empty loops, empty if blocks, and empty blocks
    result.retain(|stmt| match stmt {
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => !is_effectively_empty_body(body),
        Statement::If {
            then_body,
            else_body,
            ..
        } => !is_effectively_empty_body(then_body) || !is_effectively_empty_body(else_body),
        Statement::Block(inner) => !is_effectively_empty_body(inner),
        _ => true,
    });

    result
}

// Check if expression is `!this` (ESM interop guard condition)
fn is_not_this(expr: &Expression) -> bool {
    if let Expression::Unary {
        op: crate::ir::UnaryOp::Not,
        operand,
    } = expr
    {
        return matches!(&**operand, Expression::Value(Value::This));
    }
    false
}

// Check if a body is effectively empty (no meaningful statements).
fn is_effectively_empty_body(stmts: &[Statement]) -> bool {
    stmts.iter().all(|s| {
        matches!(s,
            Statement::Block(inner) if inner.is_empty() || is_effectively_empty_body(inner)
        ) || matches!(s, Statement::Comment(_))
    })
}

fn cleanup_noise_recurse(stmt: &mut Statement) {
    crate::ir::map_nested_bodies_mut(stmt, cleanup_noise);

    // Post-pass: invert empty then-body: `if (x) {} else { ... }` -> `if (!x) { ... }`
    if let Statement::If {
        condition,
        then_body,
        else_body,
    } = stmt
    {
        if then_body.is_empty() && !else_body.is_empty() {
            let mut temp_else = std::mem::take(else_body);
            std::mem::swap(then_body, &mut temp_else);
            let old_cond = std::mem::replace(
                condition,
                Expression::Value(Value::Constant(crate::ir::Constant::Undefined)),
            );
            *condition = crate::transforms::logic_simplify::negate_expr(old_cond);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Constant, VarKind};

    #[test]
    fn return_undefined_stops_the_code_after_its_arm() {
        let call = || {
            Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(
                "f".into(),
            ))))
        };
        let ret = || Statement::Return(Some(Expression::constant(Constant::Undefined)));
        let arm = |then_body| Statement::If {
            condition: Expression::Value(Value::Binding(Binding::Variable("a".into()))),
            then_body,
            else_body: vec![],
        };
        // Only a return in the arm: the whole `if` went with it.
        let kept = cleanup_noise(vec![arm(vec![ret()]), call()]);
        assert_eq!(kept.len(), 2, "the `if` is gone: {kept:?}");
        let body = vec![Statement::While {
            condition: Expression::Value(Value::Binding(Binding::Variable("a".into()))),
            body: vec![call(), ret()],
        }];
        let kept = cleanup_noise(body);
        assert!(
            matches!(&kept[0], Statement::While { body, .. } if body.len() == 2),
            "a loop body's end is not the function's: {kept:?}"
        );
        let dropped = cleanup_noise(vec![arm(vec![call(), ret()])]);
        assert!(
            matches!(&dropped[0], Statement::If { then_body, .. } if then_body.len() == 1),
            "in tail position: {dropped:?}"
        );
    }

    #[test]
    fn a_bare_return_stops_the_code_after_its_arm() {
        let call = || {
            Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(
                "f".into(),
            ))))
        };
        let arm = |then_body| Statement::If {
            condition: Expression::Value(Value::Binding(Binding::Variable("a".into()))),
            then_body,
            else_body: vec![],
        };
        let kept = cleanup_noise(vec![arm(vec![call(), Statement::Return(None)]), call()]);
        assert!(
            matches!(&kept[0], Statement::If { then_body, .. } if then_body.len() == 2),
            "code follows the arm: {kept:?}"
        );
        // Nothing follows: the function ends here anyway.
        let dropped = cleanup_noise(vec![arm(vec![call(), Statement::Return(None)])]);
        assert!(
            matches!(&dropped[0], Statement::If { then_body, .. } if then_body.len() == 1),
            "in tail position: {dropped:?}"
        );
    }

    fn local(name: &str, reg: u32) -> crate::ir::Var {
        crate::ir::Var::with_local(name, crate::ir::LocalRef { function: 7, reg })
    }

    fn read(v: crate::ir::Var) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(v)))
    }

    fn assign(target: crate::ir::Var, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(target)),
            value,
        }
    }

    // The store `slot = x` merges function 7's `x` into the slot, register 1's
    // and the others of that function under the name; function 8's `x`,
    // inlined beside it, is another variable and keeps no stamp.
    #[test]
    fn a_merge_stamps_the_merged_register_only() {
        let slot = crate::ir::SlotRef { owner: 3, slot: 0 };
        let stamped = crate::ir::Var::with_slot("x", Some(slot));
        let one = || Expression::constant(crate::ir::Constant::Integer(1));
        let out = cleanup_noise(vec![
            assign(local("x", 1), one()),
            assign(stamped, read(local("x", 1))),
            assign(
                crate::ir::Var::with_local(
                    "x",
                    crate::ir::LocalRef {
                        function: 8,
                        reg: 1,
                    },
                ),
                one(),
            ),
            Statement::Expr(read(local("x", 3))),
        ]);
        let slots: Vec<_> = out
            .iter()
            .map(|s| match s {
                Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(v)),
                    ..
                }
                | Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(v)))) => {
                    v.slot
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(slots, vec![Some(slot), None, Some(slot)], "{out:?}");
    }

    #[test]
    fn test_let_self_assign_variable_removed() {
        // `let Error = Error;` should be removed (Babel global capture)
        let stmts = vec![Statement::Let {
            name: "Error".into(),
            value: Expression::Value(Value::Binding(Binding::Variable("Error".into()))),
            kind: VarKind::Let,
        }];
        let result = cleanup_noise(stmts);
        assert!(
            result.is_empty(),
            "Self-assign `let Error = Error` should be removed"
        );
    }

    #[test]
    fn test_let_self_assign_constant_removed() {
        // `let null = null;` and `let undefined = undefined;` should be removed
        let stmts = vec![
            Statement::Let {
                name: "null".into(),
                value: Expression::Value(Value::Constant(Constant::Null)),
                kind: VarKind::Let,
            },
            Statement::Let {
                name: "undefined".into(),
                value: Expression::Value(Value::Constant(Constant::Undefined)),
                kind: VarKind::Let,
            },
        ];
        let result = cleanup_noise(stmts);
        assert!(result.is_empty(), "Self-assign constants should be removed");
    }

    #[test]
    fn test_let_non_self_assign_preserved() {
        // `let x = y;` should NOT be removed (different names)
        let stmts = vec![Statement::Let {
            name: "x".into(),
            value: Expression::Value(Value::Binding(Binding::Variable("y".into()))),
            kind: VarKind::Let,
        }];
        let result = cleanup_noise(stmts);
        assert_eq!(result.len(), 1, "Non-self-assign should be preserved");
    }

    #[test]
    fn test_let_global_member_self_assign_removed() {
        // `let Error = globalThis.Error;` should be removed
        let stmts = vec![Statement::Let {
            name: "Error".into(),
            value: Expression::Member {
                object: Box::new(Expression::Value(Value::Global)),
                property: crate::ir::PropertyKey::Ident("Error".to_string()),
                optional: false,
            },
            kind: VarKind::Let,
        }];
        let result = cleanup_noise(stmts);
        assert!(
            result.is_empty(),
            "Global member self-assign should be removed"
        );
    }
}
