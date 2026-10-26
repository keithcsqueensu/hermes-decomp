// The spread protocol once the temporaries are named (W16), where the early
// pass (`transform_spread_rest`, on registers) could not see it: the array is
// built from member reads and other loads before the spread, so the register
// scan stopped at the first of them, and the argument array reached
// `HermesBuiltin.apply` through a variable.
//
//   items = [logger.name, "info", msg];
//   HermesBuiltin.arraySpread(items, rest, 3);
//   HermesBuiltin.apply(nativeLog, items, undefined);
//   -> nativeLog(logger.name, "info", msg, ...rest)
//
// Calls here carry no receiver argument any more (`strip_hermes_this` ran).

use super::is_builtin_call;
use crate::ir::{AssignTarget, Binding, Constant, Expression, Statement, Value, Visitor};

// Marks an array definition whose elements went into a call; removed after.
const FOLDED: &str = "\u{0}spread-folded";

pub fn fold_named_spreads(stmts: &mut Vec<Statement>) {
    fold_list(stmts);
}

fn fold_list(stmts: &mut Vec<Statement>) {
    for stmt in stmts.iter_mut() {
        crate::ir::map_nested_bodies_mut(stmt, |mut body| {
            fold_list(&mut body);
            body
        });
    }
    let mut i = 0;
    while i < stmts.len() {
        if let Some((name, mut elements)) = array_def(&stmts[i]) {
            let mut j = i + 1;
            let mut folded = false;
            while j < stmts.len() {
                if let Some(src) = spread_into(&stmts[j], &name, elements.len()) {
                    elements.push(Expression::Spread(Box::new(src)));
                    stmts.remove(j);
                    folded = true;
                    continue;
                }
                if !steps_over(&stmts[j], &name) {
                    break;
                }
                j += 1;
            }
            if folded {
                set_array(&mut stmts[i], elements);
            }
        }
        i += 1;
    }
    fold_applies(stmts);
    stmts.retain(|s| !matches!(s, Statement::Comment(c) if c == FOLDED));
}

// `X = [e…]` (or `let X = [e…]`) with no holes: (X, elements).
fn array_def(stmt: &Statement) -> Option<(String, Vec<Expression>)> {
    let (name, value) = match stmt {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(v)),
            value,
        } => (&v.name, value),
        Statement::Let { name, value, .. } => (&name.name, value),
        _ => return None,
    };
    let Expression::Array { elements } = value else {
        return None;
    };
    let elements: Option<Vec<Expression>> = elements.iter().cloned().collect();
    Some((name.clone(), elements?))
}

fn set_array(stmt: &mut Statement, elements: Vec<Expression>) {
    let value = match stmt {
        Statement::Assign { value, .. } | Statement::Let { value, .. } => value,
        _ => return,
    };
    *value = Expression::Array {
        elements: elements.into_iter().map(Some).collect(),
    };
}

// `HermesBuiltin.arraySpread(X, src, n)` with `n` the elements X holds, as a
// statement of its own: Some(src).
fn spread_into(stmt: &Statement, name: &str, len: usize) -> Option<Expression> {
    let Statement::Expr(call) = stmt else {
        return None;
    };
    let args = is_builtin_call(call, "arraySpread")?;
    let [target, src, Expression::Value(Value::Constant(Constant::Integer(n)))] = args.as_slice()
    else {
        return None;
    };
    (is_var(target, name) && usize::try_from(*n).ok() == Some(len)).then(|| src.clone())
}

fn is_var(e: &Expression, name: &str) -> bool {
    matches!(e, Expression::Value(Value::Binding(Binding::Variable(v))) if v.name == name)
}

// A statement the spread may be moved above: a load into a variable other than
// X that neither mentions X nor calls anything (`nativeLog2 = nativeLog`).
fn steps_over(stmt: &Statement, name: &str) -> bool {
    let value = match stmt {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(v)),
            value,
        } if v.name != name => value,
        Statement::Let { name: v, value, .. } if v.name != name => value,
        _ => return false,
    };
    struct Check<'n> {
        name: &'n str,
        ok: bool,
    }
    impl<'a> Visitor<'a> for Check<'_> {
        fn visit_expression(&mut self, e: &'a Expression) {
            match e {
                Expression::Value(Value::Binding(Binding::Variable(v))) if v.name == self.name => {
                    self.ok = false
                }
                Expression::Call { .. }
                | Expression::New { .. }
                | Expression::Assignment { .. }
                | Expression::Await(_)
                | Expression::Yield { .. } => self.ok = false,
                _ => {}
            }
            self.walk_expression(e);
        }
    }
    let mut c = Check { name, ok: true };
    c.visit_expression(value);
    c.ok
}

