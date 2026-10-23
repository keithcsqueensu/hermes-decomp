use super::state::VariableNamer;
use crate::ir::{AssignTarget, Binding, Expression, MutVisitor, Statement, Value};

pub fn rename_stmt(namer: &VariableNamer, stmt: Statement) -> Statement {
    match stmt {
        Statement::Assign { target, value } => Statement::Assign {
            target: rename_target(namer, target),
            value: rename_expr(namer, value),
        },
        Statement::Let { name, value, kind } => Statement::Let {
            name: maybe_rename_var(namer, &name).into(),
            value: rename_expr(namer, value),
            kind,
        },
        Statement::Expr(e) => Statement::Expr(rename_expr(namer, e)),
        Statement::Return(Some(e)) => Statement::Return(Some(rename_expr(namer, e))),
        Statement::Throw(e) => Statement::Throw(rename_expr(namer, e)),
        Statement::If {
            condition,
            then_body,
            else_body,
        } => Statement::If {
            condition: rename_expr(namer, condition),
            then_body: then_body
                .into_iter()
                .map(|s| rename_stmt(namer, s))
                .collect(),
            else_body: else_body
                .into_iter()
                .map(|s| rename_stmt(namer, s))
                .collect(),
        },
        Statement::While { condition, body } => Statement::While {
            condition: rename_expr(namer, condition),
            body: body.into_iter().map(|s| rename_stmt(namer, s)).collect(),
        },
        Statement::DoWhile { body, condition } => Statement::DoWhile {
            body: body.into_iter().map(|s| rename_stmt(namer, s)).collect(),
            condition: rename_expr(namer, condition),
        },
        Statement::For {
            init,
            condition,
            update,
            body,
        } => Statement::For {
            init: init.map(|s| Box::new(rename_stmt(namer, *s))),
            condition: condition.map(|e| rename_expr(namer, e)),
            update: update.map(|s| Box::new(rename_stmt(namer, *s))),
            body: body.into_iter().map(|s| rename_stmt(namer, s)).collect(),
        },
        Statement::ForOf {
            variable,
            iterable,
            body,
        } => Statement::ForOf {
            variable: maybe_rename_var(namer, &variable),
            iterable: rename_expr(namer, iterable),
            body: body.into_iter().map(|s| rename_stmt(namer, s)).collect(),
        },
        Statement::ForIn {
            variable,
            object,
            body,
        } => Statement::ForIn {
            variable: maybe_rename_var(namer, &variable),
            object: rename_expr(namer, object),
            body: body.into_iter().map(|s| rename_stmt(namer, s)).collect(),
        },
        Statement::TryCatch {
            try_body,
            catch_param,
            catch_body,
            finally_body,
        } => Statement::TryCatch {
            try_body: try_body
                .into_iter()
                .map(|s| rename_stmt(namer, s))
                .collect(),
            catch_param: catch_param.map(|p| maybe_rename_var(namer, &p)),
            catch_body: catch_body
                .into_iter()
                .map(|s| rename_stmt(namer, s))
                .collect(),
            finally_body: finally_body
                .into_iter()
                .map(|s| rename_stmt(namer, s))
                .collect(),
        },
        Statement::Switch {
            discriminant,
            cases,
            default,
        } => Statement::Switch {
            discriminant: rename_expr(namer, discriminant),
            cases: cases
                .into_iter()
                .map(|(e, stmts)| {
                    (
                        rename_expr(namer, e),
                        stmts.into_iter().map(|s| rename_stmt(namer, s)).collect(),
                    )
                })
                .collect(),
            default: default
                .map(|stmts| stmts.into_iter().map(|s| rename_stmt(namer, s)).collect()),
        },
        Statement::Block(stmts) => {
            Statement::Block(stmts.into_iter().map(|s| rename_stmt(namer, s)).collect())
        }
        // The `extends` expression reads the enclosing scope like any other
        // expression; left untouched, the base class kept its old name while
        // its definition was renamed, and `class X extends tmp2` extended a
        // name nothing defined any more. The class body is not renamed here.
        Statement::Class {
            name,
            super_class,
            constructor,
            methods,
        } => Statement::Class {
            name,
            super_class: super_class.map(|e| rename_expr(namer, e)),
            constructor,
            methods,
        },
        other => other,
    }
}

fn rename_target(namer: &VariableNamer, mut target: AssignTarget) -> AssignTarget {
    Renamer(namer).visit_assign_target(&mut target);
    target
}

// Rename every register and variable an expression reads or writes. Built on
// MutVisitor, which reaches every expression variant: the hand written match it
// replaces had no arm for template literals or JSX, so a definition was renamed
// while its use inside `${}` kept the old name.
fn rename_expr(namer: &VariableNamer, mut expr: Expression) -> Expression {
    Renamer(namer).visit_expression(&mut expr);
    expr
}

struct Renamer<'a>(&'a VariableNamer);

impl Renamer<'_> {
    fn rename(&self, binding: &mut Binding) {
        let key = match binding {
            Binding::Register(r) => format!("r{r}"),
            Binding::Variable(v) => v.to_string(),
            _ => return,
        };
        if let Some(name) = self.0.inferred_names.get(&key) {
            *binding = Binding::Variable(name.clone().into());
        }
    }
}

impl MutVisitor for Renamer<'_> {
    fn visit_expression(&mut self, expr: &mut Expression) {
        match expr {
            Expression::Value(Value::Binding(b)) => self.rename(b),
            _ => self.walk_expression(expr),
        }
    }

    fn visit_assign_target(&mut self, target: &mut AssignTarget) {
        match target {
            AssignTarget::Binding(b) => self.rename(b),
            _ => self.walk_assign_target(target),
        }
    }
}

fn maybe_rename_var(namer: &VariableNamer, name: &str) -> String {
    if let Some(new_name) = namer.inferred_names.get(name) {
        return new_name.clone();
    }
    name.to_string()
}
