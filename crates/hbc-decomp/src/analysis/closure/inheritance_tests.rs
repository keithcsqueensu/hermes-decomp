//! Cross-function env-slot inheritance tests (parent / grandparent / aliases).

use std::collections::BTreeMap;

use crate::ir::{AssignTarget, Expression, FunctionId, Statement, Value, VarKind};

use super::context::ClosureContext;
use super::info::encode_level_slot;
use super::resolve_closures;

fn var(s: &str) -> Expression {
    Expression::Value(Value::Binding(crate::ir::Binding::Variable(s.into())))
}

fn func(id: u32) -> Expression {
    Expression::Function {
        id: FunctionId(id),
        name: None,
        is_arrow: true,
        is_async: false,
        is_generator: false,
    }
}

fn store_env(slot: u32, level: u32, value: Expression) -> Statement {
    Statement::Assign {
        target: AssignTarget::Binding(crate::ir::Binding::ClosureVar { level, slot }),
        value,
    }
}

fn load_env(level: u32, slot: u32) -> Expression {
    Expression::Value(Value::Binding(crate::ir::Binding::ClosureVar {
        level,
        slot,
    }))
}

#[test]
fn child_inherits_parent_require_slot() {
    // Parent stores require into env[0] and creates child function 2.
    let parent = vec![
        store_env(0, 0, var("require")),
        Statement::Let {
            name: "inner".into(),
            value: func(2),
            kind: VarKind::Const,
        },
    ];
    let child = vec![Statement::Return(Some(load_env(1, 0)))];

    let mut all = BTreeMap::new();
    all.insert(1u32, parent);
    all.insert(2u32, child);

    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);

    let info = ctx.get_closure_info_for(2);
    let key = encode_level_slot(1, 0);
    assert!(
        info.slots.contains_key(&key),
        "parent slot 0 should be visible at level 1: {:?}",
        info.slots.keys().collect::<Vec<_>>()
    );
    assert_eq!(info.get_slot_name(key), "require");

    let resolved = resolve_closures(all.get(&2).unwrap().clone(), &info);
    match &resolved[0] {
        Statement::Return(Some(Expression::Value(Value::Binding(
            crate::ir::Binding::Variable(n),
        )))) => {
            assert_eq!(n, "require");
        }
        other => panic!("expected return require, got {other:?}"),
    }
}

#[test]
fn grandchild_inherits_level_two_from_grandparent() {
    // F1 creates F2, F2 creates F3. F1 stores "HTTP" in slot 3.
    // F3 loads ClosureVar level=2 slot=3 → HTTP.
    let f1 = vec![
        store_env(3, 0, var("HTTP")),
        Statement::Let {
            name: "mid".into(),
            value: func(2),
            kind: VarKind::Const,
        },
    ];
    let f2 = vec![Statement::Let {
        name: "leaf".into(),
        value: func(3),
        kind: VarKind::Const,
    }];
    let f3 = vec![Statement::Return(Some(load_env(2, 3)))];

    let mut all = BTreeMap::new();
    all.insert(1, f1);
    all.insert(2, f2);
    all.insert(3, f3);

    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);

    assert_eq!(ctx.parent_function.get(&2), Some(&1));
    assert_eq!(ctx.parent_function.get(&3), Some(&2));

    let info = ctx.get_closure_info_for(3);
    let key = encode_level_slot(2, 3);
    assert_eq!(info.get_slot_name(key), "HTTP");

    let resolved = resolve_closures(all.get(&3).unwrap().clone(), &info);
    match &resolved[0] {
        Statement::Return(Some(Expression::Value(Value::Binding(
            crate::ir::Binding::Variable(n),
        )))) => {
            assert_eq!(n, "HTTP");
        }
        other => panic!("expected return HTTP, got {other:?}"),
    }
}

#[test]
fn named_alias_followed_into_env_store() {
    // let require = arg1; env[1] = require  → slot records require (not arg1 after alias hop
    // through named_values; arg1 alone would become require only via metro roles).
    let parent = vec![
        Statement::Let {
            name: "require".into(),
            value: var("arg1"),
            kind: VarKind::Const,
        },
        store_env(1, 0, var("require")),
        Statement::Let {
            name: "child".into(),
            value: func(10),
            kind: VarKind::Const,
        },
    ];
    let child = vec![Statement::Expr(load_env(1, 1))];

    let mut all = BTreeMap::new();
    all.insert(5u32, parent);
    all.insert(10u32, child);

    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);
    // Without metro roles: named alias "require" is already the store value.
    let info = ctx.get_closure_info_for(10);
    assert_eq!(info.get_slot_name(encode_level_slot(1, 1)), "require");
}

