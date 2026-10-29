// Strip meaningless Hermes `this` arguments from calls.
//
// In Hermes bytecode, ALL function calls pass `this` as arguments[0].
// For non-method calls, this is meaningless (register reuse artifact).
// This pass strips it at the IR level so that dead assignment elimination
// can catch variables that were only used as `this` arguments.

use crate::ir::{AssignTarget, Expression, PropertyKey, Statement, Value};

// `HermesInternal.concat(a, b, c, ...)` (emitted for template literals / string
// concatenation) takes real operands, NOT a `this` receiver. Its first argument
// must never be stripped.
fn is_hermes_internal_concat(callee: &Expression) -> bool {
    if let Expression::Member {
        object, property, ..
    } = callee
    {
        let is_concat =
            matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "concat");
        if is_concat {
            return mentions_hermes_internal(object);
        }
    }
    false
}

// `Object.create`, `<...>.Object.create` or `Object.create`, regardless of how
// the global `Object` is rendered (bare variable, member on globalThis, etc.).
fn is_object_create(callee: &Expression) -> bool {
    if let Expression::Member {
        object, property, ..
    } = callee
    {
        let is_create =
            matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "create");
        if is_create {
            return match object.as_ref() {
                Expression::Value(Value::Binding(crate::ir::Binding::Variable(v))) => v == "Object",
                Expression::Member { property, .. } => {
                    matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "Object")
                }
                _ => false,
            };
        }
    }
    false
}

// `[...arguments].slice(N)`, which `spread_rest` builds from
// `HermesBuiltin.copyRestArgs(N)` with no receiver argument: N is the rest
// parameter's index, and stripping it as a receiver made every rest parameter
// start at 0.
fn is_rest_args_slice(callee: &Expression) -> bool {
    let Expression::Member {
        object, property, ..
    } = callee
    else {
        return false;
    };
    if !matches!(property, PropertyKey::Ident(p) if p == "slice") {
        return false;
    }
    let Expression::Array { elements } = object.as_ref() else {
        return false;
    };
    matches!(
        elements.as_slice(),
        [Some(Expression::Spread(inner))]
            if matches!(inner.as_ref(), Expression::Value(Value::Binding(crate::ir::Binding::Variable(v))) if v == "arguments")
    )
}

fn mentions_hermes_internal(e: &Expression) -> bool {
    match e {
        Expression::Value(Value::Binding(crate::ir::Binding::Variable(v))) => v == "HermesInternal",
        Expression::Member {
            object, property, ..
        } => {
            matches!(property, PropertyKey::Ident(p) | PropertyKey::String(p) if p == "HermesInternal")
                || mentions_hermes_internal(object)
        }
        _ => false,
    }
}

// Strip meaningless `this` from Call expressions at the IR level.
// - For method calls (callee is Member, object == args[0]): strip args[0]
// - For variable callees (not Member): strip args[0] if non-trivial
// - For explicit .call (callee is Member with prop "call"): strip args[0] (function self-ref)
// - Keep args[0] only when it's needed (already undefined/global -> keep as-is for consistency)
pub fn strip_hermes_this(stmts: &mut [Statement]) {
    keep_method_receivers(stmts);
    strip_list(stmts);
}

fn strip_list(stmts: &mut [Statement]) {
    for stmt in stmts.iter_mut() {
        strip_this_in_stmt(stmt);
    }
}

