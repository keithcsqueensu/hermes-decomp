use super::*;
use crate::ir::{Constant, Expression, Value};

#[test]
fn nested_mutation_forces_let_not_const() {
    // Parent: tmp = 0;  Child free-mutates tmp → parent must use let.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("tmp".into())),
        value: Expression::constant(Constant::Integer(0)),
    }];
    let child = vec![
        // first use is read (free), then write
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("tmp".into())),
            value: Expression::Binary {
                op: crate::ir::BinaryOp::Add,
                left: Box::new(Expression::Value(Value::Binding(Binding::Variable(
                    "tmp".into(),
                )))),
                right: Box::new(Expression::constant(Constant::Integer(1))),
            },
        },
    ];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(extra.get("tmp").copied().unwrap_or(0) >= 2);
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, name, .. } => {
            assert_eq!(name, "tmp");
            assert_eq!(*kind, VarKind::Let, "expected let when nested writes");
        }
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn no_nested_writes_keeps_const() {
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("x".into())),
        value: Expression::constant(Constant::Integer(1)),
    }];
    insert_declarations_with_extra_writes(&mut parent, &[], &BTreeMap::new());
    match &parent[0] {
        Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Const),
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn write_first_nested_reassign_forces_let() {
    // Parent: items = [];  Child (constructor/getter): items = [1] as first use.
    // Free-only counting used to miss this; parent must still be let.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array { elements: vec![] },
    }];
    let child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array {
            elements: vec![Some(Expression::constant(Constant::Integer(1)))],
        },
    }];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(
        extra.get("items").copied().unwrap_or(0) >= 1,
        "nested write-first must contribute extra writes, got {extra:?}"
    );
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, name, .. } => {
            assert_eq!(name, "items");
            assert_eq!(
                *kind,
                VarKind::Let,
                "expected let for cross-scope reassignment"
            );
        }
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn nested_local_let_does_not_force_parent_let() {
    // Child owns `items` via Let, pure shadowing, parent stays const.
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("items".into())),
        value: Expression::Array { elements: vec![] },
    }];
    let child = vec![Statement::Let {
        name: "items".into(),
        value: Expression::Array { elements: vec![] },
        kind: VarKind::Let,
    }];
    let extra = extra_writes_from_nested_bodies(&[&child]);
    assert!(
        !extra.contains_key("items"),
        "explicit nested Let must not leak writes, got {extra:?}"
    );
    insert_declarations_with_extra_writes(&mut parent, &[], &extra);
    match &parent[0] {
        Statement::Let { kind, .. } => assert_eq!(*kind, VarKind::Const),
        other => panic!("expected Let, got {other:?}"),
    }
}

#[test]
fn nested_env_slot_write_stays_assign() {
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1".into())),
        value: Expression::Value(Value::Binding(Binding::Variable("payload".into()))),
    }];
    let mut outer = HashSet::new();
    outer.insert("closure_1".into());
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &outer, true);
    match &child[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(n)),
            ..
        } => assert_eq!(n, "closure_1"),
        other => panic!("expected Assign, got {other:?}"),
    }
}

#[test]
fn switch_cases_declare_the_same_name_separately() {
    let mut body = vec![Statement::Switch {
        discriminant: Expression::Value(Value::Binding(Binding::Variable("kind".into()))),
        cases: vec![
            (
                Expression::constant(crate::ir::Constant::String("voice".into())),
                vec![Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("userId".into())),
                    value: Expression::constant(crate::ir::Constant::Integer(1)),
                }],
            ),
            (
                Expression::constant(crate::ir::Constant::String("unified".into())),
                vec![Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("userId".into())),
                    value: Expression::constant(crate::ir::Constant::Integer(2)),
                }],
            ),
        ],
        default: None,
    }];
    insert_declarations(&mut body, &[]);
    let Statement::Switch { cases, .. } = &body[0] else {
        panic!()
    };
    assert!(matches!(&cases[0].1[0], Statement::Let { name, .. } if name == "userId"));
    assert!(matches!(&cases[1].1[0], Statement::Let { name, .. } if name == "userId"));
}

#[test]
fn own_env_slot_is_declared_when_it_is_not_an_ancestor_capture() {
    // A `closure_N` written here and not listed as an ancestor slot is this
    // function's own binding. Leaving it as a bare assignment makes it an
    // implicit global. Ancestor captures stay assignments; see the test above.
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1_2".into())),
        value: Expression::Array { elements: vec![] },
    }];
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &HashSet::new(), true);
    match &child[0] {
        Statement::Let { name, kind, .. } => {
            assert_eq!(name, "closure_1_2");
            assert_eq!(*kind, crate::ir::VarKind::Let);
        }
        other => panic!("own env slot must be declared, got {other:?}"),
    }
}

#[test]
fn factory_env_slot_still_declared() {
    let mut parent = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("closure_1".into())),
        value: Expression::Array { elements: vec![] },
    }];
    insert_declarations_with_extra_writes(&mut parent, &[], &BTreeMap::new());
    match &parent[0] {
        Statement::Let { name, .. } => assert_eq!(name, "closure_1"),
        other => panic!("factory must still declare env slot, got {other:?}"),
    }
}

