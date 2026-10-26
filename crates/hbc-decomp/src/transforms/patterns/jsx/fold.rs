use crate::ir::{
    map_nested_bodies_mut, AssignTarget, Binding, Expression, ObjectProperty, PropertyKey,
    Statement, Value,
};
use std::collections::BTreeMap;

// Fold a straight-line run of `obj.prop = value` into the object `obj` holds,
// so `jsx(Tag, obj)` sees a literal. A later write of a `jsx(...)` call onto a
// key that already has a value is the slot-reuse overwrite (`children = real`
// then `children = jsx(View, {})`); the earlier value stays.
//
// The store's value moves up to the literal, so nothing between the two may
// change what it reads: a statement in between that writes a variable the
// value reads, or one with an effect when the value has one too, ends the run
// for that object. A raw generator step built its result as `obj5 = { value:
// null }; _Promise = globalThis.Promise; obj5.value = _Promise.resolve(…)`, and
// the fold read `_Promise` before it was assigned (then the dead store went).
pub(super) fn fold_prop_assignments(stmts: Vec<Statement>) -> Vec<Statement> {
    // An object literal in progress, with what ran since it was written.
    struct Open {
        literal: Expression,
        written: std::collections::HashSet<String>,
        effects: bool,
    }
    let mut objects: BTreeMap<String, Open> = BTreeMap::new();
    let open = |literal: Expression| Open {
        literal,
        written: std::collections::HashSet::new(),
        effects: false,
    };
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        // Whether `stmt` folded into a literal (it then runs nowhere else).
        let mut folded = false;
        if let Statement::Assign { target, value } = &stmt {
            if let (Some(obj_name), Some(key)) = (member_var_target(target), ident_key(target)) {
                if let Some(o) = objects.get(&obj_name) {
                    let reads = variables_read(value);
                    let blocked = reads.iter().any(|r| o.written.contains(r))
                        || (o.effects && value.has_side_effects());
                    if blocked {
                        objects.remove(&obj_name);
                    } else if let Expression::Object { mut properties } = o.literal.clone() {
                        let later_is_jsx = matches!(value, Expression::Call { callee, .. } if super::is_jsx_call(callee));
                        let already = properties.iter().any(|p| prop_key_eq(&p.key, &key));
                        if !(already && later_is_jsx) {
                            if let Some(existing) =
                                properties.iter_mut().find(|p| prop_key_eq(&p.key, &key))
                            {
                                existing.value = value.clone();
                            } else {
                                properties.push(ObjectProperty {
                                    key,
                                    value: value.clone(),
                                });
                            }
                            let obj = Expression::Object { properties };
                            if let Some(o) = objects.get_mut(&obj_name) {
                                o.literal = obj.clone();
                            }
                            update_binding_value(&mut out, &obj_name, obj);
                        }
                        folded = true;
                    }
                }
            }
        }
        if folded {
            continue;
        }
        // What this statement does to every literal still open.
        let writes = variables_written(&stmt);
        let effects = crate::ir::stmt_has_side_effects(&stmt);
        for o in objects.values_mut() {
            o.written.extend(writes.iter().cloned());
            o.effects |= effects;
        }
        match stmt {
            Statement::Let { name, value, kind } => {
                if matches!(value, Expression::Object { .. }) {
                    objects.insert(name.name.clone(), open(value.clone()));
                } else {
                    objects.remove(name.as_str());
                }
                out.push(Statement::Let { name, value, kind });
            }
            Statement::Assign { target, value } => {
                if let AssignTarget::Binding(Binding::Variable(name)) = &target {
                    if matches!(value, Expression::Object { .. }) {
                        objects.insert(name.name.clone(), open(value.clone()));
                    } else {
                        objects.remove(name.as_str());
                    }
                }
                out.push(Statement::Assign { target, value });
            }
            other => {
                let mut s = other;
                map_nested_bodies_mut(&mut s, fold_prop_assignments);
                out.push(s);
            }
        }
    }
    out
}

fn variables_read(e: &Expression) -> std::collections::HashSet<String> {
    use crate::ir::Visitor;
    struct R(std::collections::HashSet<String>);
    impl<'a> Visitor<'a> for R {
        fn visit_expression(&mut self, e: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(v))) = e {
                self.0.insert(v.name.clone());
            }
            self.walk_expression(e);
        }
    }
    let mut r = R(std::collections::HashSet::new());
    r.visit_expression(e);
    r.0
}

// Every variable the statement assigns or declares, nested bodies included.
fn variables_written(s: &Statement) -> std::collections::HashSet<String> {
    use crate::ir::Visitor;
    struct W(std::collections::HashSet<String>);
    impl<'a> Visitor<'a> for W {
        fn visit_statement(&mut self, s: &'a Statement) {
            if let Statement::Let { name, .. } = s {
                self.0.insert(name.name.clone());
            }
            self.walk_statement(s);
        }
        fn visit_assign_target(&mut self, t: &'a AssignTarget) {
            if let AssignTarget::Binding(Binding::Variable(v)) = t {
                self.0.insert(v.name.clone());
            }
            self.walk_assign_target(t);
        }
    }
    let mut w = W(std::collections::HashSet::new());
    w.visit_statement(s);
    w.0
}

fn member_var_target(target: &AssignTarget) -> Option<String> {
    if let AssignTarget::Member { object, .. } = target {
        if let Expression::Value(Value::Binding(Binding::Variable(name))) = object {
            return Some(name.name.clone());
        }
    }
    None
}

fn ident_key(target: &AssignTarget) -> Option<PropertyKey> {
    if let AssignTarget::Member { property, .. } = target {
        return Some(PropertyKey::Ident(property.clone()));
    }
    None
}

fn prop_key_eq(a: &PropertyKey, b: &PropertyKey) -> bool {
    match (a, b) {
        (
            PropertyKey::Ident(x) | PropertyKey::String(x),
            PropertyKey::Ident(y) | PropertyKey::String(y),
        ) => x == y,
        _ => false,
    }
}

fn update_binding_value(stmts: &mut [Statement], name: &str, value: Expression) {
    for stmt in stmts.iter_mut().rev() {
        match stmt {
            Statement::Let {
                name: n, value: v, ..
            } if n == name => {
                *v = value;
                return;
            }
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Variable(n)),
                value: v,
            } if n == name => {
                *v = value;
                return;
            }
            _ => {}
        }
    }
}
