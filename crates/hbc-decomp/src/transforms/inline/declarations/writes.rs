use crate::ir::{AssignTarget, Binding, Statement};
use std::collections::BTreeMap;

pub(super) fn count_writes(
    stmts: &[Statement],
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    for stmt in stmts {
        count_writes_stmt(stmt, writes, let_declared);
    }
}

fn pattern_writes(target: &AssignTarget, writes: &mut BTreeMap<String, usize>) {
    match target {
        AssignTarget::Binding(Binding::Variable(name)) => {
            *writes.entry(name.name.clone()).or_insert(0) += 1;
        }
        AssignTarget::DestructuringObject(props) => {
            for (_, t, _) in props {
                pattern_writes(t, writes);
            }
        }
        AssignTarget::DestructuringArray(elems) => {
            for (t, _) in elems.iter().flatten() {
                pattern_writes(t, writes);
            }
        }
        AssignTarget::DestructuringArrayRest { elements, rest } => {
            for (t, _) in elements.iter().flatten() {
                pattern_writes(t, writes);
            }
            pattern_writes(rest, writes);
        }
        AssignTarget::Rest(inner) => pattern_writes(inner, writes),
        _ => {}
    }
}

fn count_writes_stmt(
    stmt: &Statement,
    writes: &mut BTreeMap<String, usize>,
    let_declared: &mut std::collections::HashSet<String>,
) {
    match stmt {
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(name)),
            ..
        } => {
            *writes.entry(name.name.clone()).or_insert(0) += 1;
        }
        Statement::Assign {
            target: AssignTarget::Binding(Binding::Register(r)),
            ..
        } => {
            *writes.entry(format!("r{r}")).or_insert(0) += 1;
        }
        // A pattern writes every name in it (`({ user_id } = arg0)` in a
        // closure assigns the enclosing `user_id`).
        Statement::Assign {
            target:
                target @ (AssignTarget::DestructuringObject(_)
                | AssignTarget::DestructuringArray(_)
                | AssignTarget::DestructuringArrayRest { .. }),
            ..
        } => pattern_writes(target, writes),
        Statement::Let { name, .. } => {
            let_declared.insert(name.name.clone());
        }
        Statement::If {
            condition: _,
            then_body,
            else_body,
        } => {
            count_writes(then_body, writes, let_declared);
            count_writes(else_body, writes, let_declared);
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::For { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => {
            // Variables assigned inside loops are always multi-write
            let mut inner_writes: BTreeMap<String, usize> = BTreeMap::new();
            let mut inner_lets = std::collections::HashSet::new();
            count_writes(body, &mut inner_writes, &mut inner_lets);
            for (name, count) in inner_writes {
                // Treat loop body assignments as at least 2 writes (since loops repeat)
                *writes.entry(name).or_insert(0) += count.max(2);
            }
            let_declared.extend(inner_lets);
        }
        Statement::Block(inner) => count_writes(inner, writes, let_declared),
        Statement::TryCatch {
            try_body,
            catch_body,
            finally_body,
            ..
        } => {
            count_writes(try_body, writes, let_declared);
            count_writes(catch_body, writes, let_declared);
            count_writes(finally_body, writes, let_declared);
        }
        Statement::Switch { cases, default, .. } => {
            for (_, body) in cases {
                count_writes(body, writes, let_declared);
            }
            if let Some(d) = default {
                count_writes(d, writes, let_declared);
            }
        }
        _ => {}
    }
}
