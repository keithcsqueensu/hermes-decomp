// Patch an existing Hermes bytecode image (hermes_rs issue #10 class).
//
// Split by concern: `strings` (string table edits, same-length and resize),
// `functions` (whole function body replace), `inject` (stub injection).

use crate::write::serialize::SerializeOptions;

pub mod functions;
pub mod inject;
pub mod operands;
pub mod strings;

pub use functions::{patch_function_body, patch_function_bytes};
pub use inject::{inject_stub, InjectStubKind};
pub use operands::{patch_string_operand, OperandTarget};
pub use strings::{add_string, patch_string_by_id, patch_string_replace, retarget_string};

#[derive(Debug, Clone, Default)]
pub struct PatchOptions {
    pub serialize: SerializeOptions,
    /// Permit a size-changing edit to a function that carries debug info,
    /// knowing the edit leaves that function's source locations stale.
    ///
    /// Default `false` — the edit is refused, mirroring the exception-handler
    /// guard. Location streams store bytecode addresses *within* a function, so a
    /// resize silently repoints every location past the edit; see R24 and
    /// `docs/UNMODELED_REGIONS_PLAN.md` P0. Measured: no function in the Equinox
    /// bundle carries `FLAG_HAS_DEBUG_INFO` (0 of 62,909), so refusing by default
    /// costs the workflow this crate exists for nothing, and fires only on
    /// debug-built bundles where it is right.
    pub allow_stale_debug_info: bool,
}
