pub mod context;
pub mod identity;
pub mod info;
pub mod scope_graph;
pub mod stamps;

#[cfg(test)]
mod inheritance_tests;

use crate::ir::{AssignTarget, Binding, Expression, PropertyKey, SlotRef, Statement, Value, Var};

pub use context::ClosureContext;
use info::encode_level_slot;
pub use info::{ClosureInfo, ClosureSlotValue};
use std::collections::BTreeMap;

// Hermes bytecode uses an environment system for closures.
// IR: `ClosureVar { level, slot }` where level 0 = current function env, 1 = parent, …
// (levels come from GetEnvironment during IR build, see ir/builder/env_state.rs).
// This pass renames those slots to JS identifiers via ClosureInfo::get_slot_name.
pub fn resolve_closures(stmts: Vec<Statement>, info: &ClosureInfo) -> Vec<Statement> {
    resolve_closures_recording(stmts, info, None).0
}

// The same pass, also returning, for every slot access (own slots at level 0,
// ancestor captures above), the name it was baked as and the (level, slot). A capture
// baked while its slot had no name prints as `closure_N`, which says nothing
// about the level; the late inherit pass reads this record to resolve it
// once the owner has named the slot.
//
// Given `scope` (the context and the id of the function whose body this is),
// every variable baked from a slot is also stamped with the slot it is, as an
// absolute `SlotRef`, so the identity outlives the spelling (K4.2).
pub fn resolve_closures_recording(
    stmts: Vec<Statement>,
    info: &ClosureInfo,
    scope: Option<(&ClosureContext, u32)>,
) -> (Vec<Statement>, BTreeMap<String, (u32, u32)>) {
    let (out, baked, _) = resolve_closures_with_owners(stmts, info, scope);
    (out, baked)
}

// The same, also returning the scope each baked name was stamped with
// (IDENTITY_PLAN.md A1b3): the owner later passes would otherwise walk the
// chain from the level for.
#[allow(clippy::type_complexity)]
pub fn resolve_closures_with_owners(
    stmts: Vec<Statement>,
    info: &ClosureInfo,
    scope: Option<(&ClosureContext, u32)>,
) -> (
    Vec<Statement>,
    BTreeMap<String, (u32, u32)>,
    BTreeMap<String, u32>,
) {
    let resolver = Resolver {
        info,
        scope,
        baked: std::cell::RefCell::new(BTreeMap::new()),
        owners: std::cell::RefCell::new(BTreeMap::new()),
    };
    let out = stmts
        .into_iter()
        .map(|s| resolve_stmt(s, &resolver))
        .collect();
    (
        out,
        resolver.baked.into_inner(),
        resolver.owners.into_inner(),
    )
}

fn resolve_list(stmts: Vec<Statement>, info: &Resolver) -> Vec<Statement> {
    stmts.into_iter().map(|s| resolve_stmt(s, info)).collect()
}

struct Resolver<'a> {
    info: &'a ClosureInfo,
    scope: Option<(&'a ClosureContext, u32)>,
    baked: std::cell::RefCell<BTreeMap<String, (u32, u32)>>,
    owners: std::cell::RefCell<BTreeMap<String, u32>>,
}

impl Resolver<'_> {
    // Level 0 is recorded too: the owner's own binding for a slot is what
    // its captures must end up named after.
    fn record(&self, name: &str, level: u32, slot: u32, owner: Option<u32>) {
        let mut baked = self.baked.borrow_mut();
        if baked.contains_key(name) {
            return;
        }
        baked.insert(name.to_string(), (level, slot));
        if let Some(owner) = owner {
            self.owners.borrow_mut().insert(name.to_string(), owner);
        }
    }

    // The variable a slot access becomes: its name, and the slot it is when the
    // owner is known. A placed access is its scope's, named from that scope's
    // table (IDENTITY_PLAN.md A2b, A1b2); the walk answers only for the rest.
    fn bake(&self, level: u32, slot: u32, scope: Option<(u32, Option<u32>)>) -> Var {
        // The scope's id as the context numbers it now.
        let scope = scope.and_then(|r| self.scope.and_then(|(ctx, _)| ctx.known_scope_id(r)));
        let encoded = encode_level_slot(level, slot);
        let mut name = if self.slots.contains_key(&encoded) {
            self.get_slot_name(encoded)
        } else if level == 0 {
            self.get_slot_name(slot)
        } else {
            // Unresolved parent-env capture: same family as local `closure_N`.
            crate::ir::Value::closure_var_name(level, slot)
        };
        // A1b1: the name the scope's own table gives, against the level view's.
        if let (Some((ctx, func)), Some(id)) = (self.scope, scope) {
            let scoped = match ctx.function_closures.get(&id) {
                Some(info) if info.slots.contains_key(&slot) => info.get_slot_name(slot),
                _ if id == func => self.get_slot_name(slot),
                _ => crate::ir::Value::closure_var_name(level, slot),
            };
            let same = scoped == name;
            SCOPE_NAMES[if same { 0 } else { 1 }].fetch_add(1, Relaxed);
            if !same {
                log::trace!(target: stamps::TARGET, "scoped name: F{func} level {level} slot {slot}: `{name}` by level, `{scoped}` by scope {id}");
            }
            name = scoped;
        }
        let stamp = self
            .scope
            .and_then(|(ctx, func)| {
                let walked = ctx.slot_owner(func, level);
                let Some(id) = scope else {
                    SCOPE_STAMPS[0].fetch_add(1, Relaxed);
                    return walked;
                };
                let agrees = walked == Some(id);
                SCOPE_STAMPS[if agrees { 1 } else { 2 }].fetch_add(1, Relaxed);
                if !agrees {
                    // Counted, not followed: the walk is what moved (A2a
                    // brought this to 0 on every bundle measured).
                    log::trace!(target: stamps::TARGET, "scoped stamp: F{func} level {level} slot {slot}: scope {id}, the walk {walked:?}");
                }
                Some(id)
            });
        self.record(&name, level, slot, stamp);
        let slot = stamp.map(|owner| SlotRef { owner, slot });
        Var { name, slot }
    }
}

