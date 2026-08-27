use crate::error::Result;
use crate::format::{
    BytecodeHeader, FunctionHeader, FunctionHeaderLayout, LegacyFunctionHeader,
    ModernFunctionHeader, FLAG_OVERFLOWED,
};
use crate::io::ByteReader;
use crate::modern_layout::{
    ModernLayout, MODERN_LARGE_BYTECODE_SIZE, MODERN_LARGE_FRAME_SIZE,
    MODERN_LARGE_FUNCTION_NAME, MODERN_LARGE_LOOP_DEPTH, MODERN_LARGE_NON_PTR_REG_COUNT,
    MODERN_LARGE_NUMBER_REG_COUNT, MODERN_LARGE_OFFSET, MODERN_LARGE_PARAM_COUNT,
};

// When a function header is marked `FLAG_OVERFLOWED`, the inline header no
// longer holds the real field values; instead it packs the byte offset to the
// out-of-line "large" header. The two layouts pack that offset differently:
//
//   Legacy16: large_offset = (info_offset << 16) | offset
//   Modern12: large_offset = (function_name << 24) | (offset & 0x00ff_ffff)
//
// The shift amounts below name those packings.
const LEGACY_LARGE_OFFSET_SHIFT: u64 = 16;
const MODERN_LARGE_OFFSET_SHIFT: u64 = 24;
// Mask for the low 24 bits of the Modern packed offset (the `offset` portion).
const MODERN_LARGE_OFFSET_MASK: u64 = 0x00ff_ffff;