// A call through a variable that holds a method read off an object, passing
// that object as its receiver: `push = items.push; push(items, x)`. The read
// is kept apart from the call when an argument is evaluated in between (or the
// read is used twice), and its receiver is real, not the `undefined` a plain
// call passes. Stripped like one, it printed `push(x)`, a call with no `this`.
// It becomes `push.call(push, items, x)`, which the `.call` rule below brings
// to `push.call(items, x)`. The receiver may be a copy of the object, assigned
// once (`tmp = items; forEach = items.forEach; forEach(tmp, cb)`: the read was
// hoisted above a branch whose arms both call it).
fn keep_method_receivers(stmts: &mut [Statement]) {
    use crate::ir::{Binding, MutVisitor, Visitor};
    use std::collections::HashMap;

    // Each binding with the number of its assignments and the value of the
    // first: the object of a member read, or the binding a copy reads.
    struct Defs(HashMap<Binding, (usize, Option<Expression>, Option<Binding>)>);
    impl<'a> Visitor<'a> for Defs {
        fn visit_statement(&mut self, s: &'a Statement) {
            match s {
                Statement::Assign {
                    target: AssignTarget::Binding(b),
                    value,
                } => self.note(b.clone(), value),
                Statement::Let { name, value, .. } => {
                    self.note(Binding::Variable(name.clone()), value)
                }
                _ => {}
            }
            self.walk_statement(s);
        }
    }
    impl Defs {
        fn note(&mut self, b: Binding, value: &Expression) {
            let object = match value {
                Expression::Member { object, .. } => Some(object.as_ref().clone()),
                _ => None,
            };
            let copy = match value {
                Expression::Value(Value::Binding(c)) => Some(c.clone()),
                _ => None,
            };
            let e = self.0.entry(b).or_insert((0, object, copy));
            e.0 += 1;
        }
    }
    let mut defs = Defs(HashMap::new());
    for s in stmts.iter() {
        defs.visit_statement(s);
    }
    let copies: HashMap<Binding, Binding> = defs
        .0
        .iter()
        .filter_map(|(b, (n, _, copy))| {
            (*n == 1)
                .then_some(())
                .and(copy.clone().map(|c| (b.clone(), c)))
        })
        .collect();
    let methods: HashMap<Binding, Expression> = defs
        .0
        .into_iter()
        .filter_map(|(b, (n, object, _))| (n == 1).then_some(()).and(object.map(|o| (b, o))))
        .collect();
    if methods.is_empty() {
        return;
    }
    // The same value: equal, or bindings that copy one binding.
    fn same(copies: &HashMap<Binding, Binding>, a: &Expression, b: &Expression) -> bool {
        fn root<'c>(
            copies: &'c HashMap<Binding, Binding>,
            e: &'c Expression,
        ) -> Option<&'c Binding> {
            let Expression::Value(Value::Binding(b)) = e else {
                return None;
            };
            let mut b = b;
            for _ in 0..8 {
                match copies.get(b) {
                    Some(next) => b = next,
                    None => break,
                }
            }
            Some(b)
        }
        a == b || root(copies, a).is_some_and(|r| root(copies, b) == Some(r))
    }
    struct Keep<'m>(
        &'m HashMap<Binding, Expression>,
        &'m HashMap<Binding, Binding>,
    );
    impl MutVisitor for Keep<'_> {
        fn visit_expression(&mut self, e: &mut Expression) {
            self.walk_expression(e);
            let Expression::Call { callee, arguments } = e else {
                return;
            };
            let Expression::Value(Value::Binding(b)) = callee.as_ref() else {
                return;
            };
            if arguments
                .first()
                .is_some_and(|first| self.0.get(b).is_some_and(|o| same(self.1, o, first)))
            {
                let function = callee.as_ref().clone();
                **callee = Expression::member(function.clone(), "call");
                arguments.insert(0, function);
            }
        }
    }
    let mut keep = Keep(&methods, &copies);
    for s in stmts.iter_mut() {
        keep.visit_statement(s);
    }
}

fn strip_this_in_stmt(stmt: &mut Statement) {
    match stmt {
        Statement::Assign { target, value } => {
            strip_this_in_target(target);
            strip_this_in_expr(value);
        }
        Statement::Let { value, .. } => strip_this_in_expr(value),
        Statement::Expr(e) => strip_this_in_expr(e),
        Statement::Return(Some(e)) | Statement::Throw(e) => strip_this_in_expr(e),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            strip_this_in_expr(condition);
            strip_list(then_body);
            strip_list(else_body);
        }
        Statement::While { condition, body } | Statement::DoWhile { condition, body } => {
            strip_this_in_expr(condition);
            strip_list(body);
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(s) = init {
                strip_this_in_stmt(s);
            }
            if let Some(e) = condition {
                strip_this_in_expr(e);
            }
            if let Some(s) = update {
                strip_this_in_stmt(s);
            }
            strip_list(body);
        }
        Statement::ForIn { object, body, .. } => {
            strip_this_in_expr(object);
            strip_list(body);
        }
        Statement::ForOf { iterable, body, .. } => {
            strip_this_in_expr(iterable);
            strip_list(body);
        }
        Statement::Block(inner) => strip_list(inner),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            strip_list(try_body);
            strip_list(catch_body);
            strip_list(finally_body);
        }
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            strip_this_in_expr(discriminant);
            for (e, body) in cases.iter_mut() {
                strip_this_in_expr(e);
                strip_list(body);
            }
            if let Some(d) = default {
                strip_list(d);
            }
        }
        _ => {}
    }
}

fn strip_this_in_target(target: &mut AssignTarget) {
    match target {
        AssignTarget::Member { object, .. } => strip_this_in_expr(object),
        AssignTarget::Index { object, key } => {
            strip_this_in_expr(object);
            strip_this_in_expr(key);
        }
        _ => {}
    }
}

