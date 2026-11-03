use crate::analysis::rename_registers;
use crate::ir::{
    expr_uses_register, AssignTarget, BinaryOp, Binding, Constant, Expression, PropertyKey,
    Statement, UnaryOp, Value,
};
use std::collections::BTreeMap;

// Detect for-in loop patterns and rebuild them as `for (key in object)`.
//
// Hermes lowers `for (k in o)` to the property-enumeration opcodes
// (GetPNameList / GetNextPName). After IR build + structure recovery the shape is:
//
//   keys = Object.keys(o)                 // GetPNameList (our lowering)
//   if (keys === undefined) {             // JmpUndefined: no enumerable props
//   } else {
//     cur = keys[idx]                     // first GetNextPName (peeled to header)
//     while (!(cur === undefined)) {      // JmpUndefined: enumeration exhausted
//       <body using cur>                  // the back-edge re-runs GetNextPName
//     }
//   }
//
// The internal index/advance of GetNextPName has no source-level form, so we
// match this whole shape and emit `for (cur in o) { <body> }`, dropping the
// enumeration plumbing (the per-iteration fetch is implied by for-in semantics).
pub fn detect_for_in_loops(stmts: Vec<Statement>) -> Vec<Statement> {
    let stmts = recurse(stmts);
    let mut result: Vec<Statement> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        if i + 1 < stmts.len() {
            if let Some(emitted) = try_match_for_in(&stmts[i], &stmts[i + 1], &stmts[i + 2..]) {
                result.extend(emitted);
                i += 2;
                continue;
            }
        }
        result.push(stmts[i].clone());
        i += 1;
    }
    mark_unrecovered(&mut result);
    result
}

// The callee an enumeration the matcher could not rebuild prints with. It is not
// `Object.keys`: that skips inherited keys and runs in another order, and the
// lowering's `Object.keys(o)` lost its argument to the receiver strip and threw.
pub const UNRECOVERED_FOR_IN: &str = "__forInKeys";

// Renames what is left of `GetPNameList` after matching, so an unrecovered loop
// reads as one.
fn mark_unrecovered(stmts: &mut [Statement]) {
    use crate::ir::MutVisitor;
    struct Mark;
    impl MutVisitor for Mark {
        fn visit_expression(&mut self, e: &mut Expression) {
            self.walk_expression(e);
            if let Expression::Call { callee, arguments } = e {
                if arguments.len() == 1 && is_object_keys_callee(callee) {
                    **callee = Expression::Value(Value::Binding(Binding::Variable(
                        UNRECOVERED_FOR_IN.into(),
                    )));
                }
            }
        }
    }
    for s in stmts.iter_mut() {
        Mark.visit_statement(s);
    }
}

// Recurse into nested blocks first so inner for-in loops are rebuilt too.
fn recurse(stmts: Vec<Statement>) -> Vec<Statement> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Statement::While { condition, body } => Statement::While {
                condition,
                body: detect_for_in_loops(body),
            },
            Statement::DoWhile { body, condition } => Statement::DoWhile {
                body: detect_for_in_loops(body),
                condition,
            },
            Statement::If {
                condition,
                then_body,
                else_body,
            } => Statement::If {
                condition,
                then_body: detect_for_in_loops(then_body),
                else_body: detect_for_in_loops(else_body),
            },
            Statement::For {
                init,
                condition,
                update,
                body,
            } => Statement::For {
                init,
                condition,
                update,
                body: detect_for_in_loops(body),
            },
            Statement::ForIn {
                variable,
                object,
                body,
            } => Statement::ForIn {
                variable,
                object,
                body: detect_for_in_loops(body),
            },
            Statement::ForOf {
                variable,
                iterable,
                body,
            } => Statement::ForOf {
                variable,
                iterable,
                body: detect_for_in_loops(body),
            },
            Statement::Block(inner) => Statement::Block(detect_for_in_loops(inner)),
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => Statement::TryCatch {
                try_body: detect_for_in_loops(try_body),
                catch_param,
                catch_body: detect_for_in_loops(catch_body),
                finally_body: detect_for_in_loops(finally_body),
            },
            other => other,
        })
        .collect()
}