pub fn parse_function_headers(
    reader: &mut ByteReader<'_>,
    header: &BytecodeHeader,
) -> Result<Vec<FunctionHeader>> {
    // Modern images need a version-keyed layout descriptor for the out-of-line
    // large header; resolve it once so an unsupported modern version fails here,
    // loudly, instead of silently mis-decoding every overflowed function.
    let modern_layout = match header.function_header_layout {
        FunctionHeaderLayout::Modern12 => Some(ModernLayout::for_version(header.version)?),
        FunctionHeaderLayout::Legacy16 => None,
    };
    let mut headers = Vec::with_capacity(reader.capacity_hint(header.function_count as usize));
    for function_id in 0..header.function_count {
        let current_pos = reader.position();
        let function_header = match header.function_header_layout {
            // Legacy Header (16 bytes):
            // Used in Hermes bytecode version < 97.
            // Compacts multiple fields into a single u128 for extreme density.
            // fields: [offset, param_count, size, name, info_offset, frame_size, env_size, registers]
            FunctionHeaderLayout::Legacy16 => {
                let raw = reader.read_bytes(16)?;
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(raw);
                let raw = u128::from_le_bytes(bytes);
                // Legacy16 bitfield map, (bit offset, width) within the 128-bit word:
                //   offset                    : ( 0, 25)
                //   param_count               : (25,  7)
                //   bytecode_size_in_bytes    : (32, 15)
                //   function_name             : (47, 17)
                //   info_offset               : (64, 25)
                //   frame_size                : (89,  7)
                //   environment_size          : (96,  8)
                //   highest_read_cache_index  : (104, 8)
                //   highest_write_cache_index : (112, 8)
                //   flags                     : (120, 8)
                let offset = (raw & ((1u128 << 25) - 1)) as u32;
                let param_count = ((raw >> 25) & ((1u128 << 7) - 1)) as u32;
                let bytecode_size_in_bytes = ((raw >> 32) & ((1u128 << 15) - 1)) as u32;
                let function_name = ((raw >> 47) & ((1u128 << 17) - 1)) as u32;
                let info_offset = ((raw >> 64) & ((1u128 << 25) - 1)) as u32;
                let frame_size = ((raw >> 89) & ((1u128 << 7) - 1)) as u32;
                let environment_size = ((raw >> 96) & 0xff) as u32;
                let highest_read_cache_index = ((raw >> 104) & 0xff) as u32;
                let highest_write_cache_index = ((raw >> 112) & 0xff) as u32;
                let flags = ((raw >> 120) & 0xff) as u8;

                if flags & FLAG_OVERFLOWED != 0 {
                    let large_offset =
                        ((info_offset as u64) << LEGACY_LARGE_OFFSET_SHIFT) | (offset as u64);
                    let large_header =
                        parse_large_header_legacy(reader, large_offset as usize, function_id)?;
                    reader.seek(current_pos + 16)?;
                    FunctionHeader::Legacy(large_header)
                } else {
                    FunctionHeader::Legacy(LegacyFunctionHeader {
                        function_id,
                        offset,
                        param_count,
                        bytecode_size_in_bytes,
                        function_name,
                        info_offset,
                        frame_size,
                        environment_size,
                        highest_read_cache_index,
                        highest_write_cache_index,
                        flags,
                    })
                }
            }
            // Modern Header (12 bytes):
            // Used in Hermes bytecode version >= 97 (including v98).
            // Even more compact (12 bytes vs 16 bytes).
            // Re-arranges bitfields for better packing and newer features (e.g., loop_depth, distinct register counts).
            // This is the default for recent React Native versions (0.75+).
            FunctionHeaderLayout::Modern12 => {
                let raw = reader.read_bytes(12)?;
                let mut bytes = [0u8; 16];
                bytes[..12].copy_from_slice(raw);
                let raw = u128::from_le_bytes(bytes);

                // Modern12 bitfield map, (bit offset, width) within the 96-bit word:
                //   offset                  : ( 0, 25)
                //   param_count             : (25,  5)
                //   loop_depth              : (30,  2)
                //   bytecode_size_in_bytes  : (32, 14)
                //   function_name           : (46,  8)
                //   number_reg_count        : (54,  5)
                //   non_ptr_reg_count       : (59,  5)
                //   frame_size              : (64,  8)
                //   read_cache_size         : (72,  8)
                //   write_cache_size        : (80,  6 on v98 / 7 on v99)
                //   num_cache_new_object    : (86,  1) -- v98 only, gone at v99
                //   private_name_cache_size : (87,  1)
                //   flags                   : (88,  8)
                // Only the write_cache/num_cache_new_object split moves between
                // the supported versions; everything else is fixed, which is why
                // `flags` at bit 88 is reliable in the *small* header.
                let offset = (raw & ((1u128 << 25) - 1)) as u32;
                let param_count = ((raw >> 25) & ((1u128 << 5) - 1)) as u32;
                let loop_depth = ((raw >> 30) & ((1u128 << 2) - 1)) as u32;
                let bytecode_size_in_bytes = ((raw >> 32) & ((1u128 << 14) - 1)) as u32;
                let function_name = ((raw >> 46) & ((1u128 << 8) - 1)) as u32;
                let number_reg_count = ((raw >> 54) & ((1u128 << 5) - 1)) as u32;
                let non_ptr_reg_count = ((raw >> 59) & ((1u128 << 5) - 1)) as u32;
                let frame_size = ((raw >> 64) & 0xff) as u32;
                let read_cache_size = ((raw >> 72) & 0xff) as u8;
                let layout = modern_layout.expect("Modern12 layout resolved above");
                let wc_bits = layout.small_write_cache_bits();
                let write_cache_size = ((raw >> 80) & ((1u128 << wc_bits) - 1)) as u8;
                let num_cache_new_object = match layout.large_num_cache_new_object_pos() {
                    Some(_) => ((raw >> 86) & 0x1) as u8,
                    None => 0,
                };
                let private_name_cache_size = ((raw >> 87) & 0x1) as u8;
                let flags = ((raw >> 88) & 0xff) as u8;

                if flags & FLAG_OVERFLOWED != 0 {
                    let large_offset = ((function_name as u64) << MODERN_LARGE_OFFSET_SHIFT)
                        | (offset as u64 & MODERN_LARGE_OFFSET_MASK);
                    let large_header = parse_large_header_modern(
                        reader,
                        large_offset as usize,
                        function_id,
                        layout,
                    )?;
                    reader.seek(current_pos + 12)?;
                    FunctionHeader::Modern(large_header)
                } else {
                    // Not overflowed: a 12-byte small header has no FunctionInfo
                    // section, so info_offset is 0 (no exception handlers).
                    FunctionHeader::Modern(ModernFunctionHeader {
                        function_id,
                        offset,
                        param_count,
                        loop_depth,
                        bytecode_size_in_bytes,
                        function_name,
                        number_reg_count,
                        non_ptr_reg_count,
                        frame_size,
                        read_cache_size,
                        write_cache_size,
                        num_cache_new_object,
                        private_name_cache_size,
                        flags,
                        info_offset: 0,
                    })
                }
            }
        };
        headers.push(function_header);
    }
    Ok(headers)
}

