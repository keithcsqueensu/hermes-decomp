use crate::ir::{AssignTarget, Binding, Expression, PropertyKey, Statement, Value};

thread_local! {
    // The function whose parameters `rename_param_registers_of` is naming.
    static FUNCTION: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

// The same, each parameter read (`Parameter(i)` named) carrying parameter `i`
// of `function` as its identity (IDENTITY_PLAN.md A3 step 2.0b).
pub fn rename_param_registers_of(
    statements: &mut [Statement],
    names: &[Option<String>],
    function: u32,
) {
    let outer = FUNCTION.with(|f| f.replace(Some(function)));
    rename_param_registers(statements, names);
    FUNCTION.with(|f| f.set(outer));
}

fn param_var(index: u32, name: &str) -> crate::ir::Var {
    match FUNCTION.with(|f| f.get()) {
        Some(function) => {
            crate::ir::Var::with_local(name, crate::ir::LocalRef::param(function, index))
        }
        None => name.into(),
    }
}

pub fn rename_param_registers(statements: &mut [Statement], names: &[Option<String>]) {
    // 1. Map of Reg -> Name
    let mut reg_rename_map = std::collections::BTreeMap::new();

    // 2. Map of VarName -> Name
    let mut var_rename_map = std::collections::HashMap::new();

    // Scan for LoadParam to get registers (both v2 IR and legacy formats)
    for stmt in statements.iter() {
        if let Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(r)),
            value,
        } = stmt
        {
            match value {
                // v2 IR: LoadParam produces Value::Parameter(idx)
                Expression::Value(Value::Parameter(idx)) => {
                    let i = *idx;
                    let idx = *idx as usize;
                    if idx < names.len() {
                        if let Some(name) = &names[idx] {
                            reg_rename_map.insert(*r, (name.clone(), i));
                        }
                    }
                }
                // Legacy v1 IR: Expression::Unknown { opcode: "LoadParam", operands }
                Expression::Unknown { opcode, operands } if opcode == "LoadParam" => {
                    if let Some(idx) = operands.first().and_then(|o| o.parse::<usize>().ok()) {
                        if idx < names.len() {
                            if let Some(name) = &names[idx] {
                                reg_rename_map.insert(*r, (name.clone(), idx as u32));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Always include argN -> Name mappings
    for (idx, name_opt) in names.iter().enumerate() {
        if let Some(name) = name_opt {
            var_rename_map.insert(format!("arg{idx}"), name.clone());
        }
    }

    if reg_rename_map.is_empty() && var_rename_map.is_empty() {
        return;
    }

    // 3. Rename usages
    for stmt in statements.iter_mut() {
        rename_stmt(stmt, &reg_rename_map, &var_rename_map);
    }
}

fn rename_stmt(
    stmt: &mut Statement,
    reg_map: &std::collections::BTreeMap<u32, (String, u32)>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match stmt {
        Statement::Let { name, value, .. } => {
            if let Some(new_name) = var_map.get(name.as_str()) {
                // Through `name`, keeping the identity, as the other arms do.
                name.name = new_name.clone();
            }
            rename_expr(value, reg_map, var_map);
        }
        Statement::Assign { target, value } => {
            rename_target(target, reg_map, var_map);
            rename_expr(value, reg_map, var_map);
        }
        Statement::Expr(e) => rename_expr(e, reg_map, var_map),
        Statement::Return(Some(e)) | Statement::Throw(e) => rename_expr(e, reg_map, var_map),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => {
            rename_expr(condition, reg_map, var_map);
            for s in then_body {
                rename_stmt(s, reg_map, var_map);
            }
            for s in else_body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::While { condition, body } => {
            rename_expr(condition, reg_map, var_map);
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::DoWhile { body, condition } => {
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
            rename_expr(condition, reg_map, var_map);
        }
        // A `for … in` / `for … of` over a parameter kept the placeholder
        // (`for (const k in arg1)` under `function extend(fn36, fn35)`), so
        // the loop read a name the signature never bound.
        Statement::ForIn {
            variable,
            object,
            body,
        } => {
            if let Some(new_name) = var_map.get(variable) {
                *variable = new_name.clone();
            }
            rename_expr(object, reg_map, var_map);
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => {
            if let Some(new_name) = var_map.get(variable) {
                *variable = new_name.clone();
            }
            rename_expr(iterable, reg_map, var_map);
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::Delete { target, .. } => rename_expr(target, reg_map, var_map),
        Statement::Block(body) => {
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::For {
            init,
            condition,
            update,
            body,
        } => {
            if let Some(s) = init {
                rename_stmt(s, reg_map, var_map);
            }
            if let Some(e) = condition {
                rename_expr(e, reg_map, var_map);
            }
            if let Some(s) = update {
                rename_stmt(s, reg_map, var_map);
            }
            for s in body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => {
            rename_expr(discriminant, reg_map, var_map);
            for (val, body) in cases {
                rename_expr(val, reg_map, var_map);
                for s in body {
                    rename_stmt(s, reg_map, var_map);
                }
            }
            if let Some(body) = default {
                for s in body {
                    rename_stmt(s, reg_map, var_map);
                }
            }
        }
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => {
            for s in try_body {
                rename_stmt(s, reg_map, var_map);
            }
            if let Some(p) = catch_param {
                if let Some(new_p) = var_map.get(p) {
                    *catch_param = Some(new_p.clone());
                }
            }
            for s in catch_body {
                rename_stmt(s, reg_map, var_map);
            }
            for s in finally_body {
                rename_stmt(s, reg_map, var_map);
            }
        }
        Statement::Class {
            name,
            super_class,
            methods,
            ..
        } => {
            if let Some(new_name) = var_map.get(name) {
                *name = new_name.clone();
            }
            if let Some(sc) = super_class {
                rename_expr(sc, reg_map, var_map);
            }
            for m in methods {
                if let Some(b) = &mut m.body {
                    for s in b {
                        rename_stmt(s, reg_map, var_map);
                    }
                }
            }
        }
        _ => {}
    }
}

fn rename_target(
    target: &mut AssignTarget,
    reg_map: &std::collections::BTreeMap<u32, (String, u32)>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match target {
        AssignTarget::Binding(Binding::Register(r)) => {
            if let Some((name, i)) = reg_map.get(r) {
                *target = AssignTarget::Binding(Binding::Variable(param_var(*i, name)));
            }
        }
        AssignTarget::Binding(Binding::Variable(v)) => {
            if let Some(name) = var_map.get(v.as_str()) {
                v.name = name.clone();
            }
        }
        AssignTarget::Member { object, .. } => rename_expr(object, reg_map, var_map),
        AssignTarget::Index { object, key } => {
            rename_expr(object, reg_map, var_map);
            rename_expr(key, reg_map, var_map);
        }
        AssignTarget::DestructuringArray(targets) => {
            for entry in targets.iter_mut().flatten() {
                rename_target(&mut entry.0, reg_map, var_map);
                if let Some(def) = &mut entry.1 {
                    rename_expr(def, reg_map, var_map);
                }
            }
        }
        AssignTarget::DestructuringObject(targets) => {
            for (_, t, def) in targets {
                rename_target(t, reg_map, var_map);
                if let Some(d) = def {
                    rename_expr(d, reg_map, var_map);
                }
            }
        }
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            for entry in elements.iter_mut().flatten() {
                rename_target(&mut entry.0, reg_map, var_map);
                if let Some(def) = &mut entry.1 {
                    rename_expr(def, reg_map, var_map);
                }
            }
            rename_target(rest, reg_map, var_map);
        }
        AssignTarget::DestructuringObjectRest { properties, rest } => {
            for (_, t, def) in properties {
                rename_target(t, reg_map, var_map);
                if let Some(d) = def {
                    rename_expr(d, reg_map, var_map);
                }
            }
            rename_target(rest, reg_map, var_map);
        }
        AssignTarget::Rest(t) => rename_target(t, reg_map, var_map),
        _ => {}
    }
}

fn rename_expr(
    expr: &mut Expression,
    reg_map: &std::collections::BTreeMap<u32, (String, u32)>,
    var_map: &std::collections::HashMap<String, String>,
) {
    match expr {
        Expression::Value(Value::Binding(Binding::Register(r))) => {
            if let Some((name, i)) = reg_map.get(r) {
                *expr = Expression::Value(Value::Binding(Binding::Variable(param_var(*i, name))));
            }
        }
        Expression::Value(Value::Binding(Binding::Variable(v))) => {
            if let Some(name) = var_map.get(v.as_str()) {
                v.name = name.clone();
            }
        }
        Expression::Value(Value::Parameter(idx)) => {
            // This is direct Parameter access (argN)
            // We can rename it to Variable(name) if we have it
            if let Some(name) = var_map.get(&format!("arg{idx}")) {
                *expr = Expression::Value(Value::Binding(Binding::Variable(param_var(*idx, name))));
            }
        }
        Expression::Binary { left, right, .. } => {
            rename_expr(left, reg_map, var_map);
            rename_expr(right, reg_map, var_map);
        }
        Expression::Unary { operand, .. } => rename_expr(operand, reg_map, var_map),
        Expression::Member {
            object, property, ..
        } => {
            rename_expr(object, reg_map, var_map);
            if let PropertyKey::Computed(k) = property {
                rename_expr(k, reg_map, var_map);
            }
        }
        Expression::Call { callee, arguments } | Expression::New { callee, arguments } => {
            rename_expr(callee, reg_map, var_map);
            for a in arguments {
                rename_expr(a, reg_map, var_map);
            }
        }
        Expression::Object { properties } => {
            for p in properties {
                rename_expr(&mut p.value, reg_map, var_map);
                if let PropertyKey::Computed(k) = &mut p.key {
                    rename_expr(k, reg_map, var_map);
                }
            }
        }
        Expression::Array { elements } => {
            for e in elements.iter_mut().flatten() {
                rename_expr(e, reg_map, var_map);
            }
        }
        Expression::Assignment { target, value } => {
            crate::ir::for_each_target_expression_mut(target, &mut |e| {
                rename_expr(e, reg_map, var_map)
            });
            rename_expr(value, reg_map, var_map);
        }
        Expression::Spread(e) => rename_expr(e, reg_map, var_map),
        // No arm here left `function f(style) { return <A b={arg0.b} />; }`.
        Expression::JSXElement {
            tag,
            attributes,
            children,
        } => {
            rename_expr(tag, reg_map, var_map);
            for (_, v) in attributes {
                rename_expr(v, reg_map, var_map);
            }
            for c in children {
                rename_expr(c, reg_map, var_map);
            }
        }
        Expression::TemplateLiteral { expressions, .. } => {
            for e in expressions {
                rename_expr(e, reg_map, var_map);
            }
        }
        Expression::Yield { value, .. } | Expression::Await(value) => {
            rename_expr(value, reg_map, var_map)
        }
        Expression::Conditional {
            condition,
            then_expr,
            else_expr,
        } => {
            rename_expr(condition, reg_map, var_map);
            rename_expr(then_expr, reg_map, var_map);
            rename_expr(else_expr, reg_map, var_map);
        }
        _ => {}
    }
}

#[cfg(test)]
mod loop_head_tests {
    use super::rename_param_registers;

    // A named parameter read, and a register loaded from a parameter, carry
    // that parameter of that function (IDENTITY_PLAN.md A3 step 2.0b).
    #[test]
    fn a_named_parameter_carries_its_identity() {
        use crate::ir::{AssignTarget, Binding, Expression, LocalRef, Statement, Value};
        let mut stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(4)),
                value: Expression::Value(Value::Parameter(1)),
            },
            Statement::Return(Some(Expression::Value(Value::Parameter(0)))),
        ];
        super::rename_param_registers_of(&mut stmts, &[Some("a".into()), Some("b".into())], 9);
        let Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(b)),
            ..
        } = &stmts[0]
        else {
            panic!("{stmts:?}")
        };
        assert_eq!(
            (b.name.as_str(), b.local),
            ("b", Some(LocalRef::param(9, 1)))
        );
        let Statement::Return(Some(Expression::Value(Value::Binding(Binding::Variable(a))))) =
            &stmts[1]
        else {
            panic!("{stmts:?}")
        };
        assert_eq!(
            (a.name.as_str(), a.local),
            ("a", Some(LocalRef::param(9, 0)))
        );
        assert!(a.local.unwrap().is_param() && !a.local.unwrap().is_made());
    }

    // The names printed from a parameter inside JSX, where the walk used to stop.
    #[test]
    fn a_parameter_read_inside_jsx_takes_the_parameter_name() {
        use crate::ir::{Expression, Statement, Value};
        let arg = || Expression::Value(Value::Parameter(0));
        let mut stmts = vec![Statement::Return(Some(Expression::JSXElement {
            tag: Box::new(Expression::member(arg(), "Row")),
            attributes: vec![("a".to_string(), Expression::member(arg(), "a"))],
            children: vec![arg()],
        }))];
        rename_param_registers(&mut stmts, &[Some("props".into())]);
        let Statement::Return(Some(e)) = &stmts[0] else {
            panic!()
        };
        let text = crate::ir::expr::display::format_expr(e);
        assert!(!text.contains("arg0"), "{text}");
        assert!(text.contains("<props.Row"), "{text}");
    }
    use crate::ir::{AssignTarget, Binding, Expression, Statement, Value};

    #[test]
    fn a_for_in_over_a_parameter_takes_the_parameter_name() {
        // r1 = arg1; for (k in r1) { r1[k] }  with parameter 1 named `source`
        let mut stmts = vec![
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(1)),
                value: Expression::Value(Value::Parameter(1)),
            },
            Statement::ForIn {
                variable: "k".into(),
                object: Expression::Value(Value::Binding(Binding::Register(1))),
                body: vec![Statement::Expr(Expression::Value(Value::Binding(
                    Binding::Register(1),
                )))],
            },
        ];
        rename_param_registers(&mut stmts, &[None, Some("source".into())]);
        let text = format!("{:?}", stmts[1]);
        assert!(text.contains("source"), "loop head renamed: {text}");
        assert!(!text.contains("Register(1)"), "loop body renamed: {text}");
    }
}
