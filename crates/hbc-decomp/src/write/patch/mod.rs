// Patch an existing Hermes bytecode image (hermes_rs issue #10 class).
//
// Split by concern: `strings` (string table edits, same-length and resize),
// `functions` (whole function body replace), `inject` (stub injection), `regexp`
// (replace one RegExp entry's bytecode).

use crate::write::serialize::SerializeOptions;

pub mod debug_reloc;
pub mod functions;
pub(crate) mod handlers;
pub mod inject;
pub mod operands;
pub mod regexp;
pub(crate) mod string_storage;
pub mod strings;

pub use functions::{patch_function_body, patch_function_bytes};
pub use inject::{inject_stub, InjectStubKind};
pub use operands::{patch_string_operand, OperandTarget};
pub use regexp::{
    regexp_payload, repoint_regexp, set_regexp, RegexpRepointReport, RegexpSetMode, RegexpSetReport,
};
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
    /// `docs/plan_guides/01_read/unmodeled_regions/PLAN.md` P0. Measured: no function in the Equinox
    /// bundle carries `FLAG_HAS_DEBUG_INFO` (0 of 62,909), so refusing by default
    /// costs the workflow this crate exists for nothing, and fires only on
    /// debug-built bundles where it is right.
    pub allow_stale_debug_info: bool,
    /// Give every string its own storage when a write rebuilds the string
    /// table, as `hermesc` does without `-O`.
    ///
    /// Default `false`: the rebuild shares storage between a string and any
    /// string it is a suffix of, which keeps a patched bundle closer to what
    /// `hermesc -O` emitted (on the Equinox bundle the unpacked rebuild costs
    /// ~431 KB, packing recovers ~100 KB of it) and keeps offsets further from
    /// the 23-bit limit past which an entry needs an overflow record. Either way
    /// the entry being written gets bytes of its own, and every entry is checked
    /// against its intended text before the result is returned
    /// (`docs/plan_guides/06_write/string_packing/PLAN.md`).
    pub unpacked_strings: bool,
}

impl PatchOptions {
    fn string_packing(&self) -> string_storage::Packing {
        if self.unpacked_strings {
            string_storage::Packing::Unpacked
        } else {
            string_storage::Packing::Packed
        }
    }
}