fn strip_this_in_expr(expr: &mut Expression) {
    match expr {
        Expression::Call { callee, arguments } => {
            // First recurse into sub-expressions
            strip_this_in_expr(callee);
            for arg in arguments.iter_mut() {
                strip_this_in_expr(arg);
            }
            // Now strip `this` (arguments[0]) if appropriate.
            // In Hermes bytecode, ALL function calls pass `this` as arguments[0].
            if arguments.is_empty() {
                return;
            }
            // Builtins whose first argument is a real operand, not a receiver.
            if is_hermes_internal_concat(callee) {
                return;
            }
            // `Object.create(parent)` synthesized from the NewObjectWithParent
            // opcode carries a single genuine argument and no ABI `this`
            // receiver. A real source `Object.create(proto)` compiles to a Call
            // with two arguments (receiver + proto), so only the single-arg form
            // is the synthetic one whose argument must be preserved.
            if arguments.len() == 1 && is_object_create(callee) {
                return;
            }
            if arguments.len() == 1 && is_rest_args_slice(callee) {
                return;
            }
            // A reconstructed spread call (`f(...a)`) has a real first argument,
            // never a Hermes `this` receiver (which is a plain value).
            if matches!(arguments.first(), Some(Expression::Spread(_))) {
                return;
            }
            let first = &arguments[0];
            let should_strip = if let Expression::Member {
                object, property, ..
            } = callee.as_ref()
            {
                if matches!(property, PropertyKey::Ident(p) if p == "call") {
                    // Explicit .call() from source: strip the function self-reference (first arg),
                    // keeping the user-supplied receiver as the new first arg.
                    true
                } else {
                    // Member callee (method call): strip if object matches first arg (receiver IS the object)
                    // For non-matching (e.g., globalThis.func(garbage_this, ...)), also strip.
                    let is_method_call = **object == *first;
                    if is_method_call {
                        true // Standard method call: obj.method(obj, args) -> obj.method(args)
                    } else {
                        // Member on global or unrelated receiver -- always strip (garbage this)
                        true
                    }
                }
            } else {
                // Variable callee or trivial this (undefined/global) -- always strip
                true
            };
            if should_strip {
                arguments.remove(0);
            }
        }
        Expression::Binary { left, right, .. } => {
            strip_this_in_expr(left);
            strip_this_in_expr(right);
        }
        Expression::Unary { operand, .. } => strip_this_in_expr(operand),
        Expression::New { callee, arguments } => {
            strip_this_in_expr(callee);
            for a in arguments.iter_mut() {
                strip_this_in_expr(a);
            }
        }
        Expression::Member { object, .. } => strip_this_in_expr(object),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            strip_this_in_expr(condition);
            strip_this_in_expr(then_expr);
            strip_this_in_expr(else_expr);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                strip_this_in_expr(e);
            }
        }
        Expression::Object { properties } => {
            for p in properties.iter_mut() {
                strip_this_in_expr(&mut p.value);
            }
        }
        Expression::Assignment { target, value } => {
            crate::ir::for_each_target_expression_mut(target, &mut strip_this_in_expr);
            strip_this_in_expr(value);
        }
        Expression::Spread(inner) => strip_this_in_expr(inner),
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions.iter_mut() {
                strip_this_in_expr(e);
            }
        }
        Expression::Yield { value, .. } => strip_this_in_expr(value),
        Expression::Await(inner) => strip_this_in_expr(inner),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Binding;

    fn var(n: &str) -> Expression {
        Expression::Value(Value::Binding(Binding::Variable(n.into())))
    }

    fn assign(n: &str, value: Expression) -> Statement {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n.into())),
            value,
        }
    }

    fn call(callee: Expression, arguments: Vec<Expression>) -> Statement {
        Statement::Expr(Expression::Call {
            callee: Box::new(callee),
            arguments,
        })
    }

    // (callee, arguments) of the call statement `s`. `Display` reads the first
    // argument as the receiver still, so the stripped shape is checked as is.
    fn parts(s: &Statement) -> (Expression, Vec<Expression>) {
        match s {
            Statement::Expr(Expression::Call { callee, arguments }) => {
                (callee.as_ref().clone(), arguments.clone())
            }
            _ => panic!("not a call: {s}"),
        }
    }

    #[test]
    fn a_method_read_apart_from_its_call_keeps_its_receiver() {
        let mut body = vec![
            assign("push", Expression::member(var("items"), "push")),
            call(var("push"), vec![var("items"), var("x")]),
        ];
        strip_hermes_this(&mut body);
        let (callee, args) = parts(&body[1]);
        assert_eq!(callee, Expression::member(var("push"), "call"));
        assert_eq!(args, vec![var("items"), var("x")]);
    }

    #[test]
    fn a_receiver_copied_from_the_object_is_kept() {
        let mut body = vec![
            assign("tmp", var("items")),
            assign("forEach", Expression::member(var("items"), "forEach")),
            call(var("forEach"), vec![var("tmp"), var("cb")]),
        ];
        strip_hermes_this(&mut body);
        let (callee, args) = parts(&body[2]);
        assert_eq!(callee, Expression::member(var("forEach"), "call"));
        assert_eq!(args, vec![var("tmp"), var("cb")]);
    }

    #[test]
    fn a_plain_call_still_loses_its_undefined_receiver() {
        let undef = Expression::Value(Value::Constant(crate::ir::Constant::Undefined));
        let mut body = vec![
            assign("f", Expression::member(var("lib"), "f")),
            call(var("f"), vec![undef, var("x")]),
            call(var("g"), vec![var("items"), var("x")]),
        ];
        strip_hermes_this(&mut body);
        assert_eq!(parts(&body[1]), (var("f"), vec![var("x")]));
        assert_eq!(parts(&body[2]), (var("g"), vec![var("x")]));
    }
}