fn parse_large_header_legacy(
    reader: &mut ByteReader<'_>,
    offset: usize,
    function_id: u32,
) -> Result<LegacyFunctionHeader> {
    let current = reader.position();
    reader.seek(offset)?;
    let header = LegacyFunctionHeader {
        function_id,
        offset: reader.read_u32()?,
        param_count: reader.read_u32()?,
        bytecode_size_in_bytes: reader.read_u32()?,
        function_name: reader.read_u32()?,
        info_offset: reader.read_u32()?,
        frame_size: reader.read_u32()?,
        environment_size: reader.read_u32()?,
        highest_read_cache_index: reader.read_u8()? as u32,
        highest_write_cache_index: reader.read_u8()? as u32,
        // Hermes' `SmallFuncHeader(uint32_t largeHeaderOffset)` zeroes the whole
        // small header and sets only `Overflowed`, so the large header never
        // carries that bit -- at either layout. The modern path reinstates it
        // (see `parse_large_header_modern`); this one did not, so every accessor
        // built on `flags()` reported "not overflowed" for a legacy function that
        // plainly is. Measured on a shipped v96 bundle: 15 small headers carry
        // the bit, `is_overflowed()` reported 0, and six functions had a
        // `frame_size` above 127 -- impossible in the small header's 7-bit field,
        // so demonstrably read from a large one.
        //
        // The visible cost was cosmetic (`function_info` never printed
        // "overflowed"), but `write::serialize::has_overflowed_functions` reads
        // the same bit, and legacy is the v96 case -- the one this repo patches.
        flags: reader.read_u8()? | FLAG_OVERFLOWED,
    };
    reader.seek(current)?;
    Ok(header)
}

fn parse_large_header_modern(
    reader: &mut ByteReader<'_>,
    offset: usize,
    function_id: u32,
    layout: ModernLayout,
) -> Result<ModernFunctionHeader> {
    let current = reader.position();

    // Read the whole header as one slice and index it through the layout, rather
    // than streaming fields in declaration order. The u8 tail after the eight u32s
    // is exactly what changed at v99 (NumCacheNewObject was removed), so reading
    // sequentially is what made the old code silently take `flags` from one byte
    // past the header.
    reader.seek(offset)?;
    let raw = reader.read_bytes(layout.large_size())?;
    let u32_at = |pos: usize| -> u32 {
        u32::from_le_bytes(raw[pos..pos + 4].try_into().expect("4 bytes in range"))
    };

    let header = ModernFunctionHeader {
        function_id,
        offset: u32_at(MODERN_LARGE_OFFSET),
        param_count: u32_at(MODERN_LARGE_PARAM_COUNT),
        loop_depth: u32_at(MODERN_LARGE_LOOP_DEPTH),
        bytecode_size_in_bytes: u32_at(MODERN_LARGE_BYTECODE_SIZE),
        function_name: u32_at(MODERN_LARGE_FUNCTION_NAME),
        number_reg_count: u32_at(MODERN_LARGE_NUMBER_REG_COUNT),
        non_ptr_reg_count: u32_at(MODERN_LARGE_NON_PTR_REG_COUNT),
        frame_size: u32_at(MODERN_LARGE_FRAME_SIZE),
        read_cache_size: raw[layout.large_read_cache_size_pos()],
        write_cache_size: raw[layout.large_write_cache_size_pos()],
        num_cache_new_object: layout
            .large_num_cache_new_object_pos()
            .map_or(0, |p| raw[p]),
        private_name_cache_size: raw[layout.large_private_name_cache_size_pos()],
        // Hermes' `SmallFuncHeader(uint32_t largeHeaderOffset)` zeroes the whole
        // small header and sets only `Overflowed`, so the large header never
        // carries that bit. Reinstate it here, otherwise every accessor built on
        // `flags()` -- `is_overflowed`, `has_overflowed_functions` -- reports
        // "not overflowed" for a modern function that plainly is. The remaining
        // bits are the large header's own, which is where the VM reads them from.
        flags: raw[layout.large_flags_pos()] | crate::format::FLAG_OVERFLOWED,
        // The FunctionInfo (exception handler table, then debug offsets) is laid
        // out immediately after the large header, 4-byte aligned. HBC >=97 small
        // headers carry no info_offset field, so a function with exception
        // handlers / debug info is emitted overflowed and its info section is
        // located here. Size is version-dependent -- see ModernLayout.
        info_offset: layout.info_offset_for(offset) as u32,
    };

    reader.seek(current)?;
    Ok(header)
}
