// Function-header flag bits (the `flags: u8` field of both Legacy and Modern
// function headers). These mirror `FunctionHeaderFlag` in Hermes'
// `BytecodeFileFormat.h`, whose bit layout is **identical from v96 through v99**
// (v99 only appends `Kind` in the top two bits):
//
//   bits 0-1 (0x03): prohibitInvoke, an enum -- NOT a pair of independent flags
//   bit  2   (0x04): strictMode
//   bit  3   (0x08): hasExceptionHandler, the info section carries a handler table
//   bit  4   (0x10): hasDebugInfo, the info section carries debug offsets
//   bit  5   (0x20): overflowed, the real header is stored out-of-line
//   bits 6-7 (0xC0): kind (Normal / Generator / Async), v99+
//
// Verified against hermesc-built fixtures on both a v96 and a v99 engine: a
// strict, constructible function with debug info is `0x16`.

// prohibitInvoke occupies bits 0-1 and holds one of PROHIBIT_* below. It is an
// enum, so it must be masked and compared, never tested bitwise -- ProhibitCall
// is zero, so `flags & X != 0` can never detect it.
pub const MASK_PROHIBIT_INVOKE: u8 = 0x03;
// Plain calls prohibited (a class constructor: must be invoked with `new`).
pub const PROHIBIT_CALL: u8 = 0;
// Construction prohibited (an arrow function, method, or similar).
pub const PROHIBIT_CONSTRUCT: u8 = 1;
// Nothing prohibited: an ordinary function. Note this is 2, not 0, so a
// zero-filled or misaligned flags byte reads as "calls prohibited" and fails
// closed rather than silently permitting.
pub const PROHIBIT_NONE: u8 = 2;

// Strict-mode flag (bit 2).
pub const FLAG_STRICT: u8 = 0x04;
// hasExceptionHandler flag (bit 3): the function's info section has an
// exception handler table.
pub const FLAG_HAS_EXCEPTION_HANDLER: u8 = 0x08;
// hasDebugInfo flag (bit 4): the function's info section has debug offsets.
pub const FLAG_HAS_DEBUG_INFO: u8 = 0x10;
// Overflowed flag (bit 5): the function header is a large/overflowed header
// stored out-of-line.
pub const FLAG_OVERFLOWED: u8 = 0x20;

// ---------------------------------------------------------------------------
// The header's `options` byte (OB1)
// ---------------------------------------------------------------------------

/// Bit 0 -- `staticBuiltins`. Present at every version this crate reads.
pub const OPTION_STATIC_BUILTINS: u8 = 1 << 0;
/// Bit 1 -- `cjsModulesStaticallyResolved`. Present at every version, and the
/// bit that decides which of the two CJS module tables the file carries.
pub const OPTION_CJS_MODULES_STATICALLY_RESOLVED: u8 = 1 << 1;
/// Bit 2 -- `hasAsync`. Declared from v81 through v97 and **removed at v98**;
/// see `BytecodeOptions::has_async`.
pub const OPTION_HAS_ASYNC: u8 = 1 << 2;

/// The last version whose `BytecodeOptions` declares `hasAsync`.
///
/// Upstream added the bit while the tree declared version 81 (`Check Promise for
/// Async Fn via BytecodeOptions`, 2021-01-25) and removed it in the BitField
/// rewrite while the tree declared 98 (`Use the new BitField class for the file
/// format`, 2025-02-25). Neither commit bumped `BYTECODE_VERSION`, so v98 is the
/// version whose meaning changed under it -- a v98 file built before that commit
/// can still carry the bit. `has_async` therefore reports `None` from v98 on
/// rather than `Some(false)`, and such a byte surfaces through `unknown_bits`
/// instead of being read as a flag that no longer exists.
pub const OPTION_HAS_ASYNC_MAX_VERSION: u32 = 97;

