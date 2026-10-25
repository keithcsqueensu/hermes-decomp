use super::{is_builtin_call, resolve_array_elements};
use crate::ir::{Expression, Statement, Value};

// `HermesBuiltin.apply(f, args, thisArg)` -> `f(...)` / `f.apply(thisArg, args)`,
// anywhere in each statement's expressions (the apply is often nested as a call
// argument, e.g. `print(apply(f, args, undefined))`).
pub(super) fn reconstruct_apply(stmts: &mut [Statement]) {
    for idx in 0..stmts.len() {
        // Resolve the args array against the statements BEFORE this one.
        let (before, rest) = stmts.split_at_mut(idx);
        let stmt = &mut rest[0];
        rewrite_applies_in_stmt(stmt, before);
    }
}

fn rewrite_applies_in_stmt(stmt: &mut Statement, before: &[Statement]) {
    match stmt {
        Statement::Assign { value, .. }
        | Statement::Expr(value)
        | Statement::Return(Some(value))
        | Statement::Throw(value) => rewrite_applies_in_expr(value, before),
        _ => {}
    }
}

fn rewrite_applies_in_expr(expr: &mut Expression, before: &[Statement]) {
    // Bottom-up: rewrite children first.
    match expr {
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            rewrite_applies_in_expr(callee, before);
            for a in arguments.iter_mut() {
                rewrite_applies_in_expr(a, before);
            }
        }
        Expression::Member { object, .. } => rewrite_applies_in_expr(object, before),
        Expression::Binary { left, right, .. } => {
            rewrite_applies_in_expr(left, before);
            rewrite_applies_in_expr(right, before);
        }
        Expression::Unary { operand, .. } => rewrite_applies_in_expr(operand, before),
        Expression::Spread(inner) => rewrite_applies_in_expr(inner, before),
        _ => {}
    }
    if let Some(call) = apply_to_call(expr, before) {
        *expr = call;
    }
}

// If `value` is `HermesBuiltin.apply(f, args, thisArg)`, build the equivalent call.
fn apply_to_call(value: &Expression, before: &[Statement]) -> Option<Expression> {
    let args = match is_builtin_call(value, "apply") {
        Some(a) if a.len() >= 2 => a,
        _ => return None,
    };
    // Same `this`-slot shift as arraySpread. Try the unshifted layout first,
    // then the layout with a leading receiver.
    let layouts: [(usize, usize, usize); 2] = [(0, 1, 2), (1, 2, 3)];
    let (func, args_array, this_arg) = layouts.iter().find_map(|(f, a, t)| {
        let func = args.get(*f)?.clone();
        let args_array = args.get(*a)?;
        resolve_array_elements(args_array, before)?;
        let this_arg = args.get(*t).cloned();
        Some((func, args_array.clone(), this_arg))
    })?;

    // Resolve the args array to a literal: either inline, or a register defined
    // earlier as an array literal.
    let elements = resolve_array_elements(&args_array, before)?;

    let this_is_undefined = matches!(
        this_arg,
        None | Some(Expression::Value(Value::Constant(
            crate::ir::Constant::Undefined
        )))
    );

    // This runs before `strip_hermes_this` (W12), which takes argument 0 of
    // every call for its receiver: each call built here carries one. Without
    // it `f(1, 2, ...s)` printed as `f(2, ...s)`, and `o.m.apply(o, args)` as
    // `o.m.apply(args)`.
    if this_is_undefined {
        // f(...elements)
        let mut arguments = vec![Expression::Value(Value::Constant(
            crate::ir::Constant::Undefined,
        ))];
        arguments.extend(elements);
        return Some(Expression::Call {
            callee: Box::new(func),
            arguments,
        });
    }
    let this_arg = this_arg.unwrap();
    // `o.m` applied to `o` is the method call `o.m(...elements)`.
    if matches!(&func, Expression::Member { object, .. } if **object == this_arg) {
        let mut arguments = vec![this_arg];
        arguments.extend(elements);
        return Some(Expression::Call {
            callee: Box::new(func),
            arguments,
        });
    }
    // f.apply(thisArg, argsArray)
    Some(Expression::Call {
        callee: Box::new(Expression::member(func.clone(), "apply")),
        arguments: vec![func, this_arg, args_array],
    })
}

// Resolve an expression to array elements: an inline Array literal, or a
// register whose nearest preceding definition is an array literal.
