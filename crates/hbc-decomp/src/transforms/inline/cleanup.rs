use crate::ir::{
    is_nan_check, is_undefined_expr, AssignTarget, Binding, Expression, Statement, Value,
};

use super::esm_cleanup::{inline_hoisted_aliases_and_trim, remove_esm_boilerplate};

fn is_self_assign_value(var: &crate::ir::Var, value: &Expression) -> bool {
    let name = var.name.as_str();
    match value {
        // One spelling, two variables: a store into an environment slot of a
        // value named after it (`sum = sum` for `slot = local`) is the store.
        Expression::Value(Value::Binding(Binding::Variable(val))) => {
            name == val.name && !matches!((var.slot, val.slot), (Some(a), Some(b)) if a != b)
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
    // Names the whole body writes without a stamp: its own locals.
    local_writes: std::collections::HashSet<String>,
    // Stores into a slot dropped as `x = x` where only the target is stamped.
    dropped: Vec<(String, crate::ir::SlotRef)>,
}

thread_local! {
    static MERGE: std::cell::RefCell<Merge> = std::cell::RefCell::new(Merge::default());
}

pub fn cleanup_noise(stmts: Vec<Statement>) -> Vec<Statement> {
    MERGE.with(|m| {
        let mut m = m.borrow_mut();
        if m.depth == 0 {
            m.local_writes = unstamped_writes(&stmts);
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
fn stamp_merged(stmts: &mut [Statement], dropped: &[(String, crate::ir::SlotRef)]) {
    use crate::ir::MutVisitor;
    let mut by_name: std::collections::HashMap<&str, Option<crate::ir::SlotRef>> =
        std::collections::HashMap::new();
    for (n, s) in dropped {
        by_name
            .entry(n.as_str())
            .and_modify(|e| {
                if *e != Some(*s) {
                    *e = None;
                }
            })
            .or_insert(Some(*s));
    }
    struct Stamp<'a>(&'a std::collections::HashMap<&'a str, Option<crate::ir::SlotRef>>);
    impl Stamp<'_> {
        fn var(&self, v: &mut crate::ir::Var) {
            if v.slot.is_none() {
                if let Some(Some(s)) = self.0.get(v.name.as_str()) {
                    v.slot = Some(*s);
                }
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
    let mut st = Stamp(&by_name);
    for s in stmts.iter_mut() {
        st.visit_statement(s);
    }
}

fn cleanup_noise_here(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut result = Vec::new();

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
                                    m.dropped.push((name.name.clone(), slot));
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
                    // If this is the last statement, drop it entirely
                    if i == stmts.len() - 1 {
                        continue;
                    }
                    // Otherwise convert to bare return
                    result.push(Statement::Return(None));
                } else {
                    result.push(stmt.clone());
                }
            }
            // Remove trailing bare `return;` if it's the last statement
            Statement::Return(None) if i == stmts.len() - 1 => {
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
    for stmt in &mut result {
        cleanup_noise_recurse(stmt);
    }

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
        ) || matches!(s, Statement::Continue(_))
            || matches!(s, Statement::Comment(_))
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
