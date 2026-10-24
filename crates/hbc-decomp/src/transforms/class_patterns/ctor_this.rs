// Undo the HBC >= 97 lowering of a base-class constructor.
//
// hermesc compiles `constructor(x) { this.#f = x; }` as a plain function that
// builds its own receiver and returns it:
//
//   obj = Object.create(new.target.prototype);
//   if (#K in obj) { throw new TypeError("Cannot initialize private field twice."); }
//   obj[#K] = undefined;          // the brand
//   obj[#f] = undefined;          // each private field, before the body runs
//   obj[#f] = x;
//   return obj;
//
// Printed as is inside `class K { #f; … }` that is wrong: the class installs
// `#f` on `this`, not on `obj`, so the first `obj.#f` throws. The receiver is
// `this` again, the guard and the installs are what the field declarations do
// already, and the trailing `return obj` is what a constructor does anyway.
// The receiver keeps its name as an alias of `this`: an arrow made in the
// constructor reads it through its slot.
//
// A derived class's implicit constructor (`constructor(...args) {
// super(...args); }`, written by the compiler when the class has fields) gets
// its receiver from `HermesBuiltin.applyArguments(this, new.target)` instead.
// Printed as is it reads `this` before `super()` and throws; it is
// `super(...arguments)` followed by the same alias.

use crate::ir::{AssignTarget, Binding, Constant, Expression, Statement, Value, Visitor};

const DOUBLE_INIT: &str = "Cannot initialize private field twice.";

pub fn restore_constructor_this(mut stmts: Vec<Statement>) -> Vec<Statement> {
    let Some((at, (receiver, derived))) = stmts
        .iter()
        .enumerate()
        .find_map(|(i, s)| receiver_def(s).map(|b| (i, b)))
    else {
        return stmts;
    };
    // Only a constructor that hands back its own receiver: one that returns
    // something else replaces the instance, and `this` would be wrong there.
    if !returns_only(&stmts, &receiver) {
        return stmts;
    }
    match &mut stmts[at] {
        Statement::Let { value, .. } | Statement::Assign { value, .. } => {
            *value = Expression::Value(Value::This);
        }
        _ => unreachable!(),
    }

    let mut out: Vec<Statement> = stmts.drain(..=at).collect();
    if derived {
        let super_call = Statement::Expr(Expression::Call {
            callee: Box::new(Expression::Value(Value::Super)),
            arguments: vec![Expression::Spread(Box::new(Expression::Value(
                Value::Arguments,
            )))],
        });
        out.insert(at, super_call);
    }
    let mut rest: Vec<Statement> = stmts;
    // The guard: the branch that throws goes, the other stays in place, and
    // the installs open it: one `X[name] = undefined` per private name, the
    // brand first. The names are still slot reads here (closures resolve
    // later), so the run is recognised by its place and shape.
    if let Some(pos) = rest
        .iter()
        .position(|s| double_init_guard(s, &receiver).is_some())
    {
        let mut kept = double_init_guard(&rest[pos], &receiver).unwrap();
        // Copies the later cleanup drops (`tmp = arg0`) sit between them.
        let run = kept
            .iter()
            .take_while(|s| is_install(s, &receiver) || is_value_copy(s))
            .count();
        let mut i = 0;
        kept.retain(|s| {
            i += 1;
            !(i <= run && is_install(s, &receiver))
        });
        rest.splice(pos..=pos, kept);
    }
    if matches!(rest.last(), Some(Statement::Return(Some(e))) if is_binding(e, &receiver)) {
        rest.pop();
    }
    out.extend(rest);
    out
}

// `X = Object.create(new.target.prototype)` (false) or
// `X = HermesBuiltin.applyArguments(this, new.target)` (true, derived) at the
// top of the body.
fn receiver_def(stmt: &Statement) -> Option<(Binding, bool)> {
    let (binding, value) = match stmt {
        Statement::Assign {
            target: AssignTarget::Binding(b),
            value,
        } => (b.clone(), value),
        Statement::Let { name, value, .. } => (Binding::Variable(name.clone()), value),
        _ => return None,
    };
    if super::analyzer::is_object_create_new_target(value) {
        return Some((binding, false));
    }
    is_super_forwarding(value).then_some((binding, true))
}

// `HermesBuiltin.applyArguments([r,] this, new.target)`.
fn is_super_forwarding(value: &Expression) -> bool {
    let Expression::Call { callee, arguments } = value else {
        return false;
    };
    let Expression::Member {
        object, property, ..
    } = callee.as_ref()
    else {
        return false;
    };
    matches!(property, crate::ir::PropertyKey::Ident(p) if p == "applyArguments")
        && matches!(object.to_string().as_str(), "HermesBuiltin" | "globalThis.HermesBuiltin")
        // A leading argument, the receiver CreateThisForSuper made, is still
        // there at this point; a later pass drops it.
        && matches!(
            arguments.as_slice(),
            [.., Expression::Value(Value::This), Expression::Value(Value::NewTarget)]
                if arguments.len() <= 3
        )
}

fn is_binding(expr: &Expression, binding: &Binding) -> bool {
    matches!(expr, Expression::Value(Value::Binding(b)) if b == binding)
}

