// Byte layout of the Modern (HBC v97+) function headers, keyed by bytecode
// version.
//
// The out-of-line "large" function header is NOT a single layout across all of
// v97+, and treating it as one is what R8/R9/R15 in docs/WRITE_PATH_GUIDE.md
// were about. Upstream `FUNC_HEADER_FIELDS` (Hermes' BytecodeFileFormat.h) has
// changed shape twice inside the "modern" era, and *without a version bump*
// both times, so the version integer alone is a weak selector. This descriptor
// makes the layout explicit, keyed to a version, and refuses versions it has
// not been told about rather than extrapolating from the newest known shape.
//
// Timeline of the upstream shapes (facebook/hermes, `static_h`):
//
//   v97 .. v98(early)   pre-`352c7d0aa` (2025-02-25). Seven fields, no
//                       LoopDepth / NumberRegCount / NonPtrRegCount. The small
//                       header packs the large pointer as
//                       (functionName << 16) | (offset & 0xffff) and uses
//                       different bit widths (paramCount 7, size 15,
//                       functionName 17). The large header is 4x u32 + 4x u8 =
//                       20 bytes. NOT SUPPORTED here: the small-header decode in
//                       file/parser/function.rs assumes the post-BitField
//                       widths, so this vintage needs its own decoder, not just
//                       a different size.
//   v98(late)           `e42564dc6` (2025-03-31) onward. 8x u32 + 5x u8 = 37
//                       bytes; the u8 tail is Read, Write, NumCacheNewObject,
//                       PrivateName, flags. This is what React Native ships as
//                       v98 (`origin/250829098.0.0-stable`).
//   v99                 `7193d4485` (2026-01-21) removed NumCacheNewObject, so
//                       8x u32 + 4x u8 = 36 bytes. Both `static_h` HEAD and
//                       `origin/260318099.0.0-stable`.
//
// The 8x u32 prefix is identical across the late-v98 and v99 shapes, which is
// why the frame-size / read-cache offsets used by stub injection survived the
// v99 change; only the u8 tail and the derived FunctionInfo position moved.

use crate::error::{Error, Result};

// First bytecode version using the 12-byte Modern function header. Canonical
// home for this constant; `file::parser::header` re-exports it.
pub const MODERN_FUNCTION_HEADER_MIN_VERSION: u32 = 97;

// Size of a Modern12 inline ("small") function header. Unchanged v97..v99.
pub const MODERN_SMALL_HEADER_SIZE: usize = 12;

// Byte offset of the flags byte within a Modern12 small header.
//
// Read the overflow bit from HERE, never from a parsed `FunctionHeader::Modern`:
// for an overflowed function the parsed struct's flags come from the *large*
// header, and Hermes' `SmallFuncHeader(uint32_t largeHeaderOffset)` zeroes the
// whole header and sets only `Overflowed`, so the large header never carries
// that bit.
pub const MODERN_SMALL_FLAGS_POS: usize = 11;

// Byte offsets of the u32 prefix of a modern large header. Shared by every
// supported version, hence plain constants rather than methods.
pub const MODERN_LARGE_OFFSET: usize = 0;
pub const MODERN_LARGE_PARAM_COUNT: usize = 4;
pub const MODERN_LARGE_LOOP_DEPTH: usize = 8;
pub const MODERN_LARGE_BYTECODE_SIZE: usize = 12;
pub const MODERN_LARGE_FUNCTION_NAME: usize = 16;
pub const MODERN_LARGE_NUMBER_REG_COUNT: usize = 20;
pub const MODERN_LARGE_NON_PTR_REG_COUNT: usize = 24;
pub const MODERN_LARGE_FRAME_SIZE: usize = 28;
// First byte of the u8 tail, i.e. just past the u32 prefix.
pub const MODERN_LARGE_U8_TAIL: usize = 32;

// Byte layout of the modern function headers for one bytecode version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModernLayout {
    version: u32,
    // The single bit that distinguishes the two supported shapes: late-v98 has a
    // NumCacheNewObject field between WriteCacheSize and PrivateNameCacheSize
    // (its own byte in the large header, one bit stolen from WriteCacheSize in
    // the small header); v99 removed it.
    has_num_cache_new_object: bool,
}