#[test]
fn generic_local_declared_when_outer_is_only_env_slots() {
    // Ancestor locals like `obj` must not suppress the child's own `let obj`.
    let mut child = vec![Statement::Assign {
        target: AssignTarget::Binding(Binding::Variable("obj".into())),
        value: Expression::Object { properties: vec![] },
    }];
    let mut outer = HashSet::new();
    outer.insert("closure_1".into());
    insert_declarations_with_outer(&mut child, &[], &BTreeMap::new(), &outer, true);
    match &child[0] {
        Statement::Let { name, .. } => assert_eq!(name, "obj"),
        other => panic!("expected Let obj, got {other:?}"),
    }
}

// K4.4a: an own slot the body only reads (its closures write it) passes for a
// capture; its stamp says otherwise and it is declared at the top.
#[test]
fn an_own_slot_read_first_is_declared_by_its_stamp() {
    let read = |n: &str| Expression::Value(Value::Binding(Binding::Variable(n.into())));
    let body = || vec![Statement::Expr(read("pending"))];
    let none = HashSet::new();
    let mut unstamped = body();
    insert_declarations_with_slots(
        &mut unstamped,
        &[],
        &BTreeMap::new(),
        &none,
        &none,
        &none,
        &Default::default(),
    );
    assert!(matches!(unstamped[0], Statement::Expr(_)), "{unstamped:?}");

    let own_used = HashSet::from(["pending".to_string()]);
    let mut stamped = body();
    insert_declarations_with_slots(
        &mut stamped,
        &[],
        &BTreeMap::new(),
        &none,
        &none,
        &own_used,
        &Default::default(),
    );
    match &stamped[0] {
        Statement::Let { name, kind, .. } => {
            assert_eq!(name, "pending");
            assert_eq!(*kind, VarKind::Let);
        }
        other => panic!("expected a hoisted let, got {other:?}"),
    }
    assert!(matches!(stamped[1], Statement::Expr(_)), "{stamped:?}");
}

// K4.4a: an own slot spelled like an ancestor's slot is the function's own
// variable; leaving it undeclared wrote into the ancestor's.
#[test]
fn an_own_slot_named_like_an_ancestor_slot_is_declared() {
    let body = || {
        vec![Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable("l".into())),
            value: Expression::constant(Constant::Integer(1)),
        }]
    };
    let outer = HashSet::from(["l".to_string()]);
    let none = HashSet::new();
    let mut by_name = body();
    insert_declarations_with_slots(
        &mut by_name,
        &[],
        &BTreeMap::new(),
        &outer,
        &none,
        &none,
        &Default::default(),
    );
    assert!(
        matches!(by_name[0], Statement::Assign { .. }),
        "{by_name:?}"
    );

    let mut by_stamp = body();
    let own_used = outer.clone();
    insert_declarations_with_slots(
        &mut by_stamp,
        &[],
        &BTreeMap::new(),
        &outer,
        &none,
        &own_used,
        &Default::default(),
    );
    assert!(
        matches!(&by_stamp[0], Statement::Let { name, .. } if name == "l"),
        "{by_stamp:?}"
    );
}

// K4.4a: an own slot first written inside a block is declared at the top when
// a closure created outside the block reads it, and at the write otherwise.
#[test]
fn an_own_slot_written_in_a_block_is_hoisted_for_a_closure_outside() {
    let body = || {
        vec![
            Statement::Expr(Expression::Function {
                id: crate::ir::FunctionId(9),
                name: None,
                is_arrow: true,
                is_async: false,
                is_generator: false,
            }),
            Statement::If {
                condition: Expression::constant(Constant::Bool(true)),
                then_body: vec![Statement::Assign {
                    target: AssignTarget::Binding(Binding::Variable("seen".into())),
                    value: Expression::constant(Constant::Integer(1)),
                }],
                else_body: vec![],
            },
        ]
    };
    let none = HashSet::new();
    let own_used = HashSet::from(["seen".to_string()]);
    let no_reads = std::collections::HashMap::new();

    let mut local = body();
    insert_declarations_with_slots(
        &mut local,
        &[],
        &BTreeMap::new(),
        &none,
        &none,
        &own_used,
        &no_reads,
    );
    assert!(matches!(local[0], Statement::Expr(_)), "{local:?}");

    let read_by = std::collections::HashMap::from([(9u32, own_used.clone())]);
    let mut stmts = body();
    insert_declarations_with_slots(
        &mut stmts,
        &[],
        &BTreeMap::new(),
        &none,
        &none,
        &own_used,
        &read_by,
    );
    assert!(
        matches!(&stmts[0], Statement::Let { name, .. } if name == "seen"),
        "{stmts:?}"
    );
    match &stmts[2] {
        Statement::If { then_body, .. } => {
            assert!(
                matches!(then_body[0], Statement::Assign { .. }),
                "{then_body:?}"
            )
        }
        other => panic!("expected the if, got {other:?}"),
    }
}