// Stamps W6 gave, over the run: [walked (no scope), scope agreeing with the
// walk, scope disagreeing]. Logged with the identity check.
static SCOPE_STAMPS: [std::sync::atomic::AtomicU32; 3] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];
use std::sync::atomic::Ordering::Relaxed;

pub fn scope_stamp_counts() -> [u32; 3] {
    [0, 1, 2].map(|i| SCOPE_STAMPS[i].load(Relaxed))
}

// A1b1: scoped accesses whose name by scope [equals, differs from] the name by
// level.
static SCOPE_NAMES: [std::sync::atomic::AtomicU32; 2] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];

pub fn scope_name_counts() -> [u32; 2] {
    [0, 1].map(|i| SCOPE_NAMES[i].load(Relaxed))
}

impl std::ops::Deref for Resolver<'_> {
    type Target = ClosureInfo;
    fn deref(&self) -> &ClosureInfo {
        self.info
    }
}

fn resolve_stmt(stmt: Statement, info: &Resolver) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target: resolve_target(target, info),
            value: resolve_expr(value, info),
        },
        Statement::Delete { target, result } => Statement::Delete {
            target: resolve_expr(target, info),
            result,
        },
        Statement::Expr(e) => Statement::Expr(resolve_expr(e, info)),
        Statement::Return(Some(e)) => Statement::Return(Some(resolve_expr(e, info))),
        Statement::Throw(e) => Statement::Throw(resolve_expr(e, info)),
        Statement::Let { name, value, kind } => Statement::Let {
            name,
            value: resolve_expr(value, info),
            kind,
        },
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: resolve_expr(condition, info),
            then_body: resolve_list(then_body, info),
            else_body: resolve_list(else_body, info),
        },
        Statement::While { condition, body } => Statement::While {
            condition: resolve_expr(condition, info),
            body: resolve_list(body, info),
        },
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: resolve_list(body, info),
            condition: resolve_expr(condition, info),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(resolve_stmt(*s, info))),
            condition: condition.map(|c| resolve_expr(c, info)),
            update: update.map(|s| Box::new(resolve_stmt(*s, info))),
            body: resolve_list(body, info),
        },
        Statement::ForIn {
            variable,
            object,
            body,
        } => Statement::ForIn {
            variable,
            object: resolve_expr(object, info),
            body: resolve_list(body, info),
        },
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => Statement::ForOf {
            variable,
            iterable: resolve_expr(iterable, info),
            body: resolve_list(body, info),
        },
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => Statement::Switch {
            discriminant: resolve_expr(discriminant, info),
            cases: cases
                .into_iter()
                .map(|(v, b)| (resolve_expr(v, info), resolve_list(b, info)))
                .collect(),
            default: default.map(|d| resolve_list(d, info)),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: resolve_list(try_body, info),
            catch_param,
            catch_body: resolve_list(catch_body, info),
            finally_body: resolve_list(finally_body, info),
        },
        Statement::Class {
            name,
            super_class,
            constructor,
            methods,
        } => Statement::Class {
            name,
            super_class: super_class.map(|e| resolve_expr(e, info)),
            constructor: constructor.map(|s| Box::new(resolve_stmt(*s, info))),
            methods: methods
                .into_iter()
                .map(|mut m| {
                    m.value = resolve_expr(m.value, info);
                    m
                })
                .collect(),
        },
        Statement::CondGoto {
            condition,
            target,
            fallthrough,
        } => Statement::CondGoto {
            condition: resolve_expr(condition, info),
            target,
            fallthrough,
        },
        Statement::Block(inner) => Statement::Block(resolve_list(inner, info)),
        other => other,
    }
}

