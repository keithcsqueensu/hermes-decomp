// Reconstructs JSXElement nodes from React.createElement and _jsx / _jsxs calls.
//
// Two-phase approach:
// 1. Resolve 1-hop object props: `const p = { a: 1 }; jsx(Tag, p)` → use `{ a: 1 }`
// 2. Match factory calls and lower to Expression::JSXElement

use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Constant, Expression, MutVisitor, ObjectProperty,
    PropertyKey, Statement, Value, Visitor,
};
use std::collections::{BTreeMap, BTreeSet};

/// Run JSX reconstruction over a statement list (idempotent).
pub fn reconstruct_jsx(mut stmts: Vec<Statement>) -> Vec<Statement> {
    stmts = resolve_prop_object_vars(stmts);
    JSXReconstructor::new().visit_statement_list(&mut stmts);
    stmts
}

// ---------------------------------------------------------------------------
// Phase 1, 1-hop props resolution (same block, sequential)
// ---------------------------------------------------------------------------

fn resolve_prop_object_vars(stmts: Vec<Statement>) -> Vec<Statement> {
    let mut out = Vec::with_capacity(stmts.len());
    let mut objects: BTreeMap<String, Expression> = BTreeMap::new();

    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, kind } => {
                let value = maybe_subst_call(value, &objects);
                invalidate_written(&mut objects, &value);
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.clone(), value.clone());
                } else {
                    objects.remove(&name);
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign {
                target: AssignTarget::Variable(name),
                value,
            } => {
                let value = maybe_subst_call(value, &objects);
                invalidate_written(&mut objects, &value);
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.clone(), value.clone());
                } else {
                    objects.remove(&name);
                }
                out.push(Statement::Assign {
                    target: AssignTarget::Variable(name),
                    value,
                });
            }
            other => {
                let mut s = other;
                // Nested blocks get their own scope (fresh map via recursion).
                map_nested_bodies_mut(&mut s, resolve_prop_object_vars);
                // Also rewrite jsx calls at this level if Assign/Expr not caught above.
                rewrite_stmt_calls(&mut s, &objects);
                // Any write reaching a recorded name makes its literal stale, so
                // substituting it into a later `jsx(Tag, p)` would silently drop
                // the write. Nested bodies count: they are resolved in a fresh
                // scope, so this scope never sees what they assign.
                if !objects.is_empty() {
                    let written = written_names(|c| c.visit_statement(&s));
                    objects.retain(|name, _| !written.contains(name));
                }
                out.push(s);
            }
        }
    }
    out
}

fn maybe_subst_call(mut expr: Expression, objects: &BTreeMap<String, Expression>) -> Expression {
    subst_jsx_props_in_expr(&mut expr, objects);
    expr
}