impl ModernLayout {
    // Look up the layout for a bytecode version.
    //
    // Deliberately an allow-list with a hard error, not a best-effort guess: a
    // wrong guess here does not fail loudly, it silently emits a file the VM
    // misreads. That is exactly how `create --version 99` came to produce an
    // image the real v99 engine rejects at entry, and how the exception-handler
    // guard came to fire at random. A clear "unknown layout" beats a file that
    // loads and misbehaves.
    pub fn for_version(version: u32) -> Result<Self> {
        match version {
            98 => Ok(Self {
                version,
                has_num_cache_new_object: true,
            }),
            99 => Ok(Self {
                version,
                has_num_cache_new_object: false,
            }),
            97 => Err(Error::Parse(format!(
                "HBC v{version} uses the pre-2025-02-25 modern function-header layout \
                 (20-byte large header, 16-bit packed large pointer, different small-header \
                 bit widths). That vintage needs its own decoder and is not implemented; \
                 supported modern layouts are v98 and v99."
            ))),
            v if v >= MODERN_FUNCTION_HEADER_MIN_VERSION => Err(Error::Parse(format!(
                "HBC v{v} modern function-header layout is not known to this build \
                 (supported: 98, 99). Upstream has changed this layout without bumping the \
                 bytecode version before, so this refuses rather than assuming the newest \
                 known shape. Re-derive it from FUNC_HEADER_FIELDS in Hermes' \
                 BytecodeFileFormat.h and add it to ModernLayout::for_version."
            ))),
            v => Err(Error::Parse(format!(
                "HBC v{v} is not a modern (v{MODERN_FUNCTION_HEADER_MIN_VERSION}+) layout"
            ))),
        }
    }

    pub fn version(self) -> u32 {
        self.version
    }

    // Total size of the out-of-line large header, i.e. `sizeof(FunctionHeader)`
    // upstream. The struct is LLVM_PACKED, so this is just the sum of its fields.
    pub fn large_size(self) -> usize {
        MODERN_LARGE_U8_TAIL + if self.has_num_cache_new_object { 5 } else { 4 }
    }

    pub fn large_read_cache_size_pos(self) -> usize {
        MODERN_LARGE_U8_TAIL
    }

    pub fn large_write_cache_size_pos(self) -> usize {
        MODERN_LARGE_U8_TAIL + 1
    }

    // `None` from v99 on, where the field was removed.
    pub fn large_num_cache_new_object_pos(self) -> Option<usize> {
        self.has_num_cache_new_object
            .then_some(MODERN_LARGE_U8_TAIL + 2)
    }

    pub fn large_private_name_cache_size_pos(self) -> usize {
        MODERN_LARGE_U8_TAIL + if self.has_num_cache_new_object { 3 } else { 2 }
    }

    // Position of the flags byte: always the last byte of the large header.
    pub fn large_flags_pos(self) -> usize {
        self.large_size() - 1
    }

    // Where a function's FunctionInfo (exception-handler table, then debug
    // offsets) begins, given the position of its large header.
    //
    // Modern small headers carry no info_offset field, so this is derived rather
    // than read. It mirrors the VM exactly (`BCProviderFromBuffer::
    // getExceptionTableAndDebugOffsets`): advance past the large header by
    // `sizeof(FunctionHeader)`, then align to 4.
    pub fn info_offset_for(self, large_ptr: usize) -> usize {
        (large_ptr + self.large_size() + 3) & !3
    }

    // Width in bits of the small header's WriteCacheSize bitfield. Late-v98
    // gives one of its bits to NumCacheNewObject; v99 takes it back.
    pub fn small_write_cache_bits(self) -> u32 {
        if self.has_num_cache_new_object {
            6
        } else {
            7
        }
    }

    pub fn has_num_cache_new_object(self) -> bool {
        self.has_num_cache_new_object
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v98_is_37_bytes_with_num_cache_new_object() {
        let l = ModernLayout::for_version(98).unwrap();
        assert_eq!(l.large_size(), 37);
        assert_eq!(l.large_num_cache_new_object_pos(), Some(34));
        assert_eq!(l.large_private_name_cache_size_pos(), 35);
        assert_eq!(l.large_flags_pos(), 36);
        assert_eq!(l.small_write_cache_bits(), 6);
    }

    #[test]
    fn v99_is_36_bytes_without_num_cache_new_object() {
        let l = ModernLayout::for_version(99).unwrap();
        assert_eq!(l.large_size(), 36);
        assert_eq!(l.large_num_cache_new_object_pos(), None);
        assert_eq!(l.large_private_name_cache_size_pos(), 34);
        assert_eq!(l.large_flags_pos(), 35);
        assert_eq!(l.small_write_cache_bits(), 7);
    }

    // The whole point of the descriptor: v98 and v99 must not agree, and the
    // difference has to land on the FunctionInfo position. A 4-byte-aligned
    // large header is 37 -> +40 on v98 and 36 -> +36 on v99.
    #[test]
    fn info_offset_differs_between_v98_and_v99() {
        assert_eq!(
            ModernLayout::for_version(98).unwrap().info_offset_for(400),
            440
        );
        assert_eq!(
            ModernLayout::for_version(99).unwrap().info_offset_for(400),
            436
        );
    }

    #[test]
    fn unknown_and_unsupported_versions_are_hard_errors() {
        // v97 is a real but different layout, and the error says so.
        let e = ModernLayout::for_version(97).unwrap_err().to_string();
        assert!(e.contains("97"), "{e}");
        // A future version must not silently reuse the newest known shape.
        assert!(ModernLayout::for_version(100).is_err());
        // Legacy versions are not modern.
        assert!(ModernLayout::for_version(96).is_err());
    }
}
