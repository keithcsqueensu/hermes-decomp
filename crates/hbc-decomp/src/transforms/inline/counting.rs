// Variable definition/use counting and substitution functions for named variable inlining.

use crate::ir::{AssignTarget, Binding, Expression, MutVisitor, Statement, Value, Var, Visitor};
use std::collections::BTreeMap;

// --- Counting ---

pub(super) fn count_var_defs_uses(
    stmts: &[Statement],
    defs: &mut BTreeMap<String, usize>,
    uses: &mut BTreeMap<String, usize>,
) {
    let mut counter = VarCounter { defs, uses };
    for stmt in stmts {
        counter.visit_statement(stmt);
    }
}

struct VarCounter<'c> {
    defs: &'c mut BTreeMap<String, usize>,
    uses: &'c mut BTreeMap<String, usize>,
}

impl<'a, 'c> Visitor<'a> for VarCounter<'c> {
    fn visit_statement(&mut self, stmt: &'a Statement) {
        match stmt {
            Statement::Assign { target, value } => {
                if let AssignTarget::Binding(Binding::Variable(name)) = target {
                    *self.defs.entry(name.name.clone()).or_insert(0) += 1;
                }
                self.visit_assign_target(target);
                self.visit_expression(value);
            }
            Statement::Let { name, value, .. } => {
                *self.defs.entry(name.name.clone()).or_insert(0) += 1;
                self.visit_expression(value);
            }
            _ => self.walk_statement(stmt),
        }
    }

    fn visit_expression(&mut self, expr: &'a Expression) {
        if let Expression::Value(Value::Binding(Binding::Variable(name))) = expr {
            *self.uses.entry(name.name.clone()).or_insert(0) += 1;
        }
        // Don't recurse into function bodies
        if matches!(expr, Expression::Function { .. }) {
            return;
        }
        self.walk_expression(expr);
    }
}

// --- Substitution ---

pub(super) fn substitute_vars_in_expr<K: std::borrow::Borrow<str> + Ord>(
    expr: &mut Expression,
    pending: &BTreeMap<K, Expression>,
) {
    let mut substitutor = VarSubstitutor { pending };
    substitutor.visit_expression(expr);
}

struct VarSubstitutor<'p, K> {
    pending: &'p BTreeMap<K, Expression>,
}

impl<'p, K: std::borrow::Borrow<str> + Ord> MutVisitor for VarSubstitutor<'p, K> {
    fn visit_expression(&mut self, expr: &mut Expression) {
        self.walk_expression(expr);
        if let Expression::Value(Value::Binding(Binding::Variable(name))) = expr {
            if let Some(replacement) = self.pending.get(name.as_str()) {
                *expr = replacement.clone();
            }
        }
    }
}

// --- Apply pending to statement ---

// The names a statement uses are taken before substitution: after it, the name of
// a value substituted in is gone, and that value stayed pending and was written out
// again at the end of the block, a call printed twice (`obj.value = f(x); const fResult
// = f(x);`).
pub(super) fn apply_pending_to_stmt(stmt: &mut Statement, pending: &mut BTreeMap<Var, Expression>) {
    let in_place = |e: &mut Expression, pending: &mut BTreeMap<Var, Expression>| {
        let before = e.clone();
        substitute_vars_in_expr(e, pending);
        remove_used_from_pending(&before, pending);
    };
    match stmt {
        Statement::Assign { target, value } => {
            let target_before = target.clone();
            apply_pending_to_target(target, pending);
            in_place(value, pending);
            remove_used_from_target(&target_before, pending);
        }
        Statement::Let { value, .. } => in_place(value, pending),
        Statement::Class {
            super_class: Some(sc),
            ..
        } => in_place(sc, pending),
        Statement::Expr(e) | Statement::Return(Some(e)) | Statement::Throw(e) => {
            in_place(e, pending)
        }
        Statement::Delete { target, .. } => in_place(target, pending),
        _ => {
            // For structured statements (if/while/for/etc.), flush all pending first
            // because control flow makes ordering complex
        }
    }
}

// Every expression inside the target, a destructuring pattern's included
// (`{ a: x.b } = v`): a pending definition left out here was flushed after the
// pattern that read it.
fn apply_pending_to_target(target: &mut AssignTarget, pending: &mut BTreeMap<Var, Expression>) {
    crate::ir::for_each_target_expression_mut(target, &mut |e| substitute_vars_in_expr(e, pending));
}

pub(super) fn flush_pending(pending: &mut BTreeMap<Var, Expression>, result: &mut Vec<Statement>) {
    let items = std::mem::take(pending);
    for (name, value) in items {
        result.push(Statement::Assign {
            target: AssignTarget::Binding(Binding::Variable(name)),
            value,
        });
    }
}

// Collect all variable names referenced in an expression using the Visitor trait,
// then remove them from the pending map.
pub(super) fn remove_used_from_pending(expr: &Expression, pending: &mut BTreeMap<Var, Expression>) {
    struct UsedVarCollector {
        names: Vec<String>,
    }
    impl<'a> Visitor<'a> for UsedVarCollector {
        fn visit_expression(&mut self, expr: &'a Expression) {
            if let Expression::Value(Value::Binding(Binding::Variable(name))) = expr {
                self.names.push(name.name.clone());
            }
            self.walk_expression(expr);
        }
    }
    let mut collector = UsedVarCollector { names: Vec::new() };
    collector.visit_expression(expr);
    for name in collector.names {
        pending.remove(name.as_str());
    }
}

fn remove_used_from_target(target: &AssignTarget, pending: &mut BTreeMap<Var, Expression>) {
    crate::ir::for_each_target_expression(target, &mut |e| remove_used_from_pending(e, pending));
}
