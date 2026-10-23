mod builder;
mod cfg;
pub mod depth;
pub mod expr;
pub mod placeholder;
mod stmt;
mod types;
pub mod utils;
mod visitor;

pub use builder::*;
pub use cfg::*;
pub use depth::{DepthGuard, MAX_RENDER_DEPTH, TOO_DEEP, TOO_DEEP_EXPR};
pub use expr::*;
pub use stmt::{AssignTarget, ClassMethod, MethodKind, Statement, Terminator, VarKind};
pub use types::*;
pub use utils::{
    expr_uses_register, exprs_equal, extract_function_id, for_each_nested_body,
    for_each_target_expression, for_each_target_expression_mut, get_value_name, is_nan_check,
    is_simple_value, is_undefined_expr, map_nested_bodies, map_nested_bodies_mut,
    map_target_expressions, property_key_uses_register, property_keys_equal, shift_env_levels,
    shift_env_levels_cfg, stmt_has_side_effects, stmt_uses_register, target_to_key,
};
pub use visitor::*;
