use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, Statement, Value, Visitor,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------
// Phase 1, 1-hop props resolution (same block, sequential)
// ---------------------------------------------------------------------------

// The object literals in scope for a props argument, and the names a jsx call
// took its props from. A definition that fed exactly one call is removed once
// the call carries the literal, otherwise it stays behind as a dead object
// that later reads as a stray `{ ... };` statement.
pub(super) struct PropScope {
    map: BTreeMap<String, Expression>,
    consumed: RefCell<Vec<String>>,
}

impl PropScope {
    fn get(&self, name: &str) -> Option<&Expression> {
        let found = self.map.get(name)?;
        self.consumed.borrow_mut().push(name.to_string());
        Some(found)
    }
}

pub(super) fn resolve_prop_object_vars(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut uses: BTreeMap<String, usize> = BTreeMap::new();
    count_variable_uses(&stmts, &mut uses);

    let mut out = Vec::with_capacity(stmts.len());
    let mut scope = PropScope {
        map: BTreeMap::new(),
        consumed: RefCell::new(Vec::new()),
    };
    let mut def_index: BTreeMap<String, usize> = BTreeMap::new();

    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                let value = maybe_subst_call(value, &scope);
                invalidate_written(&mut scope.map, &value);
                if matches!(value, Expression::Object { .. }) {
                    scope.map.insert(name.clone(), value.clone());
                    def_index.insert(name.clone(), out.len());
                } else {
                    scope.map.remove(&name);
                    def_index.remove(&name);
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(name)),
                value,
            } => {
                let value = maybe_subst_call(value, &scope);
                invalidate_written(&mut scope.map, &value);
                if matches!(value, Expression::Object { .. }) {
                    scope.map.insert(name.clone(), value.clone());
                    def_index.insert(name.clone(), out.len());
                } else {
                    scope.map.remove(&name);
                    def_index.remove(&name);
                }
                out.push(Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable(name)),
                    value,
                });
            }
            other => {
                let mut s = other;
                // Nested blocks get their own scope (fresh map via recursion).
                map_nested_bodies_mut(&mut s, resolve_prop_object_vars);
                // Also rewrite jsx calls at this level if Assign/Expr not caught above.
                rewrite_stmt_calls(&mut s, &scope);
                // Any write reaching a recorded name makes its literal stale, so
                // substituting it into a later `jsx(Tag, p)` would silently drop
                // the write. Nested bodies count: they are resolved in a fresh
                // scope, so this scope never sees what they assign.
                if !scope.map.is_empty() {
                    let written = written_names(|c| c.visit_statement(&s));
                    scope.map.retain(|name, _| !written.contains(name));
                }
                out.push(s);
            }
        }
    }

    let mut drop: Vec<usize> = scope
        .consumed
        .borrow()
        .iter()
        .filter(|name| uses.get(*name).copied().unwrap_or(0) == 1)
        .filter_map(|name| def_index.get(name).copied())
        .collect();
    drop.sort_unstable();
    drop.dedup();
    for idx in drop.into_iter().rev() {
        out.remove(idx);
    }
    out
}

// Root variable of a member/index path (`p` for `p`, `p.a`, `p[k].b`), the
// object a write through that path actually mutates.
fn base_variable(expr: &Expression) -> Option<&String> {
    match expr {
        Expression::Value(Value::Binding(Binding::Variable(name))) => Some(name),
        Expression::Member { object, .. } => base_variable(object),
        _ => None,
    }
}

// Every name written anywhere under the visited node: rebinds (`p = …`,
// `let p = …`), property writes (`p.a = …`, `p[k] = …`), destructuring targets,
// loop and catch bindings, and assignment expressions — all of them reached
// through nested bodies too.
fn written_names(visit: impl FnOnce(&mut WrittenNames)) -> BTreeSet<String> {
    let mut collector = WrittenNames(BTreeSet::new());
    visit(&mut collector);
    collector.0
}

// Drop every recorded literal the expression writes to, so a stale one is never
// substituted into a later jsx call (`let x = (p.a = 1)` mutates `p`).
fn invalidate_written(objects: &mut BTreeMap<String, Expression>, value: &Expression) {
    if objects.is_empty() {
        return;
    }
    let written = written_names(|c| c.visit_expression(value));
    if !written.is_empty() {
        objects.retain(|name, _| !written.contains(name));
    }
}

struct WrittenNames(BTreeSet<String>);

impl WrittenNames {
    fn record(&mut self, expr: &Expression) {
        if let Some(name) = base_variable(expr) {
            self.0.insert(name.clone());
        }
    }
}

impl<'a> Visitor<'a> for WrittenNames {
    // `let`, loop heads, catch parameters and classes.
    fn visit_binding_def(&mut self, name: &'a str) {
        self.0.insert(name.to_string());
    }