fn rewrite_stmt_calls(stmt: &mut Statement, objects: &BTreeMap<String, Expression>) {
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

// Root variable of a member/index path (`p` for `p`, `p.a`, `p[k].b`), the
// object a write through that path actually mutates.
fn base_variable(expr: &Expression) -> Option<&String> {
    match expr {
        Expression::Value(Value::Variable(name)) => Some(name),
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
    fn visit_statement(&mut self, stmt: &'a Statement) {
        match stmt {
            Statement::Let { name, .. }
            | Statement::ForOf { variable: name, .. }
            | Statement::ForIn { variable: name, .. }
            | Statement::Class { name, .. } => {
                self.0.insert(name.clone());
            }
            Statement::TryCatch {
                catch_param: Some(name),
                ..
            } => {
                self.0.insert(name.clone());
            }
            _ => {}
        }
        self.walk_statement(stmt);
    }

    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Assignment { target, .. } = expr {
            self.record(target);
        }
        self.walk_expression(expr);
    }

    fn visit_assign_target(&mut self, target: &'a AssignTarget) {
        match target {
            AssignTarget::Variable(name) => {
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

fn subst_jsx_props_in_expr(expr: &mut Expression, objects: &BTreeMap<String, Expression>) {
    subst_jsx_props_guarded(expr, objects, &mut BTreeSet::new());
}

// `active` holds the props variables currently being expanded on this path down
// the tree. A recorded literal can mention its own name — object folding turns
// `p = {..., a: jsx(Tag, p)}` into exactly that, where the inner `p` is the
// pre-assignment value — and expanding such a binding into itself never
// terminates. Substituting a name at most once per path keeps that finite.
fn subst_jsx_props_guarded(
    expr: &mut Expression,
    objects: &BTreeMap<String, Expression>,
    active: &mut BTreeSet<String>,
) {
    let subst_jsx_props_in_expr = |e: &mut Expression, active: &mut BTreeSet<String>| {
        subst_jsx_props_guarded(e, objects, active)
    };
    match expr {
        Expression::Call { callee, arguments } if is_jsx_call(callee) && arguments.len() >= 2 => {
            let mut expanded: Option<(String, Expression)> = None;
            if let Expression::Value(Value::Variable(name)) = &arguments[1] {
                if !active.contains(name) {
                    if let Some(obj) = objects.get(name) {
                        expanded = Some((name.clone(), obj.clone()));
                    }
                }
            }
            let substituted = expanded.is_some();
            if let Some((name, obj)) = expanded {
                arguments[1] = obj;
                active.insert(name.clone());
                subst_jsx_props_in_expr(&mut arguments[1], active);
                active.remove(&name);
            }
            // Recurse into children args (classic createElement children may nest jsx)
            for (i, a) in arguments.iter_mut().enumerate() {
                // arguments[1] was already expanded above, under the guard.
                if i == 1 && substituted {
                    continue;
                }
                subst_jsx_props_in_expr(a, active);
            }
            subst_jsx_props_in_expr(callee, active);
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            subst_jsx_props_in_expr(callee, active);
            for a in arguments {
                subst_jsx_props_in_expr(a, active);
            }
        }
        Expression::Binary { left, right, .. } => {
            subst_jsx_props_in_expr(left, active);
            subst_jsx_props_in_expr(right, active);
        }
        Expression::Unary { operand, .. }
        | Expression::Spread(operand)
        | Expression::Await(operand)
        | Expression::Yield { value: operand, .. } => subst_jsx_props_in_expr(operand, active),
        Expression::Member { object, .. } => subst_jsx_props_in_expr(object, active),
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            subst_jsx_props_in_expr(condition, active);
            subst_jsx_props_in_expr(then_expr, active);
            subst_jsx_props_in_expr(else_expr, active);
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                subst_jsx_props_in_expr(e, active);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                subst_jsx_props_in_expr(&mut p.value, active);
            }
        }
        Expression::Assignment { target, value } => {
            subst_jsx_props_in_expr(target, active);
            subst_jsx_props_in_expr(value, active);
        }
        Expression::JSXElement {
            attributes,
            children,
            ..
        } => {
            for (_, v) in attributes {
                subst_jsx_props_in_expr(v, active);
            }
            for c in children {
                subst_jsx_props_in_expr(c, active);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Phase 2, factory match → JSXElement
// ---------------------------------------------------------------------------

pub struct JSXReconstructor;

impl JSXReconstructor {
    pub fn new() -> Self {
        Self
    }
}

impl MutVisitor for JSXReconstructor {
    fn visit_expression(&mut self, expr: &mut Expression) {
        self.walk_expression(expr);
        if let Expression::Call { callee, arguments } = expr {
            if is_jsx_call(callee) && !arguments.is_empty() {
                if let Some(jsx_node) = build_jsx_element(callee, arguments) {
                    *expr = jsx_node;
                }
            }
        }
    }
}

fn jsx_factory_name(callee: &Expression) -> Option<&str> {
    let raw = match callee {
        Expression::Member {
            property: PropertyKey::Ident(p) | PropertyKey::String(p),
            ..
        } => p.as_str(),
        Expression::Value(Value::Variable(n)) => n.as_str(),
        _ => return None,
    };
    // Strip leading underscores and common runtime prefixes.
    let stripped = raw.strip_prefix('_').unwrap_or(raw);
    Some(stripped)
}

fn is_jsx_call(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some(
            "createElement"
                | "jsx"
                | "jsxs"
                | "jsxDEV"
                | "jsxsDEV"
                | "jsxDev"
                | "jsxsDev"
        )
    )
}

fn is_modern_factory(callee: &Expression) -> bool {
    matches!(
        jsx_factory_name(callee),
        Some("jsx" | "jsxs" | "jsxDEV" | "jsxsDEV" | "jsxDev" | "jsxsDev")
    )
}

fn build_jsx_element(callee: &Expression, arguments: &[Expression]) -> Option<Expression> {
    // JSX tags must be identifiers or member paths (or string HTML tags).
    // Calls like `importDefault(36)` are valid createElement first-args but NOT
    // valid JSX tag forms, leave those as jsx()/createElement() calls.
    let tag_name = match &arguments[0] {
        Expression::Value(Value::Constant(Constant::String(s))) => s.clone(),
        Expression::Value(Value::Variable(v)) => v.clone(),
        Expression::Member { object, property, .. } => {
            if let (Expression::Value(Value::Variable(obj_name)), PropertyKey::Ident(prop_name)) =
                (object.as_ref(), property)
            {
                format!("{obj_name}.{prop_name}")
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let tag_name = if tag_name == "Fragment"
        || tag_name == "_Fragment"
        || tag_name.ends_with(".Fragment")
        || tag_name == "React.Fragment"
    {
        String::new()
    } else {
        tag_name
    };

    let is_modern = is_modern_factory(callee);
    let mut jsx_attributes = Vec::new();
    let mut jsx_children = Vec::new();

    if is_modern && arguments.len() >= 3 {
        if !matches!(
            arguments[2],
            Expression::Value(Value::Constant(Constant::Undefined | Constant::Null))
        ) {
            jsx_attributes.push(("key".to_string(), arguments[2].clone()));
        }
    }

    if is_modern {
        if arguments.len() >= 2 {
            match &arguments[1] {
                Expression::Object { properties } => {
                    push_props(properties, &mut jsx_attributes, &mut jsx_children, true);
                }
                Expression::Value(Value::Constant(Constant::Null | Constant::Undefined)) => {}
                other => jsx_attributes.push(("...".to_string(), other.clone())),
            }
        }
    } else {
        if arguments.len() >= 2 {
            match &arguments[1] {
                Expression::Object { properties } => {
                    push_props(properties, &mut jsx_attributes, &mut jsx_children, false);
                }
                Expression::Value(Value::Constant(Constant::Null | Constant::Undefined)) => {}
                Expression::Spread(_) => {
                    jsx_attributes.push(("...".to_string(), arguments[1].clone()));
                }
                other => {
                    jsx_attributes.push((
                        "...".to_string(),
                        Expression::Spread(Box::new(other.clone())),
                    ));
                }
            }
        }
        for child in arguments.iter().skip(2) {
            jsx_children.push(child.clone());
        }
    }

    Some(Expression::JSXElement {
        tag: tag_name,
        attributes: jsx_attributes,
        children: jsx_children,
    })
}

fn push_props(
    properties: &[ObjectProperty],
    attrs: &mut Vec<(String, Expression)>,
    children: &mut Vec<Expression>,
    modern: bool,
) {
    for prop in properties {
        match &prop.key {
            PropertyKey::Ident(k) | PropertyKey::String(k) => {
                if modern && k == "children" {
                    if let Expression::Array { elements } = &prop.value {
                        children.extend(elements.iter().flatten().cloned());
                    } else {
                        children.push(prop.value.clone());
                    }
                } else {
                    attrs.push((k.clone(), prop.value.clone()));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::VarKind;

    #[test]
    fn test_classic_jsx_element() {
        let mut expr = Expression::call(
            Expression::member(
                Expression::Value(Value::Variable("React".to_string())),
                "createElement",
            ),
            vec![
                Expression::constant(Constant::String("div".to_string())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("id".to_string()),
                        value: Expression::constant(Constant::String("main".to_string())),
                    }],
                },
                Expression::constant(Constant::String("Text".to_string())),
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        assert!(matches!(expr, Expression::JSXElement { .. }));
    }

    #[test]
    fn resolves_props_variable_one_hop() {
        let stmts = vec![
            Statement::Let {
                name: "p".into(),
                value: Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("id".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    }],
                },
                kind: VarKind::Let,
            },
            Statement::Expr(Expression::call(
                Expression::Value(Value::Variable("_jsx".into())),
                vec![
                    Expression::constant(Constant::String("div".into())),
                    Expression::Value(Value::Variable("p".into())),
                ],
            )),
        ];
        let out = reconstruct_jsx(stmts);
        match &out[1] {
            Statement::Expr(Expression::JSXElement { tag, attributes, .. }) => {
                assert_eq!(tag, "div");
                assert!(attributes.iter().any(|(k, _)| k == "id"));
            }
            other => panic!("expected jsx expr, got {other:?}"),
        }
    }

    // `p = { a: jsx(Inner, p) }` (object folding leaves this shape, the inner `p`
    // being the pre-assignment value) used to expand into itself forever.
    #[test]
    fn self_referential_props_binding_terminates() {
        let jsx_call = |tag: &str, props: Expression| {
            Expression::call(
                Expression::member(Expression::Value(Value::Variable("_r".into())), "jsx"),
                vec![Expression::Value(Value::Variable(tag.into())), props],
            )
        };
        let stmts = vec![
            Statement::Assign {
                target: AssignTarget::Variable("p".into()),
                value: Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("accessory".into()),
                        value: jsx_call("Inner", Expression::Value(Value::Variable("p".into()))),
                    }],
                },
            },
            Statement::Return(Some(jsx_call(
                "Outer",
                Expression::Value(Value::Variable("p".into())),
            ))),
        ];
        let out = reconstruct_jsx(stmts);
        match &out[1] {
            Statement::Return(Some(Expression::JSXElement {
                tag, attributes, ..
            })) => {
                assert_eq!(tag, "Outer");
                assert!(attributes.iter().any(|(k, _)| k == "accessory"));
            }
            other => panic!("expected jsx return, got {other:?}"),
        }
    }

    // A property write after the literal makes the recorded object stale, so it
    // must not be substituted (that would drop `p.b`).
    #[test]
    fn property_write_invalidates_recorded_props() {
        let stmts = vec![
            Statement::Let {
                name: "p".into(),
                value: Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("a".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    }],
                },
                kind: VarKind::Let,
            },
            Statement::Assign {
                target: AssignTarget::Member {
                    object: Expression::Value(Value::Variable("p".into())),
                    property: "b".into(),
                },
                value: Expression::constant(Constant::String("y".into())),
            },
            Statement::Expr(Expression::call(
                Expression::Value(Value::Variable("_jsx".into())),
                vec![
                    Expression::constant(Constant::String("div".into())),
                    Expression::Value(Value::Variable("p".into())),
                ],
            )),
        ];
        let out = reconstruct_jsx(stmts);
        match &out[2] {
            Statement::Expr(Expression::JSXElement { attributes, .. }) => {
                assert!(
                    attributes.iter().all(|(k, _)| k != "a"),
                    "stale literal was inlined, dropping p.b: {attributes:?}"
                );
            }
            other => panic!("expected jsx expr, got {other:?}"),
        }
    }

    // A write inside a nested block is invisible to this scope's map (nested
    // bodies resolve in a fresh scope), so it has to invalidate conservatively.
    #[test]
    fn nested_block_write_invalidates_recorded_props() {
        let props_literal = || Expression::Object {
            properties: vec![ObjectProperty {
                key: PropertyKey::Ident("a".into()),
                value: Expression::constant(Constant::String("x".into())),
            }],
        };
        let jsx_p = || {
            Statement::Expr(Expression::call(
                Expression::Value(Value::Variable("_jsx".into())),
                vec![
                    Expression::constant(Constant::String("div".into())),
                    Expression::Value(Value::Variable("p".into())),
                ],
            ))
        };
        let write = |target: AssignTarget| Statement::Assign {
            target,
            value: Expression::constant(Constant::String("y".into())),
        };
        let nested_writes = [
            // if (c) { p.b = "y"; }
            write(AssignTarget::Member {
                object: Expression::Value(Value::Variable("p".into())),
                property: "b".into(),
            }),
            // if (c) { p = "y"; }
            write(AssignTarget::Variable("p".into())),
            // if (c) { p.a.deep = "y"; }
            write(AssignTarget::Member {
                object: Expression::member(Expression::Value(Value::Variable("p".into())), "a"),
                property: "deep".into(),
            }),
        ];

        for inner in nested_writes {
            let stmts = vec![
                Statement::Let {
                    name: "p".into(),
                    value: props_literal(),
                    kind: VarKind::Let,
                },
                Statement::If {
                    condition: Expression::Value(Value::Variable("c".into())),
                    then_body: vec![inner.clone()],
                    else_body: vec![],
                },
                jsx_p(),
            ];
            let out = reconstruct_jsx(stmts);
            match &out[2] {
                Statement::Expr(Expression::JSXElement { attributes, .. }) => {
                    assert!(
                        attributes.iter().all(|(k, _)| k != "a"),
                        "stale literal inlined despite nested write {inner:?}: {attributes:?}"
                    );
                }
                other => panic!("expected jsx expr, got {other:?}"),
            }
        }

        // Without any write the one-hop resolution still applies.
        let out = reconstruct_jsx(vec![
            Statement::Let {
                name: "p".into(),
                value: props_literal(),
                kind: VarKind::Let,
            },
            Statement::If {
                condition: Expression::Value(Value::Variable("c".into())),
                then_body: vec![Statement::Break(None)],
                else_body: vec![],
            },
            jsx_p(),
        ]);
        match &out[2] {
            Statement::Expr(Expression::JSXElement { attributes, .. }) => {
                assert!(attributes.iter().any(|(k, _)| k == "a"));
            }
            other => panic!("expected jsx expr, got {other:?}"),
        }
    }

    #[test]
    fn test_modern_key_third_arg() {
        let mut expr = Expression::call(
            Expression::Value(Value::Variable("_jsx".into())),
            vec![
                Expression::Value(Value::Variable("Foo".into())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("title".into()),
                        value: Expression::constant(Constant::String("x".into())),
                    }],
                },
                Expression::Value(Value::Variable("k".into())),
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { attributes, .. } => {
                assert!(attributes.iter().any(|(k, _)| k == "key"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_fragment_empty_tag() {
        let mut expr = Expression::call(
            Expression::Value(Value::Variable("jsxs".into())),
            vec![
                Expression::Value(Value::Variable("_Fragment".into())),
                Expression::Object {
                    properties: vec![ObjectProperty {
                        key: PropertyKey::Ident("children".into()),
                        value: Expression::Array {
                            elements: vec![Some(Expression::Value(Value::Variable("a".into())))],
                        },
                    }],
                },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement { tag, children, .. } => {
                assert_eq!(tag, "");
                assert_eq!(children.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn test_modern_jsx_member_factory() {
        let mut expr = Expression::call(
            Expression::member(Expression::Value(Value::Variable("jsxProd".into())), "jsxs"),
            vec![
                Expression::constant(Constant::String("ul".into())),
                Expression::Object {
                    properties: vec![
                        ObjectProperty {
                            key: PropertyKey::Ident("className".into()),
                            value: Expression::constant(Constant::String("x".into())),
                        },
                        ObjectProperty {
                            key: PropertyKey::Ident("children".into()),
                            value: Expression::Array {
                                elements: vec![
                                    Some(Expression::Value(Value::Variable("a".into()))),
                                    Some(Expression::Value(Value::Variable("b".into()))),
                                ],
                            },
                        },
                    ],
                },
            ],
        );
        JSXReconstructor::new().visit_expression(&mut expr);
        match expr {
            Expression::JSXElement {
                tag,
                attributes,
                children,
            } => {
                assert_eq!(tag, "ul");
                assert_eq!(attributes.len(), 1);
                assert_eq!(children.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }
}
