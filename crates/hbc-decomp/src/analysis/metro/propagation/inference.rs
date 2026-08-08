use super::{default_roles, is_meaningful_require_name};
use super::define_property::{
    infer_name_from_all_define_properties, infer_name_from_define_property, is_export_barrel,
};
use crate::analysis::metro::detection::is_meaningful_name;
use crate::ir::{target_to_key, Expression, PropertyKey, Statement, Value};
use std::collections::HashMap;
use std::collections::BTreeMap;

pub(super) fn infer_module_name_from_stmts(
    stmts: &[Statement],
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<String> {
    // A React Native codegen'd native component carries its real name in the
    // view config it exports. Nothing else in the body identifies it, and its
    // only named export is the fixed `__INTERNAL_VIEW_CONFIG` protocol key.
    if let Some(name) = infer_name_from_view_config(stmts) {
        return Some(name);
    }

    // A barrel's export keys are its dependencies' names, not its own.
    let barrel = is_export_barrel(stmts);

    // Pre-pass: collect variable definitions for descriptor/value lookup
    let mut var_defs: HashMap<String, &Expression> = HashMap::new();
    for stmt in stmts {
        match stmt {
            Statement::Let { name, value, .. } => {
                var_defs.insert(name.clone(), value);
            }
            Statement::Assign { target, value } => {
                if let Some(name) = target_to_key(target) {
                    var_defs.insert(name, value);
                }
            }
            _ => {}
        }
    }

    for stmt in stmts {
        match stmt {
            Statement::Assign { target, value } => {
                let is_export = match target {
                    crate::ir::AssignTarget::Variable(n) => default_roles().is_exports_param(n),
                    crate::ir::AssignTarget::Member { object, .. } => match object {
                        Expression::Value(Value::Variable(n)) => {
                            default_roles().is_module_param(n) || default_roles().is_exports_param(n)
                        }
                        Expression::Value(Value::Parameter(idx)) => {
                            *idx == default_roles().module_idx
                                || *idx == default_roles().exports_idx
                                || *idx == 2
                        }
                        _ => false,
                    },
                    _ => false,
                };

                if is_export {
                    if let Some(name) = infer_from_expr(value, functions, visited) {
                        return Some(name);
                    }
                    if let crate::ir::AssignTarget::Member { property, .. } = target {
                        if property == "default" {
                            if let Expression::Value(Value::Variable(v)) = value {
                                if is_meaningful_name(v) && is_meaningful_require_name(v) {
                                    return Some(v.clone());
                                }
                            }
                        }
                    }
                    if let crate::ir::AssignTarget::Member { property, .. } = target {
                        if property != "default" && property != "exports" && property != "__esModule"
                            && is_meaningful_name(property)
                        {
                            return Some(property.clone());
                        }
                    }
                }

                if let Some(name) = infer_from_expr(value, functions, visited) {
                    return Some(name);
                }
            }
            Statement::Expr(expr) => {
                if !barrel {
                    if let Some(name) = infer_name_from_define_property(expr, &var_defs, functions, visited) {
                        return Some(name);
                    }
                }
                if let Some(name) = infer_from_expr(expr, functions, visited) {
                    return Some(name);
                }
            }
            Statement::Return(Some(value)) => {
                if let Some(name) = infer_from_expr(value, functions, visited) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }

    // Detect __exportStar(require(dep), exports)
    for stmt in stmts {
        let call = match stmt {
            Statement::Expr(e) => Some(e),
            Statement::Assign { value, .. } => Some(value),
            _ => None,
        };
        if let Some(Expression::Call { callee, arguments }) = call {
            let is_export_star = match &**callee {
                Expression::Value(Value::Variable(n)) => n.contains("exportStar") || n.contains("__export"),
                _ => false,
            };
            if is_export_star && !arguments.is_empty() {
                if let Some(name) = infer_from_expr(&arguments[0], functions, visited) {
                    return Some(name);
                }
            }
        }
    }

    // Try naming from export default { key1() {}, key2() {} }
    for stmt in stmts {
        if let Statement::Assign { target, value } = stmt {
            let is_default_export = match target {
                crate::ir::AssignTarget::Member { object, property } => {
                    property == "default" && match object {
                        Expression::Value(Value::Variable(n)) => default_roles().is_exports_param(n),
                        _ => false,
                    }
                }
                _ => false,
            };
            if is_default_export {
                if let Expression::Object { properties } = value {
                    for prop in properties {
                        let key_name = match &prop.key {
                            PropertyKey::Ident(s) | PropertyKey::String(s) => Some(s.as_str()),
                            _ => None,
                        };
                        if let Some(k) = key_name {
                            if k.starts_with(|c: char| c.is_ascii_uppercase()) && is_meaningful_name(k) {
                                return Some(k.to_string());
                            }
                        }
                    }
                    for prop in properties {
                        let key_name = match &prop.key {
                            PropertyKey::Ident(s) | PropertyKey::String(s) => Some(s.as_str()),
                            _ => None,
                        };
                        if let Some(k) = key_name {
                            if is_meaningful_name(k) && k.len() > 3 {
                                return Some(k.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // Last resort: check for named export properties
    if !barrel {
        if let Some(name) = infer_name_from_all_define_properties(stmts) {
            return Some(name);
        }
    }

    // Final fallback: first meaningful Let/const function definition
    for stmt in stmts {
        if let Statement::Let { name, value, .. } = stmt {
            if matches!(value, Expression::Function { .. }) && is_meaningful_name(name) {
                return Some(name.clone());
            }
            if let Expression::Function { name: Some(fname), .. } = value {
                if is_meaningful_name(fname) {
                    return Some(fname.clone());
                }
            }
        }
        if let Statement::Assign { target: crate::ir::AssignTarget::Variable(_), value } = stmt {
            if let Expression::Function { name: Some(fname), .. } = value {
                if is_meaningful_name(fname) {
                    return Some(fname.clone());
                }
            }
        }
    }

    // Try naming from requireModule.get/getEnforcing("Name")
    for stmt in stmts {
        let call_expr = match stmt {
            Statement::Let { value, .. } => Some(value),
            Statement::Assign { value, .. } => Some(value),
            Statement::Expr(e) => Some(e),
            _ => None,
        };
        if let Some(expr) = call_expr {
            if let Some(name) = extract_native_module_name(expr) {
                return Some(name);
            }
        }
    }

    // Try naming from meaningful named const exports
    for stmt in stmts {
        if let Statement::Assign { target, .. } = stmt {
            if let crate::ir::AssignTarget::Member { object, property } = target {
                let is_exports = match object {
                    Expression::Value(Value::Variable(obj_name)) => {
                        default_roles().is_exports_param(obj_name)
                    }
                    Expression::Value(Value::Parameter(idx)) => {
                        *idx == default_roles().exports_idx || *idx == 2
                    }
                    _ => false,
                };
                if is_exports
                    && property != "default"
                    && property != "__esModule"
                    && is_meaningful_name(property)
                {
                    return Some(property.clone());
                }
            }
        }
    }

    // .displayName or registerComponent/registerCallableModule
    let all_exprs = collect_call_exprs_from_stmts(stmts);
    for expr in &all_exprs {
        if let Expression::Call { callee, arguments } = expr {
            if let Expression::Member { property, .. } = &**callee {
                let method = match property {
                    PropertyKey::Ident(s) | PropertyKey::String(s) => s.as_str(),
                    _ => "",
                };
                if method == "registerComponent" || method == "registerCallableModule" {
                    for arg in arguments {
                        if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) = arg {
                            if is_meaningful_name(s) {
                                return Some(s.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    // displayName assignments
    for stmt in stmts {
        if let Statement::Assign { target, value } = stmt {
            if let crate::ir::AssignTarget::Member { property, .. } = target {
                if property == "displayName" {
                    if let Expression::Value(Value::Constant(crate::ir::Constant::String(s))) = value {
                        if is_meaningful_name(s) {
                            return Some(s.clone());
                        }
                    }
                }
            }
        }
    }

    // Try naming from first meaningful Let variable name
    for stmt in stmts {
        if let Statement::Let { name, .. } = stmt {
            if is_meaningful_name(name) && name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                return Some(name.clone());
            }
        }
    }

    None
}

// The `uiViewClassName` of a React Native view config (`{ uiViewClassName:
// "RNCSafeAreaProvider", validAttributes: … }`), which is the native component
// name the module exists to register.
fn infer_name_from_view_config(stmts: &[Statement]) -> Option<String> {
    use crate::ir::{Constant, Visitor};

    struct ViewClassName(Option<String>);

    impl<'a> Visitor<'a> for ViewClassName {
        fn visit_expression(&mut self, expr: &'a Expression) {
            if self.0.is_none() {
                if let Expression::Object { properties } = expr {
                    for prop in properties {
                        let is_key = matches!(&prop.key,
                            PropertyKey::Ident(k) | PropertyKey::String(k) if k == "uiViewClassName");
                        if is_key {
                            if let Expression::Value(Value::Constant(Constant::String(s))) = &prop.value {
                                if is_meaningful_name(s) {
                                    self.0 = Some(s.clone());
                                    return;
                                }
                            }
                        }
                    }
                }
                self.walk_expression(expr);
            }
        }
    }

    let mut finder = ViewClassName(None);
    for stmt in stmts {
        finder.visit_statement(stmt);
        if finder.0.is_some() {
            break;
        }
    }
    finder.0
}

// Extract the name from X.getEnforcing("Name") calls (React Native native module pattern).
fn extract_native_module_name(expr: &Expression) -> Option<String> {
    use crate::ir::{Constant, Value};

    let (callee, arguments) = match expr {
        Expression::Call { callee, arguments } => (&**callee, arguments.as_slice()),
        _ => return None,
    };

    if let Expression::Member { property, .. } = callee {
        let method_name = match property {
            PropertyKey::Ident(s) | PropertyKey::String(s) => s.as_str(),
            _ => return None,
        };
        let require_pascal = method_name == "get";
        if method_name != "getEnforcing" && !require_pascal {
            return None;
        }

        for arg in arguments {
            if let Expression::Value(Value::Constant(Constant::String(s))) = arg {
                if is_meaningful_name(s) {
                    if require_pascal && !s.starts_with(|c: char| c.is_ascii_uppercase()) {
                        continue;
                    }
                    return Some(s.clone());
                }
            }
        }
    }

    None
}

// Collect all call expressions from statements, including nested blocks.
fn collect_call_exprs_from_stmts(stmts: &[Statement]) -> Vec<&Expression> {
    let mut result = Vec::new();
    for stmt in stmts {
        match stmt {
            Statement::Expr(e) => result.push(e),
            Statement::Assign { value, .. } => result.push(value),
            Statement::Let { value, .. } => result.push(value),
            Statement::If { then_body, else_body, .. } => {
                result.extend(collect_call_exprs_from_stmts(then_body));
                result.extend(collect_call_exprs_from_stmts(else_body));
            }
            Statement::While { body, .. } | Statement::Block(body) => {
                result.extend(collect_call_exprs_from_stmts(body));
            }
            Statement::For { body, .. } => {
                result.extend(collect_call_exprs_from_stmts(body));
            }
            Statement::TryCatch { try_body, catch_body, finally_body, .. } => {
                result.extend(collect_call_exprs_from_stmts(try_body));
                result.extend(collect_call_exprs_from_stmts(catch_body));
                result.extend(collect_call_exprs_from_stmts(finally_body));
            }
            _ => {}
        }
    }
    result
}

pub(super) fn infer_from_expr(
    expr: &Expression,
    functions: &BTreeMap<u32, Vec<Statement>>,
    visited: &mut std::collections::HashSet<u32>,
) -> Option<String> {
    match expr {
        Expression::Function { id, name, .. } => {
            if let Some(n) = name {
                if is_meaningful_name(n) {
                    return Some(n.clone());
                }
            }
            if visited.insert(id.0) {
                if let Some(body) = functions.get(&id.0) {
                    if let Some(inner) = infer_module_name_from_stmts(body, functions, visited) {
                        return Some(inner);
                    }
                }
            }
            None
        }
        Expression::Call { callee, .. } => infer_from_expr(callee, functions, visited),
        Expression::Value(Value::Variable(name)) => {
            if is_meaningful_name(name) && is_meaningful_require_name(name) {
                Some(name.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AssignTarget, Constant, ObjectProperty, VarKind};

    // `const c = { uiViewClassName: "RNCSafeAreaProvider", … };`
    fn view_config_stmt(class_name: &str) -> Statement {
        Statement::Let {
            name: "c".into(),
            value: Expression::Object {
                properties: vec![
                    ObjectProperty {
                        key: PropertyKey::Ident("uiViewClassName".into()),
                        value: Expression::constant(Constant::String(class_name.into())),
                    },
                    ObjectProperty {
                        key: PropertyKey::Ident("validAttributes".into()),
                        value: Expression::Object { properties: vec![] },
                    },
                ],
            },
            kind: VarKind::Const,
        }
    }

    #[test]
    fn view_config_names_the_native_component() {
        let stmts = vec![
            view_config_stmt("RNCSafeAreaProvider"),
            // The module's only named export is the fixed protocol key.
            Statement::Assign {
                target: AssignTarget::Member {
                    object: Expression::Value(Value::Variable("exports".into())),
                    property: "__INTERNAL_VIEW_CONFIG".into(),
                },
                value: Expression::Value(Value::Variable("c".into())),
            },
        ];
        assert_eq!(
            infer_name_from_view_config(&stmts).as_deref(),
            Some("RNCSafeAreaProvider")
        );
    }

    #[test]
    fn view_config_is_found_inside_nested_expressions() {
        // Codegen wraps the config in a registration call.
        let stmts = vec![Statement::Return(Some(Expression::call(
            Expression::Value(Value::Variable("registerComponent".into())),
            vec![view_config_stmt_value("AndroidProgressBar")],
        )))];
        assert_eq!(
            infer_name_from_view_config(&stmts).as_deref(),
            Some("AndroidProgressBar")
        );
    }

    fn view_config_stmt_value(class_name: &str) -> Expression {
        match view_config_stmt(class_name) {
            Statement::Let { value, .. } => value,
            _ => unreachable!(),
        }
    }

    #[test]
    fn a_module_without_a_view_config_is_unaffected() {
        let stmts = vec![Statement::Let {
            name: "x".into(),
            value: Expression::Object {
                properties: vec![ObjectProperty {
                    key: PropertyKey::Ident("uiViewClassName".into()),
                    // Not a string constant: not a view config we can read.
                    value: Expression::Value(Value::Variable("dynamic".into())),
                }],
            },
            kind: VarKind::Const,
        }];
        assert_eq!(infer_name_from_view_config(&stmts), None);
    }
}