// Match `keys = Object.keys(obj)` followed by the enumeration `if`. `rest` is what
// follows the `if`. Returns the statements to put in place of the two.
fn try_match_for_in(
    keys_stmt: &Statement,
    if_stmt: &Statement,
    rest: &[Statement],
) -> Option<Vec<Statement>> {
    // [0] keys_reg = Object.keys(obj)
    let (keys_reg, obj_expr) = match keys_stmt {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(r)),
            value,
        } => (*r, is_object_keys_call(value)?),
        _ => return None,
    };

    // [1] if (keys_reg === undefined) { <exit> } else { <loop> }. The exit is empty, or
    // a copy of what follows the `if`, where the loop's own exit goes (an inner loop's
    // `continue` of the outer one).
    let else_body = match if_stmt {
        Statement::If {
            condition,
            then_body,
            else_body,
        } if is_undefined_check_eq(condition, keys_reg)
            && (then_body.is_empty() || same_code(then_body, rest)) =>
        {
            else_body
        }
        _ => return None,
    };

    // else_body: [header]; cur_reg = keys_reg[idx]; [label]; while (!(cur_reg === undefined)) { body }
    // The header is the loop header's own statements (copies of variables the body writes),
    // hoisted with the first fetch. They run before every fetch, the last one included.
    let mut idx = 0;
    let mut header: Vec<Statement> = Vec::new();
    let cur_reg = loop {
        match else_body.get(idx)? {
            Statement::Assign {
                target: AssignTarget::Binding(Binding::Register(r)),
                value,
            } if is_index_into(value, keys_reg) => {
                let r = *r;
                idx += 1;
                break r;
            }
            Statement::Comment(_) => idx += 1,
            s @ Statement::Assign {
                target: AssignTarget::Binding(_),
                value,
            } if !value.has_side_effects() => {
                header.push(s.clone());
                idx += 1;
            }
            _ => return None,
        }
    };
    let touches_plumbing = |s: &Statement| {
        let Statement::Assign {
            target: AssignTarget::Binding(t),
            value,
        } = s
        else {
            return true;
        };
        matches!(t, Binding::Register(r) if *r == cur_reg || *r == keys_reg)
            || expr_uses_register(value, cur_reg)
            || expr_uses_register(value, keys_reg)
    };
    if header.iter().any(touches_plumbing) {
        return None;
    }

    // Skip a label comment between the peeled fetch and the while.
    let mut label = None;
    while let Some(Statement::Comment(text)) = else_body.get(idx) {
        if crate::transforms::codegen::loop_label(text).is_some() {
            label = else_body.get(idx).cloned();
        }
        idx += 1;
    }

    let body = match else_body.get(idx)? {
        Statement::While { condition, body } if is_undefined_check_neq(condition, cur_reg) => body,
        _ => return None,
    };
    // The loop must be the last meaningful statement of the else branch, or be
    // followed by its exit, the same code the null path runs.
    let exit = match if_stmt {
        Statement::If { then_body, .. } => then_body.as_slice(),
        _ => return None,
    };
    if !same_code(&else_body[idx + 1..], &[]) && !same_code(&else_body[idx + 1..], exit) {
        return None;
    }

    // Strip the enumeration back-edge fetch (`cur = keys[idx]`) and the trailing
    // `// continue`, then bind the loop variable.
    let var_name = format!("key{cur_reg}");
    let cleaned: Vec<Statement> = body
        .iter()
        .filter(|s| !matches!(s, Statement::Comment(c) if c == "continue"))
        .filter(|s| !is_assign_of_index(s, cur_reg, keys_reg))
        .cloned()
        .collect();
    let mut map = BTreeMap::new();
    map.insert(cur_reg, var_name.clone());
    let cleaned = rename_registers(cleaned, &map);

    let mut out = Vec::new();
    let cleaned = if header.is_empty() {
        cleaned
    } else {
        // `GetPNameList` skips the loop, header and all, for null and undefined only;
        // the test reads the object twice, so it must be a plain value.
        if !matches!(obj_expr, Expression::Value(_)) {
            return None;
        }
        out.push(Statement::If {
            condition: Expression::binary(
                BinaryOp::Neq,
                obj_expr.clone(),
                Expression::Value(Value::Constant(Constant::Null)),
            ),
            then_body: header.clone(),
            else_body: vec![],
        });
        let label_name = label
            .as_ref()
            .and_then(|s| match s {
                Statement::Comment(t) => crate::transforms::codegen::loop_label(t),
                _ => None,
            })
            .map(str::to_string);
        let mut body = with_header_at_back_edges(cleaned, &header, label_name.as_deref(), false);
        if !always_jumps(&body) {
            body.extend(header.iter().cloned());
        }
        body
    };
    // The loop keeps its label: a `continue label1` inside names it.
    out.extend(label);
    out.push(Statement::ForIn {
        variable: var_name,
        object: obj_expr,
        body: detect_for_in_loops(cleaned),
    });
    Some(out)
}

// The two lists run the same code, comments aside.
fn same_code(a: &[Statement], b: &[Statement]) -> bool {
    let code = |s: &[Statement]| -> Vec<Statement> {
        s.iter()
            .filter(|s| !matches!(s, Statement::Comment(_)))
            .cloned()
            .collect()
    };
    code(a) == code(b)
}

