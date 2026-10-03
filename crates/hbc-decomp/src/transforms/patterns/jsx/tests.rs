use super::*;
use crate::ir::{AssignTarget, VarKind};

#[test]
fn test_classic_jsx_element() {
    let mut expr = Expression::call(
        Expression::member(
            Expression::Value(Value::Binding(Binding::Variable("React".to_string()))),
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
fn folds_assigned_props_and_keeps_the_earlier_child() {
    let tag = Expression::Value(Value::Binding(Binding::Variable("View".into())));
    let child = Expression::Value(Value::Binding(Binding::Variable("tmp3".into())));
    let stmts = vec![
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("obj".into())),
            value: Expression::Object { properties: vec![] },
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
                property: "children".into(),
            },
            value: child,
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
                property: "children".into(),
            },
            value: Expression::call(
                Expression::Value(Value::Binding(Binding::Variable("jsx".into()))),
                vec![tag, Expression::Object { properties: vec![] }],
            ),
        },
        Statement::Return(Some(Expression::call(
            Expression::Value(Value::Binding(Binding::Variable("jsx".into()))),
            vec![
                Expression::Value(Value::Binding(Binding::Variable("Provider".into()))),
                Expression::Value(Value::Binding(Binding::Variable("obj".into()))),
            ],
        ))),
    ];
    let out = reconstruct_jsx(stmts);
    // The prop assigns fold into the literal, the overwrite is dropped, and
    // the object definition goes with the call that absorbed it.
    assert_eq!(out.len(), 1, "prop assigns folded, definition consumed");
    match &out[0] {
        Statement::Return(Some(Expression::JSXElement { children, .. })) => {
            assert!(!children.is_empty(), "the earlier child is kept");
        }
        other => panic!("expected jsx return, got {other:?}"),
    }
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
            Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
            vec![
                Expression::constant(Constant::String("div".into())),
                Expression::Value(Value::Binding(Binding::Variable("p".into()))),
            ],
        )),
    ];
    let out = reconstruct_jsx(stmts);
    assert_eq!(out.len(), 1, "the props definition is consumed by the call");
    match &out[0] {
        Statement::Expr(Expression::JSXElement {
            tag, attributes, ..
        }) => {
            assert_eq!(tag, "div");
            assert!(attributes.iter().any(|(k, _)| k == "id"));
        }
        other => panic!("expected jsx expr, got {other:?}"),
    }
}

#[test]
fn test_modern_key_third_arg() {
    let mut expr = Expression::call(
        Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
        vec![
            Expression::Value(Value::Binding(Binding::Variable("Foo".into()))),
            Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("title".into()),
                    value: Expression::constant(Constant::String("x".into())),
                }],
            },
            Expression::Value(Value::Binding(Binding::Variable("k".into()))),
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
        Expression::Value(Value::Binding(Binding::Variable("jsxs".into()))),
        vec![
            Expression::Value(Value::Binding(Binding::Variable("_Fragment".into()))),
            Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("children".into()),
                    value: Expression::Array {
                        elements: vec![Some(Expression::Value(Value::Binding(Binding::Variable(
                            "a".into(),
                        ))))],
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
        Expression::member(
            Expression::Value(Value::Binding(Binding::Variable("jsxProd".into()))),
            "jsxs",
        ),
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
                                Some(Expression::Value(Value::Binding(Binding::Variable(
                                    "a".into(),
                                )))),
                                Some(Expression::Value(Value::Binding(Binding::Variable(
                                    "b".into(),
                                )))),
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

// Issue #24: a props object holding an element built from itself. Expanding
// it inside its own copy never ended and overflowed the stack; the expansion
// stops at the cycle and keeps the reference there.
#[test]
fn props_substitution_stops_at_a_direct_cycle() {
    let create_element = |props: Expression| {
        Expression::call(
            Expression::member(
                Expression::Value(Value::Binding(Binding::Variable("React".into()))),
                "createElement",
            ),
            vec![
                Expression::Value(Value::Binding(Binding::Variable("View".into()))),
                props,
            ],
        )
    };
    let props_ref = || Expression::Value(Value::Binding(Binding::Variable("props".into())));
    let stmts = vec![
        Statement::Let {
            name: "props".into(),
            value: Expression::Object { properties: vec![] },
            kind: VarKind::Let,
        },
        Statement::Assign {
            target: AssignTarget::Member {
                object: props_ref(),
                property: "children".into(),
            },
            value: create_element(props_ref()),
        },
        Statement::Return(Some(create_element(props_ref()))),
    ];
    let out = reconstruct_jsx(stmts);
    let text = format!("{out:?}");
    assert!(
        text.contains("props"),
        "the recursive edge stays a reference: {text}"
    );
}

// `p = { a: jsx(Inner, p) }` (object folding leaves this shape, the inner `p`
// being the pre-assignment value) used to expand into itself forever.
#[test]
fn self_referential_props_binding_terminates() {
    let jsx_call = |tag: &str, props: Expression| {
        Expression::call(
            Expression::member(Expression::Value(Value::Binding(Binding::Variable("_r".into()))), "jsx"),
            vec![Expression::Value(Value::Binding(Binding::Variable(tag.into()))), props],
        )
    };
    let stmts = vec![
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("p".into())),
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("accessory".into()),
                    value: jsx_call("Inner", Expression::Value(Value::Binding(Binding::Variable("p".into())))),
                }],
            },
        },
        Statement::Return(Some(jsx_call(
            "Outer",
            Expression::Value(Value::Binding(Binding::Variable("p".into()))),
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
// must not be substituted as it was (that would drop `p.b`). The fold phase
// absorbs a straight-line write into the literal first, so either way the
// element must never carry `a` without `b`.
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
                object: Expression::Value(Value::Binding(Binding::Variable("p".into()))),
                property: "b".into(),
            },
            value: Expression::constant(Constant::String("y".into())),
        },
        Statement::Expr(Expression::call(
            Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
            vec![
                Expression::constant(Constant::String("div".into())),
                Expression::Value(Value::Binding(Binding::Variable("p".into()))),
            ],
        )),
    ];
    let out = reconstruct_jsx(stmts);
    match out.last() {
        Some(Statement::Expr(Expression::JSXElement { attributes, .. })) => {
            let has = |key: &str| attributes.iter().any(|(k, _)| k == key);
            assert!(
                !has("a") || has("b"),
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
            Expression::Value(Value::Binding(Binding::Variable("_jsx".into()))),
            vec![
                Expression::constant(Constant::String("div".into())),
                Expression::Value(Value::Binding(Binding::Variable("p".into()))),
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
            object: Expression::Value(Value::Binding(Binding::Variable("p".into()))),
            property: "b".into(),
        }),
        // if (c) { p = "y"; }
        write(AssignTarget::Binding(Binding::Variable("p".into()))),
        // if (c) { p.a.deep = "y"; }
        write(AssignTarget::Member {
            object: Expression::member(Expression::Value(Value::Binding(Binding::Variable("p".into()))), "a"),
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
                condition: Expression::Value(Value::Binding(Binding::Variable("c".into()))),
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
            condition: Expression::Value(Value::Binding(Binding::Variable("c".into()))),
            then_body: vec![Statement::Break(None)],
            else_body: vec![],
        },
        jsx_p(),
    ]);
    // `p` feeds only the call, so its definition is consumed and dropped.
    assert_eq!(out.len(), 2, "the props definition is consumed by the call");
    match &out[1] {
        Statement::Expr(Expression::JSXElement { attributes, .. }) => {
            assert!(attributes.iter().any(|(k, _)| k == "a"));
        }
        other => panic!("expected jsx expr, got {other:?}"),
    }
}