/// The header's `options` byte, decoded.
///
/// Upstream's `BytecodeOptions` is a one-byte bitfield whose *set of bits is
/// version-keyed*, and this crate carried it as a bare `u8` that nothing read
/// (OB1). That is the R8/R19 shape once more: a version-keyed structure held as
/// an integer, with nothing to notice when upstream reshapes it -- and it had
/// already been reshaped inside the range we support.
///
/// | version | bits |
/// |---|---|
/// | v96, v97 | `staticBuiltins`, `cjsModulesStaticallyResolved`, `hasAsync` |
/// | v98, v99 | `StaticBuiltins`, `CjsModulesStaticallyResolved` |
///
/// Bit order is declaration order, LSB first: a `bool : 1` chain packs that way
/// on every compiler Hermes is built with, and from v98 upstream states it
/// outright, each `HERMES_NEXT_BITFIELD` naming the field it follows. Measured
/// on `tests/fixtures/asyncy.js`: the same source compiles to `options = 0b100`
/// at v96 and `0b000` at v98 and v99.
///
/// The byte itself stays on the header as `BytecodeHeader::options_raw` and the
/// write path goes on round-tripping it verbatim; this is a view over it, not a
/// replacement for it.
///
/// `tests/upstream_pin.rs::bytecode_options_bits_match_upstream` re-derives the
/// bit list from each configured checkout and fails if a bit is added, removed
/// or reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BytecodeOptions {
    raw: u8,
    version: u32,
}

impl BytecodeOptions {
    pub fn new(raw: u8, version: u32) -> Self {
        Self { raw, version }
    }

    /// The undecoded byte, exactly as it sits in the file.
    pub fn raw(self) -> u8 {
        self.raw
    }

    /// The version this byte is being read at, which is what keys everything
    /// below.
    pub fn version(self) -> u32 {
        self.version
    }

    /// `staticBuiltins`: the bundle was compiled with the builtins frozen, so
    /// `CallBuiltin` may be trusted to reach the real builtin.
    pub fn static_builtins(self) -> bool {
        self.raw & OPTION_STATIC_BUILTINS != 0
    }

    /// `cjsModulesStaticallyResolved`: the CJS module table is keyed by module
    /// index rather than by filename string id. See [`CjsModuleForm`].
    pub fn cjs_modules_statically_resolved(self) -> bool {
        self.raw & OPTION_CJS_MODULES_STATICALLY_RESOLVED != 0
    }

    /// `hasAsync`: the bundle contains an `async function`, so the VM requires a
    /// Promise implementation.
    ///
    /// `None` means *this version does not define the bit*, which is a different
    /// claim from `Some(false)` -- upstream removed it at v98, so reading bit 2
    /// there would report a flag that no longer exists. Callers that want the
    /// bit regardless can read [`Self::raw`].
    pub fn has_async(self) -> Option<bool> {
        (self.version <= OPTION_HAS_ASYNC_MAX_VERSION).then_some(self.raw & OPTION_HAS_ASYNC != 0)
    }

    /// Which CJS module table this file carries (OB2). The two are
    /// indistinguishable in the file itself; this bit is the only thing that
    /// separates them.
    pub fn cjs_module_form(self) -> CjsModuleForm {
        if self.cjs_modules_statically_resolved() {
            CjsModuleForm::StaticallyResolved
        } else {
            CjsModuleForm::Filenames
        }
    }

    /// The bits this version defines, as a mask.
    pub fn defined_mask(self) -> u8 {
        let mut mask = OPTION_STATIC_BUILTINS | OPTION_CJS_MODULES_STATICALLY_RESOLVED;
        if self.version <= OPTION_HAS_ASYNC_MAX_VERSION {
            mask |= OPTION_HAS_ASYNC;
        }
        mask
    }

    /// Set bits this version does not define.
    ///
    /// Non-zero means one of two things, and both want a look: upstream grew a
    /// bit we do not model, or the file predates a removal (a v98 image built
    /// before the BitField rewrite carries `hasAsync` in bit 2). Reporting it is
    /// the point of decoding the byte rather than carrying it.
    pub fn unknown_bits(self) -> u8 {
        self.raw & !self.defined_mask()
    }

    /// The names of the set bits, in bit order, for display.
    pub fn set_bit_names(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.static_builtins() {
            names.push("staticBuiltins");
        }
        if self.cjs_modules_statically_resolved() {
            names.push("cjsModulesStaticallyResolved");
        }
        if self.has_async() == Some(true) {
            names.push("hasAsync");
        }
        names
    }
}