// `HermesBuiltin.apply(f, X, thisArg)` with X an array this list defines:
// `f(...elements)` when X is read nowhere else, else `f(...X)`; `f.apply(t, X)`
// for a receiver, and `o.m(...)` when the receiver is the method's object.
fn fold_applies(stmts: &mut [Statement]) {
    use crate::ir::MutVisitor;
    let mut reads: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    {
        struct R<'m>(&'m mut std::collections::HashMap<String, usize>);
        impl<'a> Visitor<'a> for R<'_> {
            fn visit_expression(&mut self, e: &'a Expression) {
                if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                    *self.0.entry(v.name.clone()).or_default() += 1;
                }
                self.walk_expression(e);
            }
        }
        let mut r = R(&mut reads);
        for s in stmts.iter() {
            r.visit_statement(s);
        }
    }
    struct Apply<'b> {
        before: &'b [Statement],
        reads: &'b std::collections::HashMap<String, usize>,
        inlined: Vec<String>,
    }
    impl MutVisitor for Apply<'_> {
        fn visit_expression(&mut self, e: &mut Expression) {
            self.walk_expression(e);
            let reads = self.reads;
            if let Some(name) = rewrite_apply(e, self.before, &|n: &str| {
                reads.get(n).copied().unwrap_or(0)
            }) {
                self.inlined.push(name);
            }
        }
    }
    let mut inlined = Vec::new();
    for idx in 0..stmts.len() {
        let (before, rest) = stmts.split_at_mut(idx);
        let mut a = Apply {
            before,
            reads: &reads,
            inlined: Vec::new(),
        };
        a.visit_statement(&mut rest[0]);
        inlined.extend(a.inlined);
    }
    // An array whose elements went into the call has no reader left; kept,
    // its elements (a spread's iteration included) would run twice.
    for name in inlined {
        if let Some(def) = stmts
            .iter_mut()
            .rev()
            .find(|s| array_def(s).is_some_and(|(n, _)| n == name))
        {
            *def = Statement::Comment(FOLDED.to_string());
        }
    }
}

// The rewrite of `e`, when it is such an apply; Some(X) when X's elements went
// into the call.
fn rewrite_apply(
    e: &mut Expression,
    before: &[Statement],
    uses: &dyn Fn(&str) -> usize,
) -> Option<String> {
    let args = is_builtin_call(e, "apply")?;
    let [func, array, this_arg] = args.as_slice() else {
        return None;
    };
    let Expression::Value(Value::Binding(Binding::Variable(v))) = array else {
        return None;
    };
    let elements = before
        .iter()
        .rev()
        .find_map(|s| array_def(s).filter(|(n, _)| *n == v.name).map(|(_, e)| e))?;
    // This call is X's only reader: the elements go inline.
    let inline = uses(&v.name) == 1;
    let spread = if inline {
        elements
    } else {
        vec![Expression::Spread(Box::new(array.clone()))]
    };
    let undefined = matches!(
        this_arg,
        Expression::Value(Value::Constant(Constant::Undefined))
    );
    // A plain call, or a method applied to its own object: `f(…)` / `o.m(…)`.
    let direct =
        undefined || matches!(func, Expression::Member { object, .. } if **object == *this_arg);
    *e = if direct {
        Expression::Call {
            callee: Box::new(func.clone()),
            arguments: spread,
        }
    } else {
        Expression::Call {
            callee: Box::new(Expression::member(func.clone(), "apply")),
            arguments: vec![this_arg.clone(), array.clone()],
        }
    };
    (inline && direct).then(|| v.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(n: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(n.into())))
    }

    fn builtin(name: &str, args: Vec<Expression>) -> Expression {
        Expression::Call {
            callee: Box::new(Expression::member(v("HermesBuiltin"), name)),
            arguments: args,
        }
    }

    #[test]
    fn a_spread_call_built_in_named_variables_folds() {
        let mut body = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable("items".into())),
                value: Expression::Array {
                    elements: vec![
                        Some(Expression::member(v("logger"), "name")),
                        Some(v("msg")),
                    ],
                },
            },
            Statement::Expr(builtin(
                "arraySpread",
                vec![
                    v("items"),
                    v("rest"),
                    Expression::constant(Constant::Integer(2)),
                ],
            )),
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable("f2".into())),
                value: v("f"),
            },
            Statement::Expr(builtin(
                "apply",
                vec![
                    v("f"),
                    v("items"),
                    Expression::constant(Constant::Undefined),
                ],
            )),
        ];
        fold_named_spreads(&mut body);
        let call = body
            .iter()
            .find_map(|s| match s {
                Statement::Expr(Expression::Call { callee, arguments }) => {
                    Some((callee.as_ref().clone(), arguments.clone()))
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(call.0, v("f"));
        assert_eq!(
            call.1,
            vec![
                Expression::member(v("logger"), "name"),
                v("msg"),
                Expression::Spread(Box::new(v("rest"))),
            ]
        );
    }
}