// Every `return` in the body (nested functions are separate IR) returns the
// receiver, and at least one does.
fn returns_only(stmts: &[Statement], receiver: &Binding) -> bool {
    struct R<'a> {
        receiver: &'a Binding,
        ok: bool,
        seen: bool,
    }
    impl<'a, 'b> Visitor<'b> for R<'a> {
        fn visit_statement(&mut self, s: &'b Statement) {
            if let Statement::Return(v) = s {
                self.seen = true;
                if !v.as_ref().is_some_and(|e| is_binding(e, self.receiver)) {
                    self.ok = false;
                }
            }
            self.walk_statement(s);
        }
    }
    let mut r = R {
        receiver,
        ok: true,
        seen: false,
    };
    for s in stmts {
        r.visit_statement(s);
    }
    r.ok && r.seen
}

// `if (#P in X) { throw new TypeError(DOUBLE_INIT) } else { rest }`, in either
// orientation: the statements of the branch that does not throw.
fn double_init_guard(stmt: &Statement, receiver: &Binding) -> Option<Vec<Statement>> {
    let Statement::If {
        condition,
        then_body,
        else_body,
    } = stmt
    else {
        return None;
    };
    let (positive, test) = match condition {
        Expression::Unary {
            op: crate::ir::UnaryOp::Not,
            operand,
        } => (false, operand.as_ref()),
        e => (true, e),
    };
    let Expression::Binary {
        op: crate::ir::BinaryOp::In,
        left,
        right,
    } = test
    else {
        return None;
    };
    if !matches!(left.as_ref(), Expression::Value(Value::Binding(_)))
        || !is_binding(right, receiver)
    {
        return None;
    }
    let (throws, kept) = if positive {
        (then_body, else_body)
    } else {
        (else_body, then_body)
    };
    let throws_double_init = matches!(throws.last(),
        Some(Statement::Throw(e)) if e.to_string().contains(DOUBLE_INIT));
    throws_double_init.then(|| kept.clone())
}

// `x = <value>`: a copy with no effect of its own.
fn is_value_copy(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::Assign {
            target: AssignTarget::Binding(_),
            value: Expression::Value(_),
        }
    )
}

// `X[name] = undefined`, `name` a binding (a private name, or the slot that
// holds one).
fn is_install(stmt: &Statement, receiver: &Binding) -> bool {
    matches!(stmt, Statement::Assign {
        target: AssignTarget::Index { object, key: Expression::Value(Value::Binding(_)) },
        value: Expression::Value(Value::Constant(Constant::Undefined)),
    } if is_binding(object, receiver))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(n.into())))
    }

    fn create() -> Expression {
        Expression::Call {
            callee: Box::new(Expression::member(var("Object"), "create")),
            arguments: vec![Expression::member(
                Expression::Value(Value::NewTarget),
                "prototype",
            )],
        }
    }

    fn store(key: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Index {
                object: var("obj"),
                key: var(key),
            },
            value,
        }
    }

    #[test]
    fn a_lowered_constructor_gets_its_receiver_back() {
        let undef = Expression::Value(Value::Constant(Constant::Undefined));
        let throw = Statement::Throw(Expression::New {
            callee: Box::new(var("TypeError")),
            arguments: vec![Expression::Value(Value::Constant(Constant::String(
                DOUBLE_INIT.into(),
            )))],
        });
        let body = vec![
            Statement::Let {
                name: "obj".into(),
                value: create(),
                kind: crate::ir::VarKind::Const,
            },
            Statement::If {
                condition: Expression::binary(crate::ir::BinaryOp::In, var("#K"), var("obj")),
                then_body: vec![throw],
                else_body: vec![
                    store("#K", undef.clone()),
                    store("#f", undef.clone()),
                    store("#f", var("x")),
                    Statement::Return(Some(var("obj"))),
                ],
            },
        ];
        let out = restore_constructor_this(body);
        let text: Vec<String> = out.iter().map(|s| s.to_string()).collect();
        assert_eq!(text, vec!["const obj = this;", "obj.#f = x;"]);
    }

    #[test]
    fn a_derived_implicit_constructor_calls_super_first() {
        let forward = Expression::Call {
            callee: Box::new(Expression::member(var("HermesBuiltin"), "applyArguments")),
            arguments: vec![
                Expression::Value(Value::This),
                Expression::Value(Value::NewTarget),
            ],
        };
        let body = vec![
            Statement::Let {
                name: "r".into(),
                value: forward,
                kind: crate::ir::VarKind::Const,
            },
            Statement::Assign {
                target: AssignTarget::Member {
                    object: var("r"),
                    property: "tag".into(),
                },
                value: var("x"),
            },
            Statement::Return(Some(var("r"))),
        ];
        let out = restore_constructor_this(body);
        assert!(
            matches!(&out[0], Statement::Expr(Expression::Call { callee, arguments })
            if matches!(callee.as_ref(), Expression::Value(Value::Super))
                && matches!(arguments.as_slice(), [Expression::Spread(a)]
                    if matches!(a.as_ref(), Expression::Value(Value::Arguments))))
        );
        let text: Vec<String> = out[1..].iter().map(|s| s.to_string()).collect();
        assert_eq!(text, vec!["const r = this;", "r.tag = x;"]);
    }

    #[test]
    fn a_constructor_returning_another_object_is_left_alone() {
        let body = vec![
            Statement::Let {
                name: "obj".into(),
                value: create(),
                kind: crate::ir::VarKind::Const,
            },
            Statement::Return(Some(var("other"))),
        ];
        assert_eq!(restore_constructor_this(body.clone()), body);
    }
}