impl std::fmt::Display for BytecodeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:02x}", self.raw)?;
        let names = self.set_bit_names();
        if !names.is_empty() {
            write!(f, " [{}]", names.join(", "))?;
        }
        let unknown = self.unknown_bits();
        if unknown != 0 {
            write!(f, " [unknown bits 0x{unknown:02x}]")?;
        }
        if names.is_empty() && unknown == 0 {
            write!(f, " [none set]")?;
        }
        Ok(())
    }
}

/// Which of the two CJS module tables a file carries (OB2).
///
/// The section holds `cjsModuleCount` pairs either way, and `.second` is the
/// function id in both. What differs is `.first`, and nothing in the file says
/// which it is except `options` bit 1 -- which is why mislabelling it does not
/// produce an obvious error, just an unrelated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CjsModuleForm {
    /// Bit clear: `cjsModuleTable`, pairs are `(filename string id, function
    /// id)`, built by `addCJSModule`.
    Filenames,
    /// Bit set: `cjsModuleTableStatic`, pairs are `(module id, function id)`,
    /// built by `addCJSModuleStatic`. The module id indexes nothing in this
    /// file: resolving it against the string table prints an unrelated string.
    StaticallyResolved,
}

impl CjsModuleForm {
    /// What `.first` holds, as a field name.
    pub fn first_field(self) -> &'static str {
        match self {
            CjsModuleForm::Filenames => "filename_string_id",
            CjsModuleForm::StaticallyResolved => "module_id",
        }
    }

    /// The form, in words, including the bit that decided it.
    pub fn describe(self) -> &'static str {
        match self {
            CjsModuleForm::Filenames => "filename string ids, options bit 1 clear",
            CjsModuleForm::StaticallyResolved => {
                "statically resolved module ids, options bit 1 set"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderLayout {
    Legacy,
    Modern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionHeaderLayout {
    Legacy16,
    Modern12,
}

#[derive(Debug, Clone)]
pub struct BytecodeHeader {
    pub magic: u64,
    pub version: u32,
    pub source_hash: [u8; 20],
    pub file_length: u32,
    pub global_code_index: u32,
    pub function_count: u32,
    pub string_kind_count: u32,
    pub identifier_count: u32,
    pub string_count: u32,
    pub overflow_string_count: u32,
    pub string_storage_size: u32,
    pub big_int_count: Option<u32>,
    pub big_int_storage_size: Option<u32>,
    pub reg_exp_count: u32,
    pub reg_exp_storage_size: u32,
    pub literal_value_buffer_size: Option<u32>,
    pub array_buffer_size: Option<u32>,
    pub obj_key_buffer_size: u32,
    pub obj_value_buffer_size: Option<u32>,
    pub obj_shape_table_count: Option<u32>,
    pub num_string_switch_imms: Option<u32>,
    pub segment_id: Option<u32>,
    pub cjs_module_offset: Option<u32>,
    pub cjs_module_count: u32,
    pub function_source_count: Option<u32>,
    pub debug_info_offset: u32,
    /// The `options` bitfield, undecoded. Read it through
    /// [`BytecodeHeader::options`]; the field itself stays raw because the write
    /// path round-trips the byte verbatim.
    pub options_raw: u8,
    pub layout: HeaderLayout,
    pub function_header_layout: FunctionHeaderLayout,
}

impl BytecodeHeader {
    /// The `options` byte, decoded against this file's version.
    pub fn options(&self) -> BytecodeOptions {
        BytecodeOptions::new(self.options_raw, self.version)
    }
}

#[derive(Debug, Clone)]
pub struct LegacyFunctionHeader {
    pub function_id: u32,
    pub offset: u32,
    pub param_count: u32,
    pub bytecode_size_in_bytes: u32,
    pub function_name: u32,
    pub info_offset: u32,
    pub frame_size: u32,
    pub environment_size: u32,
    pub highest_read_cache_index: u32,
    pub highest_write_cache_index: u32,
    pub flags: u8,
}

#[derive(Debug, Clone)]
pub struct ModernFunctionHeader {
    pub function_id: u32,
    pub offset: u32,
    pub param_count: u32,
    pub loop_depth: u32,
    pub bytecode_size_in_bytes: u32,
    pub function_name: u32,
    pub number_reg_count: u32,
    pub non_ptr_reg_count: u32,
    pub frame_size: u32,
    pub read_cache_size: u8,
    pub write_cache_size: u8,
    pub num_cache_new_object: u8,
    pub private_name_cache_size: u8,
    pub flags: u8,
    // Offset of the function's FunctionInfo (exception handler table + debug
    // info), which immediately follows the large/overflow header. 0 when the
    // function is not overflowed (a small 12-byte header carries no info section,
    // so it can have no exception handlers). See parse_large_header_modern.
    pub info_offset: u32,
}

#[derive(Debug, Clone)]
pub enum FunctionHeader {
    Legacy(LegacyFunctionHeader),
    Modern(ModernFunctionHeader),
}

impl FunctionHeader {
    pub fn function_id(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.function_id,
            FunctionHeader::Modern(header) => header.function_id,
        }
    }

    pub fn offset(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.offset,
            FunctionHeader::Modern(header) => header.offset,
        }
    }

    pub fn bytecode_size_in_bytes(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.bytecode_size_in_bytes,
            FunctionHeader::Modern(header) => header.bytecode_size_in_bytes,
        }
    }

    pub fn function_name(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.function_name,
            FunctionHeader::Modern(header) => header.function_name,
        }
    }

    pub fn frame_size(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.frame_size,
            FunctionHeader::Modern(header) => header.frame_size,
        }
    }

    pub fn param_count(&self) -> u32 {
        match self {
            FunctionHeader::Legacy(header) => header.param_count,
            FunctionHeader::Modern(header) => header.param_count,
        }
    }

    pub fn flags(&self) -> u8 {
        match self {
            FunctionHeader::Legacy(header) => header.flags,
            FunctionHeader::Modern(header) => header.flags,
        }
    }

    pub fn is_overflowed(&self) -> bool {
        self.flags() & FLAG_OVERFLOWED != 0
    }

    // Which kinds of invocation the function prohibits: one of PROHIBIT_CALL /
    // PROHIBIT_CONSTRUCT / PROHIBIT_NONE.
    pub fn prohibit_invoke(&self) -> u8 {
        self.flags() & MASK_PROHIBIT_INVOKE
    }

    // Check if the function prohibits construction (cannot be used with `new`).
    // This is a strong indicator of arrow functions.
    pub fn prohibit_construct(&self) -> bool {
        self.prohibit_invoke() == PROHIBIT_CONSTRUCT
    }

    // Check if the function prohibits plain calls (must be invoked with `new`),
    // i.e. a class constructor.
    pub fn prohibit_call(&self) -> bool {
        self.prohibit_invoke() == PROHIBIT_CALL
    }

    // Check if the function's info section carries debug offsets. Together with
    // has_exception_handler this is what forces a modern function to be emitted
    // overflowed, regardless of whether its fields would fit inline.
    pub fn has_debug_info(&self) -> bool {
        self.flags() & FLAG_HAS_DEBUG_INFO != 0
    }

    // Check if the function declares an exception-handler table. On modern
    // layouts this must come from the large header (see modern_layout.rs).
    pub fn has_exception_handler(&self) -> bool {
        self.flags() & FLAG_HAS_EXCEPTION_HANDLER != 0
    }

    // Check if the function is in strict mode.
    pub fn is_strict(&self) -> bool {
        self.flags() & FLAG_STRICT != 0
    }

    // Heuristic: a function is likely an arrow function if it prohibits construction.
    // Arrow functions in JS cannot be used as constructors.
    pub fn is_likely_arrow(&self) -> bool {
        self.prohibit_construct()
    }

    // Get the environment size (closure slots) - only available in Legacy headers.
    pub fn environment_size(&self) -> Option<u32> {
        match self {
            FunctionHeader::Legacy(header) => Some(header.environment_size),
            FunctionHeader::Modern(_) => None,
        }
    }
}
