// Reconstruct ES6 generators for HBC >=97.
//
// HBC v97 DELETED the dedicated generator opcodes (StartGenerator / SaveGenerator
// / ResumeGenerator / CompleteGenerator). The frontend now desugars `function*`
// into a plain-opcode state machine: a switch over two env slots (a `status` and
// a resume `label`), wrapped in guards. This pass recognizes that exact shape and
// rebuilds the flat `yield` body.
//
// Shape produced by v98 (post structure-recovery + closure resolution):
//
//   if (closure_0 === 2) { closure_0 = 3; HermesBuiltin.throwTypeError(); }   // executing guard
//   else if (tmp === 3) { ...completed-resume handling... }                   // completed guard
//   else {
//     try {
//       closure_0 = 2;                 // status = executing
//       tmp = closure_1;               // label copy (optional)
//       if (0 === closure_1) { <case 0> }
//       else if (1 === tmp) { <case 1> }
//       else if (2 === tmp) { <case 2> }
//       else { <done> }
//     } catch { ... }
//   }
//
// where each `<case N>` is:
//
//   if (arg0 === 1) { throw arg1; }                         // .throw(v)
//   else if (arg0 === 2) { return {value: arg1, done:true}; }  // .return(v)
//   else { <pre-code>; closure_1 = N+1; closure_0 = 1; return {value: V, done:false}; }  // yield V
//
// We extract `V` per label (in order) and emit `yield V`; the terminal case
// (`return {value: undefined, done: true}`) ends the body. Conservative: any
// deviation makes the whole pass bail and return the input unchanged, so an
// unrecognized generator keeps today's (raw) output rather than wrong code.

use crate::ir::{AssignTarget, BinaryOp, Constant, Expression, PropertyKey, Statement, Value};

pub fn reconstruct_generator_v98(body: Vec<Statement>) -> Vec<Statement> {
    try_reconstruct(&body).unwrap_or(body)
}

// A parsed state-machine case: the value the case resumes with (`x = <resume>`),
// the real code before the suspend point, the yielded/returned value and whether
// this is the terminal (done) case.
struct ParsedCase {
    resume_binding: Option<AssignTarget>,
    pre: Vec<Statement>,
    value: Expression,
    done: bool,
}

fn try_reconstruct(body: &[Statement]) -> Option<Vec<Statement>> {
    let dispatch = find_label_dispatch(body)?;
    let cases = collect_label_cases(dispatch)?;
    // A real state machine has at least one yield label plus the terminal case.
    if cases.len() < 2 {
        return None;
    }

    // Variables compared against an integer literal anywhere in the machine are
    // the status / label slots and their copies (`c0`, `c1`, `tmp3`...). Their
    // assignments are bookkeeping and get dropped from the reconstructed body.
    let state_vars = collect_state_vars(body);

    let parsed: Vec<ParsedCase> = cases
        .iter()
        .map(|(_, b)| parse_case(b, &state_vars))
        .collect::<Option<_>>()?;

    // The last case must be the terminal (done) one; every earlier case suspends.
    let (last, rest) = parsed.split_last()?;
    if !last.done || rest.iter().any(|c| c.done) {
        return None;
    }

    // Thread the resume protocol: case i suspends yielding `value_i`, and case i+1
    // resumes binding that yield's result. So `case[i+1].binding = yield value_i`.
    let mut out = Vec::new();
    for (i, case) in parsed.iter().enumerate() {
        if i > 0 {
            let prev_value = parsed[i - 1].value.clone();
            let yielded = Expression::Yield {
                value: Box::new(prev_value),
                delegate: false,
            };
            match &case.resume_binding {
                Some(target) => out.push(Statement::Assign {
                    target: target.clone(),
                    value: yielded,
                }),
                None => out.push(Statement::Expr(yielded)),
            }
        }
        out.extend(case.pre.iter().cloned());
    }
    // Terminal return, unless it returns undefined (a bare `return`).
    if !is_undefined(&last.value) {
        out.push(Statement::Return(Some(last.value.clone())));
    }

    // Sanity: a reconstructed generator must contain at least one yield.
    if !out.iter().any(stmt_has_yield_deep) {
        return None;
    }
    Some(out)
}