#[test]
fn child_store_to_parent_env_is_deferred_onto_parent() {
    // Child writes ClosureVar level=1 slot=7 = "Endpoints".
    // Parent should receive the store so siblings can resolve it.
    let parent = vec![Statement::Let {
        name: "child".into(),
        value: func(20),
        kind: VarKind::Const,
    }];
    let child = vec![
        store_env(7, 1, var("Endpoints")),
        Statement::Return(Some(load_env(1, 7))),
    ];

    let mut all = BTreeMap::new();
    all.insert(19u32, parent);
    all.insert(20u32, child);

    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);

    // Parent map should hold slot 7.
    let parent_info = ctx.function_closures.get(&19).expect("parent slots");
    assert_eq!(
        parent_info.get_slot_name(7),
        "Endpoints",
        "deferred child store should land on parent: {:?}",
        parent_info.slots
    );

    let info = ctx.get_closure_info_for(20);
    assert_eq!(info.get_slot_name(encode_level_slot(1, 7)), "Endpoints");
}

#[test]
fn function_slot_enriched_with_binding_name() {
    // Parent: let helper = function(){} (id 30); env[2] = helper
    // Slot should become Function with name helper (via binding / function_names).
    let parent = vec![
        Statement::Let {
            name: "helper".into(),
            value: func(30),
            kind: VarKind::Const,
        },
        store_env(2, 0, var("helper")),
        Statement::Let {
            name: "c".into(),
            value: func(31),
            kind: VarKind::Const,
        },
    ];
    let child = vec![Statement::Return(Some(load_env(1, 2)))];

    let mut all = BTreeMap::new();
    all.insert(29u32, parent);
    all.insert(31u32, child);

    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);

    let info = ctx.get_closure_info_for(31);
    let name = info.get_slot_name(encode_level_slot(1, 2));
    // Either the variable alias "helper" or function name after enrich.
    assert!(
        name == "helper" || name.starts_with('f'),
        "expected helper-like name, got {name}"
    );
}

// The bake stamps the slot a name stands for, as the same absolute `SlotRef` in
// the owner's store and the child's capture, whatever level each reached it at.
#[test]
fn the_bake_stamps_owner_and_capture_with_one_slot() {
    use super::resolve_closures_recording;
    use crate::ir::{Binding, SlotRef};
    let parent = vec![
        store_env(0, 0, var("require")),
        Statement::Let {
            name: "inner".into(),
            value: func(2),
            kind: VarKind::Const,
        },
    ];
    let child = vec![Statement::Return(Some(load_env(1, 0)))];
    let mut all = BTreeMap::new();
    all.insert(1u32, parent);
    all.insert(2u32, child);
    let mut ctx = ClosureContext::new();
    ctx.reanalyze_all(&all);

    let slot = Some(SlotRef { owner: 1, slot: 0 });
    let (owner, _) = resolve_closures_recording(
        all[&1].clone(),
        &ctx.get_closure_info_for(1),
        Some((&ctx, 1)),
    );
    match &owner[0] {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(v)),
            ..
        } => assert_eq!(v.slot, slot, "{v:?}"),
        other => panic!("expected the store, got {other:?}"),
    }
    let (reader, _) = resolve_closures_recording(
        all[&2].clone(),
        &ctx.get_closure_info_for(2),
        Some((&ctx, 2)),
    );
    match &reader[0] {
        Statement::Return(Some(Expression::Value(Value::Binding(Binding::Variable(v))))) => {
            assert_eq!(v.slot, slot, "{v:?}")
        }
        other => panic!("expected return of the capture, got {other:?}"),
    }
}

// K4.4a: the names a body declares for its own slots come from the stamps; a
// name it also uses for an ancestor's slot is left out.
#[test]
fn own_slot_names_reads_the_stamps() {
    use super::stamps::own_slot_names;
    use crate::ir::{Binding, SlotRef, Var};
    let stamped = |name: &str, owner: u32, slot: u32| {
        Expression::Value(Value::Binding(Binding::Variable(Var {
            name: name.to_string(),
            slot: Some(SlotRef { owner, slot }),
        })))
    };
    let body = vec![
        Statement::Expr(stamped("pending", 5, 0)),
        Statement::Expr(stamped("both", 5, 1)),
        Statement::Expr(stamped("both", 4, 0)),
        Statement::Expr(stamped("captured", 4, 1)),
        Statement::Expr(var("plain")),
    ];
    let names = own_slot_names(5, &body);
    assert_eq!(
        names,
        std::collections::HashSet::from(["pending".to_string()])
    );
}

// K4.4b: a capture is a slot an ancestor owns; a slot of a function that is not
// an ancestor (its statements were inlined here) is a local of this body.
#[test]
fn captured_slot_names_takes_ancestors_only() {
    use super::stamps::captured_slot_names;
    use crate::ir::{Binding, SlotRef, Var};
    let stamped = |name: &str, owner: u32, slot: u32| {
        Statement::Expr(Expression::Value(Value::Binding(Binding::Variable(Var {
            name: name.to_string(),
            slot: Some(SlotRef { owner, slot }),
        }))))
    };
    let mut ctx = ClosureContext::new();
    ctx.parent_function.insert(5, 4);
    ctx.parent_function.insert(4, 1);
    let body = vec![
        stamped("fiber", 1, 0),
        stamped("inlined", 9, 0),
        stamped("mine", 5, 0),
    ];
    let names = captured_slot_names(&ctx, 5, &body);
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names["fiber"].contains(&SlotRef { owner: 1, slot: 0 }));
}
