pub mod display;
mod ops;

pub use ops::*;

use super::types::{Constant, FunctionId, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PropertyKey {
    Ident(String),
    Computed(Box<Expression>),
    String(String),
    Index(i64),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    Value(Value),
    Binary {
        op: BinaryOp,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expression>,
    },
    Conditional {
        condition: Box<Expression>,
        then_expr: Box<Expression>,
        else_expr: Box<Expression>,
    },
    Member {
        object: Box<Expression>,
        property: PropertyKey,
        optional: bool,
    },
    Call {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    New {
        callee: Box<Expression>,
        arguments: Vec<Expression>,
    },
    Array {
        elements: Vec<Option<Expression>>,
    },
    Object {
        properties: Vec<ObjectProperty>,
    },
    Function {
        id: FunctionId,
        name: Option<String>,
        is_arrow: bool,
        is_async: bool,
        is_generator: bool,
        // The builder level of the environment the closure was created on,
        // when the builder knew it: 0 for the creating function's own, past
        // `NESTED_ENV_LEVEL_BASE` for a block environment it built. One
        // compiled function created on several block environments of one
        // function (copies of an inlined factory) is told apart by it
        // (`pipeline::context::shared_bodies`).
        #[serde(default)]
        env: Option<u32>,
    },
    // An assignment used as an expression (`a = b` inside a larger expression).
    // The target is an `AssignTarget` and not an `Expression`, so a pass cannot
    // build a write to something that is not a place. The statement form uses the
    // same type, which is what lets one visitor hook see every write.
    Assignment {
        target: Box<crate::ir::AssignTarget>,
        value: Box<Expression>,
    },
    Spread(Box<Expression>),
    TemplateLiteral {
        quasis: Vec<String>,
        expressions: Vec<Expression>,
    },
    RegExp {
        pattern: String,
        flags: String,
    },
    Yield {
        value: Box<Expression>,
        delegate: bool,
    },
    Await(Box<Expression>),
    JSXElement {
        // An expression, so a read of the tag is a read like any other. As a string
        // no pass saw it: `x = mod; <x.Row />` lost `x =` as dead, and a rename left
        // the tag on the old name. A string constant is an intrinsic, `""` a Fragment.
        tag: Box<Expression>,
        attributes: Vec<(String, Expression)>,
        children: Vec<Expression>,
    },
    Unknown {
        opcode: String,
        operands: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectProperty {
    pub key: PropertyKey,
    pub value: Expression,
}

impl Expression {
    pub fn constant(c: Constant) -> Self {
        Expression::Value(Value::Constant(c))
    }

    pub fn register(r: u32) -> Self {
        Expression::Value(Value::Binding(crate::ir::Binding::Register(r)))
    }

    pub fn binary(op: BinaryOp, left: Expression, right: Expression) -> Self {
        Expression::Binary {
            op,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn unary(op: UnaryOp, operand: Expression) -> Self {
        Expression::Unary {
            op,
            operand: Box::new(operand),
        }
    }

    pub fn member(object: Expression, prop: &str) -> Self {
        Expression::Member {
            object: Box::new(object),
            property: PropertyKey::Ident(prop.to_string()),
            optional: false,
        }
    }

    pub fn index(object: Expression, key: Expression) -> Self {
        Expression::Member {
            object: Box::new(object),
            property: PropertyKey::Computed(Box::new(key)),
            optional: false,
        }
    }

    pub fn call(callee: Expression, arguments: Vec<Expression>) -> Self {
        Expression::Call {
            callee: Box::new(callee),
            arguments,
        }
    }

    pub fn is_simple(&self) -> bool {
        matches!(self, Expression::Value(_))
    }

    pub fn has_side_effects(&self) -> bool {
        match self {
            Expression::Value(_) => false,
            Expression::Binary { left, right, .. } => {
                left.has_side_effects() || right.has_side_effects()
            }
            Expression::Unary { operand, .. } => operand.has_side_effects(),
            Expression::Member {
                object, property, ..
            } => {
                object.has_side_effects()
                    || match property {
                        PropertyKey::Computed(k) => k.has_side_effects(),
                        _ => false,
                    }
            }
            Expression::Array { elements } => {
                elements.iter().flatten().any(|e| e.has_side_effects())
            }
            Expression::Object { properties } => properties.iter().any(|p| {
                p.value.has_side_effects()
                    || match &p.key {
                        PropertyKey::Computed(k) => k.has_side_effects(),
                        _ => false,
                    }
            }),
            Expression::Conditional {
                condition,
                then_expr,
                else_expr,
            } => {
                condition.has_side_effects()
                    || then_expr.has_side_effects()
                    || else_expr.has_side_effects()
            }
            Expression::TemplateLiteral { expressions, .. } => {
                expressions.iter().any(|e| e.has_side_effects())
            }
            Expression::Call { .. }
            | Expression::New { .. }
            | Expression::Assignment { .. }
            | Expression::Yield { .. }
            | Expression::Await(_)
            | Expression::Unknown { .. } => true,
            _ => false,
        }
    }
}

impl Expression {
    // The tag as written in JSX, or `None` if this expression cannot be one. A
    // lowercase name would read as an intrinsic, the string, not the variable.
    pub fn jsx_tag_text(&self) -> Option<String> {
        fn path(e: &Expression) -> Option<String> {
            match e {
                Expression::Value(Value::Binding(super::types::Binding::Variable(v))) => {
                    Some(crate::util::sanitize_identifier(&v.name))
                }
                Expression::Member {
                    object,
                    property: PropertyKey::Ident(p),
                    ..
                } => Some(format!("{}.{p}", path(object)?)),
                _ => None,
            }
        }
        match self {
            Expression::Value(Value::Constant(Constant::String(s))) => Some(s.clone()),
            Expression::Member { .. } => path(self),
            _ => path(self).filter(|n| !n.starts_with(|c: char| c.is_ascii_lowercase())),
        }
    }
}
