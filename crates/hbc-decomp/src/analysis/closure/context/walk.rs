// Statement/expression walk for env stores and nested Function edges.
use super::super::info::{ClosureInfo, ClosureSlotValue};
use super::{ClosureContext, ScopeRef};
use crate::ir::{AssignTarget, Binding, Expression, Statement};
use std::collections::BTreeMap;

// A store into an ancestor's slot, applied once every parent edge is known:
// (from function, level, slot, value, the scope the access was placed in).
// A placed store lands in its scope, not `level` hops up `parent_function`
// (IDENTITY_PLAN.md A2d: a shared function's parent is no longer the
// representative its levels were counted from).
pub(super) type Deferred = (u32, u32, u32, ClosureSlotValue, Option<ScopeRef>);

impl ClosureContext {
    pub(super) fn analyze_stmt_context(
        &mut self,
        current_fn: u32,
        stmt: &Statement,
        info: &mut ClosureInfo,
        reg_values: &mut BTreeMap<u32, ClosureSlotValue>,
        named_values: &mut BTreeMap<String, ClosureSlotValue>,
        deferred: &mut Vec<Deferred>,
    ) {
        match stmt {
            Statement::Assign { target, value } => {
                self.track_nested_functions(current_fn, value);

                if let Expression::Function { id, name, .. } = value {
                    if let AssignTarget::Binding(Binding::Register(r)) = target {
                        reg_values.insert(
                            *r,
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: name.clone(),
                            },
                        );
                    }
                    if let AssignTarget::Binding(Binding::Variable(vname)) = target {
                        named_values.insert(
                            vname.name.clone(),
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: name.clone().or_else(|| Some(vname.name.clone())),
                            },
                        );
                    }
                }

                // Track through register copies (r5 = r3) so later env stores see
                // the origin (require / function / named binding).
                if let AssignTarget::Binding(Binding::Register(r)) = target {
                    if let Some(val) = Self::resolve_store_value(value, reg_values, named_values) {
                        reg_values.insert(*r, val);
                    }
                }

                if let AssignTarget::Binding(Binding::Variable(name)) = target {
                    if let Some(val) = Self::resolve_store_value(value, reg_values, named_values) {
                        named_values.insert(name.name.clone(), val);
                    }
                }

                if let AssignTarget::Binding(Binding::ClosureVar { slot, level, scope }) = target {
                    if let Some(mut val) =
                        Self::resolve_store_value(value, reg_values, named_values)
                    {
                        // A slot named after a property key (`NativeModules.ExternalPip`)
                        // must not take the name of a class or function bound in
                        // this body (`class ExternalPip`): the declaration owns it.
                        if let (Expression::Member { .. }, ClosureSlotValue::Variable(v)) =
                            (value, &mut val)
                        {
                            let bound = match named_values.get(v.as_str()) {
                                Some(ClosureSlotValue::Function { .. }) => true,
                                Some(ClosureSlotValue::Variable(x)) => x == v,
                                _ => false,
                            };
                            if bound {
                                v.push('2');
                            }
                        }
                        if *level == 0 {
                            trace_store(current_fn, *slot, &val);
                            info.store_slot(*slot, val);
                        } else if *level >= crate::ir::NESTED_ENV_LEVEL_BASE {
                            // A block environment this function built is a
                            // scope of its own. A closure stored into it
                            // was created inside it (the class constructor
                            // goes into slot 0 of the class scope), so the
                            // block, not the function, is its parent.
                            // A placed store is its block's, by site: the
                            // level is a handle (past the level cap several
                            // of this function's blocks share one).
                            let placed = match scope.0 {
                                Some((g, Some(site))) if g == current_fn => {
                                    self.known_scope_id((g, Some(site)))
                                }
                                // A closure's environment off this function's
                                // chain (A2c3): the scope it was placed in.
                                Some(r) => {
                                    if r.1.is_some() {
                                        self.off_chain_blocks.insert(r.0);
                                    }
                                    Some(self.scope_id(r))
                                }
                                None => None,
                            };
                            let scope = match placed {
                                Some(id) => id,
                                None => self.block_at_level(current_fn, *level),
                            };
                            if let ClosureSlotValue::Function { id, .. } = &val {
                                if !self.scope_parents.contains_key(id)
                                    && !self.shared_parents.contains_key(id)
                                {
                                    self.parent_function.insert(*id, scope);
                                }
                            }
                            trace_store(scope, *slot, &val);
                            self.function_closures
                                .entry(scope)
                                .or_default()
                                .store_slot(*slot, val);
                        } else {
                            // Nested body writing into a parent/grandparent env:
                            // the scope it was placed in, when it was, else
                            // the one `level` hops up the parents.
                            deferred.push((current_fn, *level, *slot, val, scope.0));
                        }
                    }
                }
            }
            Statement::Let { value, name, .. } => {
                self.track_nested_functions(current_fn, value);
                if let Expression::Function {
                    id, name: fn_name, ..
                } = value
                {
                    // Prefer the binding name when the function expression is anonymous.
                    if fn_name.is_none() {
                        self.add_function_name(id.0, name.name.clone());
                    }
                    named_values.insert(
                        name.name.clone(),
                        ClosureSlotValue::Function {
                            id: id.0,
                            name: fn_name.clone().or_else(|| Some(name.name.clone())),
                        },
                    );
                } else if let Some(val) = Self::resolve_store_value(value, reg_values, named_values)
                {
                    named_values.insert(name.name.clone(), val);
                }
            }
            Statement::Return(Some(value)) | Statement::Expr(value) | Statement::Throw(value) => {
                self.track_nested_functions(current_fn, value);
            }
            Statement::Delete { target, .. } => {
                self.track_nested_functions(current_fn, target);
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.track_nested_functions(current_fn, condition);
                for s in then_body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                for s in else_body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::While { condition, body } | Statement::DoWhile { body, condition } => {
                self.track_nested_functions(current_fn, condition);
                for s in body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(i) = init {
                    self.analyze_stmt_context(
                        current_fn,
                        i,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                if let Some(c) = condition {
                    self.track_nested_functions(current_fn, c);
                }
                if let Some(u) = update {
                    self.analyze_stmt_context(
                        current_fn,
                        u,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                for s in body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::ForIn { object, body, .. } => {
                self.track_nested_functions(current_fn, object);
                for s in body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::ForOf { iterable, body, .. } => {
                self.track_nested_functions(current_fn, iterable);
                for s in body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::Block(inner) => {
                for s in inner {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::TryCatch {
                try_body,
                catch_body,
                finally_body,
                ..
            } => {
                for s in try_body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                for s in catch_body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                for s in finally_body {
                    self.analyze_stmt_context(
                        current_fn,
                        s,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
            }
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => {
                self.track_nested_functions(current_fn, discriminant);
                for (val, body) in cases {
                    self.track_nested_functions(current_fn, val);
                    for s in body {
                        self.analyze_stmt_context(
                            current_fn,
                            s,
                            info,
                            reg_values,
                            named_values,
                            deferred,
                        );
                    }
                }
                if let Some(d) = default {
                    for s in d {
                        self.analyze_stmt_context(
                            current_fn,
                            s,
                            info,
                            reg_values,
                            named_values,
                            deferred,
                        );
                    }
                }
            }
            Statement::Class {
                name,
                super_class,
                constructor,
                methods,
            } => {
                // The class name is the binding of its constructor: a later
                // `closure_k = ClassName` stores the function, not a value
                // that happens to carry the class's name.
                if let Some(ctor) = methods.iter().find(|m| m.key == "constructor") {
                    if let Expression::Function { id, .. } = &ctor.value {
                        named_values.insert(
                            name.clone(),
                            ClosureSlotValue::Function {
                                id: id.0,
                                name: Some(name.clone()),
                            },
                        );
                    }
                }
                if let Some(sc) = super_class {
                    self.track_nested_functions(current_fn, sc);
                }
                if let Some(c) = constructor {
                    self.analyze_stmt_context(
                        current_fn,
                        c,
                        info,
                        reg_values,
                        named_values,
                        deferred,
                    );
                }
                for m in methods {
                    self.track_nested_functions(current_fn, &m.value);
                    if let Some(body) = &m.body {
                        // A method is a function of its own: it sees the
                        // enclosing names but its locals and registers are its
                        // own. Sharing the maps let a method's `tmp15 =
                        // closure_37` stand for the enclosing body's `tmp15`,
                        // and a slot stored from it took the wrong slot's name.
                        let mut method_regs = BTreeMap::new();
                        let mut method_names = named_values.clone();
                        for s in body {
                            self.analyze_stmt_context(
                                current_fn,
                                s,
                                info,
                                &mut method_regs,
                                &mut method_names,
                                deferred,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Track Function expressions nested in values: bare `function*`, or
    // CreateGenerator's `(function*(){})()` call form.
    pub(super) fn track_nested_functions(&mut self, parent_fn: u32, expr: &Expression) {
        match expr {
            Expression::Function {
                id,
                name,
                is_async,
                is_generator,
                ..
            } => {
                self.add_child(parent_fn, id.0);
                if let Some(n) = name {
                    self.add_function_name(id.0, n.clone());
                }
                if *is_async {
                    self.mark_async(id.0);
                }
                if *is_generator {
                    self.mark_generator(id.0);
                }
            }
            Expression::Call { callee, arguments } => {
                self.track_nested_functions(parent_fn, callee);
                for arg in arguments {
                    self.track_nested_functions(parent_fn, arg);
                }
            }
            Expression::Member {
                object, property, ..
            } => {
                self.track_nested_functions(parent_fn, object);
                if let crate::ir::PropertyKey::Computed(e) = property {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            Expression::New { callee, arguments } => {
                self.track_nested_functions(parent_fn, callee);
                for arg in arguments {
                    self.track_nested_functions(parent_fn, arg);
                }
            }
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                self.track_nested_functions(parent_fn, condition);
                self.track_nested_functions(parent_fn, then_expr);
                self.track_nested_functions(parent_fn, else_expr);
            }
            Expression::Assignment { target, value } => {
                crate::ir::for_each_target_expression(target, &mut |e| {
                    self.track_nested_functions(parent_fn, e)
                });
                self.track_nested_functions(parent_fn, value);
            }
            Expression::Array { elements } => {
                for e in elements.iter().flatten() {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            Expression::Object { properties } => {
                for p in properties {
                    self.track_nested_functions(parent_fn, &p.value);
                    if let crate::ir::PropertyKey::Computed(e) = &p.key {
                        self.track_nested_functions(parent_fn, e);
                    }
                }
            }
            Expression::Unary { operand, .. }
            | Expression::Spread(operand)
            | Expression::Await(operand) => {
                self.track_nested_functions(parent_fn, operand);
            }
            Expression::Binary { left, right, .. } => {
                self.track_nested_functions(parent_fn, left);
                self.track_nested_functions(parent_fn, right);
            }
            Expression::Yield { value, .. } => {
                self.track_nested_functions(parent_fn, value);
            }
            Expression::TemplateLiteral { expressions, .. } => {
                for e in expressions {
                    self.track_nested_functions(parent_fn, e);
                }
            }
            _ => {}
        }
    }
}

// Trace target `slotname`: every value written into a slot at analysis time,
// so a wrong slot name can be traced back to the pass that wrote it.
pub(super) fn trace_store(scope: u32, slot: u32, val: &ClosureSlotValue) {
    if log::log_enabled!(target: "slotname", log::Level::Trace) {
        log::trace!(target: "slotname", "{scope}:{slot} store {val:?}");
    }
}

// `argN`: the placeholder a parameter carries before it is named.
pub(super) fn is_parameter_placeholder(name: &str) -> bool {
    name.strip_prefix("arg")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}