// Collect every variable name that is compared against an integer literal; these
// are the state-machine status / label slots, never user data.
fn collect_state_vars(body: &[Statement]) -> std::collections::HashSet<String> {
    use crate::ir::Visitor;
    struct C(std::collections::HashSet<String>);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = e {
                for (a, b) in [(left, right), (right, left)] {
                    if int_const(a).is_some() {
                        if let Expression::Value(Value::Variable(n)) = b.as_ref() {
                            self.0.insert(n.clone());
                        }
                    }
                }
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(std::collections::HashSet::new());
    for s in body {
        c.visit_statement(s);
    }
    c.0
}

// Parse one case body into (resume binding, real pre-code, yielded value, done).
fn parse_case(case_body: &[Statement], state_vars: &std::collections::HashSet<String>) -> Option<ParsedCase> {
    let real = strip_arg_protocol(case_body);

    // A leading `x = <resume param>` binds the value the generator was resumed
    // with (the result of the previous yield / await).
    let mut resume_binding = None;
    let mut idx = 0;
    if let Some(Statement::Assign { target, value }) = real.first() {
        if is_resume_param(value) {
            resume_binding = Some(target.clone());
            idx = 1;
        }
    }

    let mut pre = Vec::new();
    let mut i = idx;
    while i < real.len() {
        // The suspend point is the result-object return; everything up to it is
        // real code (minus state bookkeeping and dead inits).
        if let Some((value, done, consumed)) = parse_result_return(&real[i..]) {
            if i + consumed != real.len() {
                return None; // unexpected trailing code after the return
            }
            return Some(ParsedCase { resume_binding, pre, value, done });
        }
        let s = &real[i];
        if !is_bookkeeping(s, state_vars) {
            pre.push(s.clone());
        }
        i += 1;
    }
    None
}

// Drop state-slot writes, dead `x = undefined` inits and label copies.
fn is_bookkeeping(s: &Statement, state_vars: &std::collections::HashSet<String>) -> bool {
    if let Statement::Assign { target: AssignTarget::Variable(n), value } = s {
        if state_vars.contains(n) {
            return true;
        }
        if matches!(value, Expression::Value(Value::Constant(Constant::Undefined))) {
            return true;
        }
        if let Expression::Value(Value::Variable(src)) = value {
            if state_vars.contains(src) {
                return true;
            }
        }
    }
    false
}

// Recognise the two result-object shapes the frontend emits:
//   return { value: V, done: D }
//   obj = { value: <ignored>, done: D }; obj[0] = V; return obj
// Returns (value, done, statements_consumed).
fn parse_result_return(stmts: &[Statement]) -> Option<(Expression, bool, usize)> {
    // Direct literal return.
    if let Statement::Return(Some(expr)) = &stmts[0] {
        if let Some((v, d)) = parse_result_object(expr) {
            return Some((v, d, 1));
        }
    }
    // Incremental object build then return.
    if stmts.len() >= 3 {
        if let (
            Statement::Assign { target: AssignTarget::Variable(o1), value: Expression::Object { properties } },
            Statement::Assign { target: AssignTarget::Index { object, key }, value: real_value },
            Statement::Return(Some(Expression::Value(Value::Variable(o3)))),
        ) = (&stmts[0], &stmts[1], &stmts[2])
        {
            let obj_is = |e: &Expression, name: &str| matches!(e, Expression::Value(Value::Variable(v)) if v == name);
            let key_is_zero = matches!(key, Expression::Value(Value::Constant(Constant::Integer(0))));
            if o1 == o3 && obj_is(object, o1) && key_is_zero {
                let done = properties.iter().find_map(|p| match &p.key {
                    PropertyKey::Ident(k) | PropertyKey::String(k) if k == "done" => Some(is_truthy(&p.value)),
                    _ => None,
                })?;
                return Some((real_value.clone(), done, 3));
            }
        }
    }
    None
}

fn is_resume_param(e: &Expression) -> bool {
    matches!(e, Expression::Value(Value::Parameter(1)))
}

fn is_undefined(e: &Expression) -> bool {
    matches!(e, Expression::Value(Value::Constant(Constant::Undefined)))
}

fn stmt_has_yield_deep(s: &Statement) -> bool {
    use crate::ir::Visitor;
    struct C(bool);
    impl<'b> Visitor<'b> for C {
        fn visit_expression(&mut self, e: &'b Expression) {
            if matches!(e, Expression::Yield { .. }) {
                self.0 = true;
            }
            self.walk_expression(e);
        }
    }
    let mut c = C(false);
    c.visit_statement(s);
    c.0
}

// --- locating the label dispatch ---

// Find the TryCatch try-body (search through the status-guard if/else nest) then
// the label-dispatch `if` inside it.
fn find_label_dispatch(body: &[Statement]) -> Option<&Statement> {
    let try_body = find_generator_try(body)?;
    try_body.iter().find(|s| is_label_dispatch_if(s))
}

fn find_generator_try(body: &[Statement]) -> Option<&Vec<Statement>> {
    for s in body {
        match s {
            Statement::TryCatch { try_body, .. } => return Some(try_body),
            Statement::If { then_body, else_body, .. } => {
                if let Some(t) = find_generator_try(then_body) {
                    return Some(t);
                }
                if let Some(t) = find_generator_try(else_body) {
                    return Some(t);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_label_dispatch_if(s: &Statement) -> bool {
    matches!(s, Statement::If { condition, .. } if label_of_condition(condition).is_some())
}

// `<int> === <var>` or `<var> === <int>` → the integer label.
fn label_of_condition(cond: &Expression) -> Option<i32> {
    if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = cond {
        if let (Some(k), true) = (int_const(left), is_var(right)) {
            return Some(k);
        }
        if let (true, Some(k)) = (is_var(left), int_const(right)) {
            return Some(k);
        }
    }
    None
}

fn int_const(e: &Expression) -> Option<i32> {
    match e {
        Expression::Value(Value::Constant(Constant::Integer(n))) => Some(*n),
        _ => None,
    }
}

fn is_var(e: &Expression) -> bool {
    // The status/label live in the generator's own environment slots, which at
    // this stage are `ClosureVar`; a `tmp` copy of the label is a plain Variable.
    matches!(
        e,
        Expression::Value(Value::Variable(_)) | Expression::Value(Value::ClosureVar { .. })
    )
}

// Walk the `if (0===l) {..} else if (1===l) {..} else {done}` chain into
// (label, body) pairs, in source order. The trailing non-label `else` is the
// terminal/done case (given a sentinel label).
fn collect_label_cases(mut s: &Statement) -> Option<Vec<(i32, Vec<Statement>)>> {
    let mut cases = Vec::new();
    loop {
        let Statement::If { condition, then_body, else_body } = s else {
            break;
        };
        let Some(k) = label_of_condition(condition) else {
            break;
        };
        cases.push((k, then_body.clone()));
        if else_body.len() == 1 {
            // Either the next label `if`, or a single-statement done case.
            if is_label_dispatch_if(&else_body[0]) {
                s = &else_body[0];
                continue;
            }
            cases.push((i32::MAX, else_body.clone()));
            break;
        } else if !else_body.is_empty() {
            cases.push((i32::MAX, else_body.clone())); // multi-statement done case
            break;
        } else {
            break;
        }
    }
    if cases.is_empty() {
        None
    } else {
        Some(cases)
    }
}

// --- per-case extraction ---

// Navigate past the `if (arg0===1) {throw} else if (arg0===2) {return} else {..}`
// resume-protocol wrapper to the real (next) branch.
fn strip_arg_protocol(body: &[Statement]) -> &[Statement] {
    if body.len() == 1 {
        if let Statement::If { condition, else_body, .. } = &body[0] {
            if is_resume_protocol_cond(condition) {
                return strip_arg_protocol(else_body);
            }
        }
    }
    body
}

// `arg0 === 1` or `arg0 === 2` (resume method check; arg0 is Parameter(0)).
fn is_resume_protocol_cond(cond: &Expression) -> bool {
    if let Expression::Binary { op: BinaryOp::StrictEq, left, right } = cond {
        let is_p0 = |e: &Expression| matches!(e, Expression::Value(Value::Parameter(0)));
        let is_12 = |e: &Expression| matches!(int_const(e), Some(1) | Some(2));
        return (is_p0(left) && is_12(right)) || (is_p0(right) && is_12(left));
    }
    false
}

// Extract (value, done) from an `{value: V, done: D}` object literal.
fn parse_result_object(expr: &Expression) -> Option<(Expression, bool)> {
    let Expression::Object { properties } = expr else {
        return None;
    };
    let mut value = None;
    let mut done = None;
    for p in properties {
        let key = match &p.key {
            PropertyKey::Ident(k) | PropertyKey::String(k) => k.as_str(),
            _ => continue,
        };
        match key {
            "value" => value = Some(p.value.clone()),
            "done" => done = Some(is_truthy(&p.value)),
            _ => {}
        }
    }
    Some((value?, done?))
}

fn is_truthy(e: &Expression) -> bool {
    match e {
        Expression::Value(Value::Constant(Constant::Bool(b))) => *b,
        Expression::Value(Value::Constant(Constant::Integer(n))) => *n != 0,
        _ => false,
    }
}