    // Statement and expression-position writes both arrive here.
    fn visit_assign_target(&mut self, target: &'a AssignTarget) {
        match target {
            AssignTarget::Binding(Binding::Variable(name)) => {
                self.0.insert(name.clone());
            }
            AssignTarget::Member { object, .. } | AssignTarget::Index { object, .. } => {
                self.record(object)
            }
            // Destructuring targets nest, `walk_assign_target` reaches them.
            _ => {}
        }
        self.walk_assign_target(target);
    }
}

fn count_variable_uses(stmts: &[Statement], uses: &mut BTreeMap<String, usize>) {
    struct C<'a>(&'a mut BTreeMap<String, usize>);
    impl<'a, 'b> Visitor<'b> for C<'a> {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(name))) = e {
                *self.0.entry(name.clone()).or_insert(0) += 1;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(uses);
    for s in stmts {
        c.visit_statement(s);
    }
}

fn maybe_subst_call(mut expr: Expression, objects: &PropScope) -> Expression {
    subst_jsx_props_in_expr(&mut expr, objects);
    expr
}

fn rewrite_stmt_calls(stmt: &mut Statement, objects: &PropScope) {
    match stmt {
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            subst_jsx_props_in_expr(e, objects);
        }
        Statement::Assign { value, .. } | Statement::Let { value, .. } => {
            subst_jsx_props_in_expr(value, objects);
        }
        _ => {}
    }
}

fn subst_jsx_props_in_expr(expr: &mut Expression, objects: &PropScope) {
    subst_jsx_props_in_expr_inner(expr, objects, &mut std::collections::HashSet::new());
}

// `expanding`: the props objects being substituted up the current path. A
// props object can hold an element built from itself (`props.children =
// createElement(View, props)` before `createElement(View, props)`), and
// substituting it again inside its own copy never ends: the reference is
// kept at that point and the expansion stops there. Reported in issue #24,
// guard contributed by luca-regne.
fn subst_jsx_props_in_expr_inner(
    expr: &mut Expression,
    objects: &PropScope,
    expanding: &mut std::collections::HashSet<String>,
) {
    match expr {
        Expression::Call { callee, arguments }
            if super::is_jsx_call(callee) && arguments.len() >= 2 =>
        {
            let mut expanded = None;
            if let Expression::Value(Value::Binding(Binding::Variable(name))) = &arguments[1] {
                if let Some(obj) = objects.get(name) {
                    if expanding.insert(name.clone()) {
                        expanded = Some(name.clone());
                        arguments[1] = obj.clone();
                    }
                }
            }
            subst_jsx_props_in_expr_inner(&mut arguments[1], objects, expanding);
            if let Some(name) = expanded {
                expanding.remove(&name);
            }
            // Children args of a classic createElement may nest jsx calls.
            for (index, a) in arguments.iter_mut().enumerate() {
                if index != 1 {
                    subst_jsx_props_in_expr_inner(a, objects, expanding);
                }
            }
            subst_jsx_props_in_expr_inner(callee, objects, expanding);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            subst_jsx_props_in_expr_inner(callee, objects, expanding);
            for a in arguments {
                subst_jsx_props_in_expr_inner(a, objects, expanding);
            }
        }
        Expression::Binary { left, right, .. } => {
            subst_jsx_props_in_expr_inner(left, objects, expanding);
            subst_jsx_props_in_expr_inner(right, objects, expanding);
        }
        Expression::Unary { operand, .. }
        | Expression::Spread(operand)
        | Expression::Await(operand)
        | Expression::Yield { value: operand, .. } => {
            subst_jsx_props_in_expr_inner(operand, objects, expanding)
        }
        Expression::Member { object, .. } => {
            subst_jsx_props_in_expr_inner(object, objects, expanding)
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            subst_jsx_props_in_expr_inner(condition, objects, expanding);
            subst_jsx_props_in_expr_inner(then_expr, objects, expanding);
            subst_jsx_props_in_expr_inner(else_expr, objects, expanding);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                subst_jsx_props_in_expr_inner(e, objects, expanding);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                subst_jsx_props_in_expr_inner(&mut p.value, objects, expanding);
            }
        }
        Expression::Assignment { target, value } => {
            crate::ir::for_each_target_expression_mut(target, &mut |e| {
                subst_jsx_props_in_expr_inner(e, objects, expanding)
            });
            subst_jsx_props_in_expr_inner(value, objects, expanding);
        }
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            for (_, v) in attributes {
                subst_jsx_props_in_expr_inner(v, objects, expanding);
            }
            for c in children {
                subst_jsx_props_in_expr_inner(c, objects, expanding);
            }
        }
        _ => {}
    }
}
