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
    pub options: u8,
    pub layout: HeaderLayout,
    pub function_header_layout: FunctionHeaderLayout,
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
