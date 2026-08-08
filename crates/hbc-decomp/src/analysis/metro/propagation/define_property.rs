// defineProperty-based module name inference.

use super::default_roles;
use super::inference::infer_from_expr;
use crate::analysis::metro::detection::is_meaningful_name;
use crate::ir::{target_to_key, Expression, PropertyKey, Statement, Value};
use std::collections::HashMap;
use std::collections::BTreeMap;

// Try to infer a module name from a defineProperty(exports, "name", descriptor) call.
pub(super) fn infer_name_from_define_property(
    expr: &Expression,
    var_defs: &HashMap<String, &Expression>,
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<String> {
    use crate::ir::Constant;

    let (callee, arguments) = match expr {
        Expression::Call { callee, arguments } => (&**callee, arguments),
        _ => return None,
    };

    // Check callee is *.defineProperty
    let is_define_prop = match callee {
        Expression::Member { property, .. } => {
            matches!(property, PropertyKey::Ident(s) | PropertyKey::String(s) if s == "defineProperty")
        }
        _ => false,
    };
    if !is_define_prop { return None; }

    // Determine argument layout (with or without this-arg)
    let (name_idx, desc_idx) = if arguments.len() == 3 {
        (1, 2)
    } else if arguments.len() >= 4 {
        let first_is_exports = match &arguments[0] {
            Expression::Value(Value::Variable(n)) => default_roles().is_exports_param(n),
            Expression::Value(Value::Parameter(idx)) if *idx == default_roles().exports_idx => true,
            _ => false,
        };
        if first_is_exports { (1, 2) } else { (2, 3) }
    } else {
        return None;
    };

    // Get the property name
    let prop_name = match &arguments[name_idx] {
        Expression::Value(Value::Constant(Constant::String(s))) => s.as_str(),
        _ => return None,
    };

    if prop_name == "__esModule" { return None; }

    // Try to extract the exported value/function name from the descriptor
    let descriptor = &arguments[desc_idx];
    let resolved_descriptor = match descriptor {
        Expression::Value(Value::Variable(name)) => var_defs.get(name.as_str()).copied(),
        Expression::Value(Value::Register(r)) => var_defs.get(&format!("r{r}")).copied(),
        _ => Some(descriptor),
    };

    if let Some(desc_expr) = resolved_descriptor {
        if let Expression::Object { properties } = desc_expr {
            for prop in properties {
                let key = match &prop.key {
                    PropertyKey::Ident(k) | PropertyKey::String(k) => k.as_str(),
                    _ => continue,
                };
                match key {
                    "get" => {
                        if let Some(name) = infer_from_expr(&prop.value, functions, visited) {
                            return Some(name);
                        }
                        if let Expression::Function { id, .. } = &prop.value {
                            if visited.insert(id.0) {
                                if let Some(body) = functions.get(&id.0) {
                                    let mut getter_defs: HashMap<String, &Expression> = HashMap::new();
                                    for stmt in body {
                                        match stmt {
                                            Statement::Assign { target, value } => {
                                                if let Some(name) = target_to_key(target) {
                                                    getter_defs.insert(name, value);
                                                }
                                            }
                                            Statement::Let { name, value, .. } => {
                                                getter_defs.insert(name.clone(), value);
                                            }
                                            _ => {}
                                        }
                                    }

                                    for stmt in body {
                                        if let Statement::Return(Some(ret_val)) = stmt {
                                            if let Expression::Value(Value::Variable(v)) = ret_val {
                                                if is_meaningful_name(v) {
                                                    return Some(v.clone());
                                                }
                                            }
                                            let ret_key = match ret_val {
                                                Expression::Value(Value::Register(r)) => Some(format!("r{r}")),
                                                Expression::Value(Value::Variable(v)) => Some(v.clone()),
                                                _ => None,
                                            };
                                            if let Some(key) = ret_key {
                                                if let Some(def_expr) = getter_defs.get(&key) {
                                                    if let Some(name) = infer_from_expr(def_expr, functions, visited) {
                                                        return Some(name);
                                                    }
                                                }
                                                if let Some(def_expr) = var_defs.get(&key) {
                                                    if let Some(name) = infer_from_expr(def_expr, functions, visited) {
                                                        return Some(name);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "value" => {
                        if let Some(name) = infer_from_expr(&prop.value, functions, visited) {
                            return Some(name);
                        }
                        if let Expression::Value(Value::Variable(v)) = &prop.value {
                            if is_meaningful_name(v) {
                                return Some(v.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Fallback: use the property name if it's not "default"
    if prop_name != "default" && is_meaningful_name(prop_name) {
        return Some(prop_name.to_string());
    }

    None
}

// A module re-exporting more than this many names through `defineProperty`
// getters is a barrel (`react-native/index.js` has 79), and none of its keys
// names it. Below the threshold the first key is a real signal: `exports.Foo`.
const MAX_NAMED_EXPORTS_FOR_NAMING: usize = 4;

// Whether a factory body is such a barrel. Both defineProperty naming
// strategies pick a key (or what a key's getter requires), which is arbitrary
// once there are many: naming the RN barrel after its first getter labelled 91
// other modules `registerCallableModule` through dependency propagation.
pub(super) fn is_export_barrel(stmts: &[Statement]) -> bool {
    named_export_keys(stmts, MAX_NAMED_EXPORTS_FOR_NAMING + 1).len()
        > MAX_NAMED_EXPORTS_FOR_NAMING
}

// For unnamed modules, try to infer a name from the first meaningful named export property.
pub(super) fn infer_name_from_all_define_properties(stmts: &[Statement]) -> Option<String> {
    named_export_keys(stmts, 1).into_iter().next()
}

// The `defineProperty` export keys of a factory body, in order, stopping once
// `limit` have been collected.
fn named_export_keys(stmts: &[Statement], limit: usize) -> Vec<String> {
    use crate::ir::Constant;

    let mut keys: Vec<String> = Vec::new();

    for stmt in stmts {
        if let Statement::Expr(expr) = stmt {
            if let Expression::Call { callee, arguments } = expr {
                let is_define_prop = match &**callee {
                    Expression::Member { property, .. } => {
                        matches!(property, PropertyKey::Ident(s) | PropertyKey::String(s) if s == "defineProperty")
                    }
                    _ => false,
                };
                if !is_define_prop { continue; }

                let name_idx = if arguments.len() == 3 {
                    1
                } else if arguments.len() >= 4 {
                    let first_is_exports = match &arguments[0] {
                        Expression::Value(Value::Variable(n)) => default_roles().is_exports_param(n),
                        Expression::Value(Value::Parameter(idx)) if *idx == default_roles().exports_idx => true,
                        _ => false,
                    };
                    if first_is_exports { 1 } else { 2 }
                } else {
                    continue;
                };

                if let Expression::Value(Value::Constant(Constant::String(s))) = &arguments[name_idx] {
                    if s != "__esModule" && s != "default" && is_meaningful_name(s) {
                        keys.push(s.clone());
                        if keys.len() >= limit {
                            return keys;
                        }
                    }
                }
            }
        }
    }

    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Constant;

    // `Object.defineProperty(obj, "<key>", { get: … })`
    fn define_property(key: &str) -> Statement {
        Statement::Expr(Expression::call(
            Expression::member(Expression::Value(Value::Variable("Object".into())), "defineProperty"),
            vec![
                Expression::Value(Value::Variable("obj".into())),
                Expression::constant(Constant::String(key.into())),
                Expression::Object { properties: vec![] },
            ],
        ))
    }

    #[test]
    fn single_named_export_still_names_the_module() {
        let stmts = vec![define_property("Button")];
        assert!(!is_export_barrel(&stmts));
        assert_eq!(
            infer_name_from_all_define_properties(&stmts).as_deref(),
            Some("Button")
        );
    }

    #[test]
    fn barrel_is_not_named_after_its_first_key() {
        let keys = [
            "registerCallableModule",
            "AccessibilityInfo",
            "ActivityIndicator",
            "Button",
            "DrawerLayoutAndroid",
            "FlatList",
        ];
        let stmts: Vec<Statement> = keys.iter().map(|k| define_property(k)).collect();
        assert!(is_export_barrel(&stmts));
    }
}