// Puts the header before every `continue` of this loop: a `continue` goes back to
// the header, which the rebuilt loop no longer has. Inside a nested loop only a
// `continue` naming this loop's label is this loop's.
fn with_header_at_back_edges(
    stmts: Vec<Statement>,
    header: &[Statement],
    label: Option<&str>,
    nested: bool,
) -> Vec<Statement> {
    let go = |b: Vec<Statement>, nested: bool| with_header_at_back_edges(b, header, label, nested);
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            Statement::Continue(ref l)
                if (l.is_none() && !nested) || (l.is_some() && l.as_deref() == label) =>
            {
                out.extend(header.iter().cloned());
                out.push(stmt);
            }
            Statement::If {
                condition,
                then_body,
                else_body,
            } => out.push(Statement::If {
                condition,
                then_body: go(then_body, nested),
                else_body: go(else_body, nested),
            }),
            Statement::Block(b) => out.push(Statement::Block(go(b, nested))),
            Statement::TryCatch {
                try_body,
                catch_param,
                catch_body,
                finally_body,
            } => out.push(Statement::TryCatch {
                try_body: go(try_body, nested),
                catch_param,
                catch_body: go(catch_body, nested),
                finally_body: go(finally_body, nested),
            }),
            Statement::Switch {
                discriminant,
                cases,
                default,
            } => out.push(Statement::Switch {
                discriminant,
                cases: cases.into_iter().map(|(c, b)| (c, go(b, nested))).collect(),
                default: default.map(|b| go(b, nested)),
            }),
            Statement::While { condition, body } => out.push(Statement::While {
                condition,
                body: go(body, true),
            }),
            Statement::DoWhile { body, condition } => out.push(Statement::DoWhile {
                body: go(body, true),
                condition,
            }),
            Statement::For {
                init,
                condition,
                update,
                body,
            } => out.push(Statement::For {
                init,
                condition,
                update,
                body: go(body, true),
            }),
            Statement::ForIn {
                variable,
                object,
                body,
            } => out.push(Statement::ForIn {
                variable,
                object,
                body: go(body, true),
            }),
            Statement::ForOf {
                variable,
                iterable,
                body,
            } => out.push(Statement::ForOf {
                variable,
                iterable,
                body: go(body, true),
            }),
            other => out.push(other),
        }
        // What follows a statement that always leaves never runs, and with the header
        // copied in front of its `continue` it would print twice.
        if always_jumps(&out) {
            break;
        }
    }
    out
}

// The list never runs off its end: its last statement leaves, or branches that all do.
fn always_jumps(stmts: &[Statement]) -> bool {
    match stmts
        .iter()
        .rev()
        .find(|s| !matches!(s, Statement::Comment(_)))
    {
        Some(
            Statement::Continue(_)
            | Statement::Break(_)
            | Statement::Return(_)
            | Statement::Throw(_),
        ) => true,
        Some(Statement::If {
            then_body,
            else_body,
            ..
        }) => !else_body.is_empty() && always_jumps(then_body) && always_jumps(else_body),
        _ => false,
    }
}

// `Object.keys(obj)` -> Some(obj)
// The lowering's call has one argument; a call written in the source still has its
// receiver at this stage, so it has two.
fn is_object_keys_call(expr: &Expression) -> Option<Expression> {
    match expr {
        Expression::Call { callee, arguments }
            if arguments.len() == 1 && is_object_keys_callee(callee) =>
        {
            Some(arguments[0].clone())
        }
        _ => None,
    }
}

fn is_object_keys_callee(callee: &Expression) -> bool {
    matches!(
        callee,
        Expression::Member { object, property: PropertyKey::Ident(prop), .. }
            if prop == "keys"
                && matches!(object.as_ref(),
                    Expression::Value(Value::Binding(Binding::Variable(name))) if name == "Object")
    )
}

// `reg[<anything>]`, the GetNextPName lowering (property at the internal index).
fn is_index_into(expr: &Expression, base_reg: u32) -> bool {
    if let Expression::Member {
        object,
        property: PropertyKey::Computed(_),
        ..
    } = expr
    {
        if let Expression::Value(Value::Binding(Binding::Register(r))) = object.as_ref() {
            return *r == base_reg;
        }
    }
    false
}

fn is_assign_of_index(stmt: &Statement, dst_reg: u32, base_reg: u32) -> bool {
    if let Statement::Assign {
        target: AssignTarget::Binding(Binding::Register(r)),
        value,
    } = stmt
    {
        return *r == dst_reg && is_index_into(value, base_reg);
    }
    false
}

// `reg === undefined`
fn is_undefined_check_eq(expr: &Expression, reg: u32) -> bool {
    if let Expression::Binary {
        op: BinaryOp::StrictEq,
        left,
        right,
    } = expr
    {
        return touches_undefined(left, right, reg);
    }
    false
}

// `reg !== undefined` or `!(reg === undefined)`
fn is_undefined_check_neq(expr: &Expression, reg: u32) -> bool {
    match expr {
        Expression::Binary {
            op: BinaryOp::StrictNeq,
            left,
            right,
        } => touches_undefined(left, right, reg),
        Expression::Unary {
            op: UnaryOp::Not,
            operand,
        } => is_undefined_check_eq(operand, reg),
        _ => false,
    }
}

fn touches_undefined(left: &Expression, right: &Expression, reg: u32) -> bool {
    let is_reg = |e: &Expression| matches!(e, Expression::Value(Value::Binding(Binding::Register(r))) if *r == reg);
    let is_undef =
        |e: &Expression| matches!(e, Expression::Value(Value::Constant(Constant::Undefined)));
    (is_reg(left) && is_undef(right)) || (is_reg(right) && is_undef(left))
}