fn resolve_target(target: AssignTarget, info: &Resolver) -> AssignTarget {
    match target {
        AssignTarget::Binding(Binding::ClosureVar { level, slot, scope }) => {
            AssignTarget::Binding(Binding::Variable(info.bake(level, slot, scope.0)))
        }
        AssignTarget::Member { object, property } => AssignTarget::Member {
            object: resolve_expr(object, info),
            property,
        },
        AssignTarget::Index { object, key } => AssignTarget::Index {
            object: resolve_expr(object, info),
            key: resolve_expr(key, info),
        },
        AssignTarget::DestructuringObject(props) => AssignTarget::DestructuringObject(
            props
                .into_iter()
                .map(|(k, t, def)| {
                    (
                        k,
                        resolve_target(t, info),
                        def.map(|e| resolve_expr(e, info)),
                    )
                })
                .collect(),
        ),
        AssignTarget::DestructuringObjectRest { properties, rest } => {
            AssignTarget::DestructuringObjectRest {
                properties: properties
                    .into_iter()
                    .map(|(k, t, def)| {
                        (
                            k,
                            resolve_target(t, info),
                            def.map(|e| resolve_expr(e, info)),
                        )
                    })
                    .collect(),
                rest: Box::new(resolve_target(*rest, info)),
            }
        }
        AssignTarget::DestructuringArray(elements) => AssignTarget::DestructuringArray(
            elements
                .into_iter()
                .map(|e| {
                    e.map(|(t, def)| (resolve_target(t, info), def.map(|d| resolve_expr(d, info))))
                })
                .collect(),
        ),
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            AssignTarget::DestructuringArrayRest {
                elements: elements
                    .into_iter()
                    .map(|e| {
                        e.map(|(t, def)| {
                            (resolve_target(t, info), def.map(|d| resolve_expr(d, info)))
                        })
                    })
                    .collect(),
                rest: Box::new(resolve_target(*rest, info)),
            }
        }
        AssignTarget::Rest(inner) => AssignTarget::Rest(Box::new(resolve_target(*inner, info))),
        other => other,
    }
}

fn resolve_property(property: PropertyKey, info: &Resolver) -> PropertyKey {
    match property {
        PropertyKey::Computed(e) => PropertyKey::Computed(Box::new(resolve_expr(*e, info))),
        other => other,
    }
}

fn resolve_expr(expr: Expression, info: &Resolver) -> Expression {
    match expr {
        Expression::Value(Value::Binding(Binding::ClosureVar { level, slot, scope })) => {
            Expression::Value(Value::Binding(Binding::Variable(
                info.bake(level, slot, scope.0),
            )))
        }
        Expression::Binary { op, left, right } => Expression::Binary {
            op,
            left: Box::new(resolve_expr(*left, info)),
            right: Box::new(resolve_expr(*right, info)),
        },
        Expression::Unary { op, operand } => Expression::Unary {
            op,
            operand: Box::new(resolve_expr(*operand, info)),
        },
        Expression::Call { callee, arguments } => Expression::Call {
            callee: Box::new(resolve_expr(*callee, info)),
            arguments: arguments
                .into_iter()
                .map(|a| resolve_expr(a, info))
                .collect(),
        },
        Expression::Member {
            object,
            property,
            optional,
        } => Expression::Member {
            object: Box::new(resolve_expr(*object, info)),
            property: resolve_property(property, info),
            optional,
        },
        Expression::New { callee, arguments } => Expression::New {
            callee: Box::new(resolve_expr(*callee, info)),
            arguments: arguments
                .into_iter()
                .map(|a| resolve_expr(a, info))
                .collect(),
        },
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => Expression::Conditional {
            condition: Box::new(resolve_expr(*condition, info)),
            then_expr: Box::new(resolve_expr(*then_expr, info)),
            else_expr: Box::new(resolve_expr(*else_expr, info)),
        },
        Expression::Array { elements } => Expression::Array {
            elements: elements
                .into_iter()
                .map(|e| e.map(|ex| resolve_expr(ex, info)))
                .collect(),
        },
        Expression::Object { properties } => Expression::Object {
            properties: properties
                .into_iter()
                .map(|mut p| {
                    p.value = resolve_expr(p.value, info);
                    p
                })
                .collect(),
        },
        Expression::Assignment { target, value } => Expression::Assignment {
            target: Box::new(resolve_target(*target, info)),
            value: Box::new(resolve_expr(*value, info)),
        },
        Expression::Spread(e) => Expression::Spread(Box::new(resolve_expr(*e, info))),
        Expression::TemplateLiteral {
            quasis,
            expressions,
        } => Expression::TemplateLiteral {
            quasis,
            expressions: expressions
                .into_iter()
                .map(|e| resolve_expr(e, info))
                .collect(),
        },
        Expression::Yield { value, delegate } => Expression::Yield {
            value: Box::new(resolve_expr(*value, info)),
            delegate,
        },
        Expression::Await(e) => Expression::Await(Box::new(resolve_expr(*e, info))),
        Expression::JSXElement {
            tag,
            attributes,
            children,
        } => Expression::JSXElement {
            tag,
            attributes: attributes
                .into_iter()
                .map(|(k, v)| (k, resolve_expr(v, info)))
                .collect(),
            children: children
                .into_iter()
                .map(|c| resolve_expr(c, info))
                .collect(),
        },
        other => other,
    }
}
