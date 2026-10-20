// The one relocation primitive: after a write op splices a single region of the
// image, shift every absolute offset that pointed past it.
//
// Every size-changing op in the write path has the same shape -- replace one
// region (a function body, or the whole string region) with one of a different
// size, then move everything after it by that size difference. This used to be
// written out three times by hand (plan_guides/06_write/relocation/PLAN.md,
// R26); it is written once here.
//
// The absolute-offset surface of the format is small and fixed (see the plan's
// "Derived facts"): per function, the body offset and the legacy info offset in
// the small header, or for an overflowed function the pointer to its out-of-line
// large header plus that header's own body and (legacy) info offsets; and the
// file header's `debug_info_offset`. `file_length` and the footer are
// `finalize_raw_image`'s. Nothing else is touched: `cjs_module_offset` is a
// module id, not a file offset, and handler tables and debug streams are
// body-relative, so a whole-region shift leaves them valid.

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::format::{BytecodeHeader, FunctionHeaderLayout, FLAG_OVERFLOWED};
use crate::modern_layout::{ModernLayout, MODERN_LARGE_BYTECODE_SIZE};
use crate::write::header_write as hw;
use crate::write::serialize::section_offset;

// Field widths of the small headers (file/parser/function.rs). A value that does
// not fit is refused rather than masked: masking is how a resize used to corrupt
// a header silently.
const OFFSET_BITS: u32 = 25;
const LEGACY_SIZE_BITS: u32 = 15;
const MODERN_SIZE_BITS: u32 = 14;

/// One spliced region, and the size change it caused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloc {
    /// The first offset, in the **old** image, that moved: the end of the region
    /// that was replaced. Every absolute offset `>= moved_from` shifts by `delta`;
    /// everything before it, including the start of the spliced region itself,
    /// stays put.
    pub moved_from: usize,
    /// Signed size change of the region. Must be a multiple of 4 (I5).
    pub delta: i64,
    /// `(function_id, new body size)` when the spliced region was a function body.
    pub resized: Option<(u32, u32)>,
}

/// Shift every absolute offset in `buf` that pointed at or past `r.moved_from`.
///
/// `buf` is the image *after* the splice. Offsets are read from its bytes, never
/// from `file`; `file` supplies only the layout (header layout, version, section
/// starts). Function header slots must lie before the splice, which holds for
/// every op today: the header table precedes both the string region and the
/// code. The model is not updated -- `commit_image` re-derives it (I1).
pub fn relocate(file: &BytecodeFile, buf: &mut [u8], r: &Reloc) -> Result<()> {
    // A non-4-aligned delta misaligns every downstream large header and the
    // FunctionInfo region that follows the code (I5).
    if r.delta % 4 != 0 {
        return Err(Error::Write(format!(
            "relocation delta {} is not a multiple of 4: everything after the splice \
             must stay 4-byte aligned (large headers, FunctionInfo)",
            r.delta
        )));
    }

    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    // Version-keyed byte layout of the out-of-line large header. Resolved once so
    // an unsupported modern version fails here rather than mis-encoding (R8/R15).
    let layout = match file.header.function_header_layout {
        FunctionHeaderLayout::Legacy16 => None,
        FunctionHeaderLayout::Modern12 => Some(ModernLayout::for_version(file.header.version)?),
    };
    let hsize = if layout.is_some() { 12 } else { 16 };
    if fh_sec + file.function_headers.len() * hsize > r.moved_from {
        return Err(Error::Write(
            "relocation: the function header table overlaps the spliced region".into(),
        ));
    }
    if let Some((id, _)) = r.resized {
        if id as usize >= file.function_headers.len() {
            return Err(Error::Write(format!(
                "relocation: invalid function id {id}"
            )));
        }
    }

    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * hsize;
        let new_size = r
            .resized
            .filter(|(id, _)| *id as usize == i)
            .map(|(_, s)| s);
        let flags = buf[slot + hsize - 1];
        if flags & FLAG_OVERFLOWED != 0 {
            relocate_overflowed(buf, slot, layout, r, new_size)?;
        } else if layout.is_some() {
            relocate_modern_small(&mut buf[slot..slot + 12], r, new_size)?;
        } else {
            relocate_legacy_small(&mut buf[slot..slot + 16], r, new_size)?;
        }
    }

    // The debug info section sits after the code, so its header offset shifts too.
    let dpos = debug_info_offset_pos(&file.header, layout.is_some());
    if dpos + 4 > buf.len() {
        return Err(Error::Write(
            "relocation: debug_info_offset out of range".into(),
        ));
    }
    let dbg = u32::from_le_bytes(buf[dpos..dpos + 4].try_into().unwrap()) as usize;
    if dbg != 0 && dbg >= r.moved_from {
        hw::shift_u32_at(buf, dpos, r.delta)?;
    }
    Ok(())
}

// Byte position of `debug_info_offset` in the file header. Modern headers keep it
// at a fixed byte 108; legacy ones mirror the field order `write_legacy_header`
// writes, which depends on which optional fields this version has.
pub(crate) fn debug_info_offset_pos(header: &BytecodeHeader, modern: bool) -> usize {
    if modern {
        return 108;
    }
    let mut pos = 64usize;
    if header.big_int_count.is_some() {
        pos += 8; // big_int_count + big_int_storage_size
    }
    pos += 8; // reg_exp_count + reg_exp_storage_size
    pos += 12; // array_buffer_size + obj_key_buffer_size + obj_value_buffer_size
    pos += 8; // segment_id/cjs_module_offset + cjs_module_count
    if header.function_source_count.is_some() {
        pos += 4; // function_source_count
    }
    pos
}

// The shifted value of an absolute offset, or the offset unchanged when it sits
// before the splice. Fails when the result does not fit in `bits`.
fn shifted(value: usize, r: &Reloc, bits: u32, what: &str) -> Result<usize> {
    if value < r.moved_from {
        return Ok(value);
    }
    let new = value as i64 + r.delta;
    if new < 0 || new >= 1i64 << bits {
        return Err(Error::Write(format!(
            "relocation: {what} {value} shifted by {} does not fit in {bits} bits",
            r.delta
        )));
    }
    Ok(new as usize)
}

fn checked_size(size: u32, bits: u32) -> Result<u128> {
    if size as u64 >= 1u64 << bits {
        return Err(Error::Write(format!(
            "relocation: body size {size} does not fit the small header's {bits}-bit \
             size field (the function would have to become overflowed)"
        )));
    }
    Ok(size as u128)
}

fn set_bits(raw: u128, lo: u32, bits: u32, value: u128) -> u128 {
    let mask = ((1u128 << bits) - 1) << lo;
    (raw & !mask) | ((value << lo) & mask)
}

// Legacy16 small header: body offset at bits 0..25, size at 32..47, info offset
// at 64..89 (0 means none). Edited in place; no other field is re-encoded.
fn relocate_legacy_small(slot: &mut [u8], r: &Reloc, new_size: Option<u32>) -> Result<()> {
    let mut raw = u128::from_le_bytes(slot[..16].try_into().unwrap());
    let off = (raw & ((1u128 << OFFSET_BITS) - 1)) as usize;
    raw = set_bits(
        raw,
        0,
        OFFSET_BITS,
        shifted(off, r, OFFSET_BITS, "body offset")? as u128,
    );
    let info = ((raw >> 64) & ((1u128 << OFFSET_BITS) - 1)) as usize;
    if info != 0 {
        let new_info = shifted(info, r, OFFSET_BITS, "info offset")?;
        raw = set_bits(raw, 64, OFFSET_BITS, new_info as u128);
    }
    if let Some(sz) = new_size {
        raw = set_bits(
            raw,
            32,
            LEGACY_SIZE_BITS,
            checked_size(sz, LEGACY_SIZE_BITS)?,
        );
    }
    slot[..16].copy_from_slice(&raw.to_le_bytes());
    Ok(())
}

// Modern12 small header: body offset at bits 0..25, size at 32..46. Modern small
// headers store no info offset.
fn relocate_modern_small(slot: &mut [u8], r: &Reloc, new_size: Option<u32>) -> Result<()> {
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let mut raw = u128::from_le_bytes(bytes);
    let off = (raw & ((1u128 << OFFSET_BITS) - 1)) as usize;
    raw = set_bits(
        raw,
        0,
        OFFSET_BITS,
        shifted(off, r, OFFSET_BITS, "body offset")? as u128,
    );
    if let Some(sz) = new_size {
        raw = set_bits(
            raw,
            32,
            MODERN_SIZE_BITS,
            checked_size(sz, MODERN_SIZE_BITS)?,
        );
    }
    slot[..12].copy_from_slice(&raw.to_le_bytes()[..12]);
    Ok(())
}

// An overflowed function's small header holds only a packed pointer to its
// out-of-line large header, which lives after the code. Shift the pointer, then
// the large header's own body offset (its first u32) and, for legacy, its info
// offset at +16; modern large headers compute the info offset, nothing stored.
fn relocate_overflowed(
    buf: &mut [u8],
    slot: usize,
    layout: Option<ModernLayout>,
    r: &Reloc,
    new_size: Option<u32>,
) -> Result<()> {
    let modern = layout.is_some();
    let large_size = layout.map_or(20, |l| l.large_size());
    let ptr = if modern {
        hw::read_modern_large_pointer(&buf[slot..slot + 12])?
    } else {
        hw::read_legacy_large_pointer(&buf[slot..slot + 16])?
    } as usize;
    // Both packed pointers carry at least 32 bits of offset.
    let lh = shifted(ptr, r, 32, "large header pointer")?;
    if lh != ptr {
        if modern {
            hw::shift_modern_large_pointer(&mut buf[slot..slot + 12], r.delta)?;
        } else {
            hw::shift_legacy_large_pointer(&mut buf[slot..slot + 16], r.delta)?;
        }
    }
    if lh + large_size > buf.len() {
        return Err(Error::Write(format!(
            "large header at {lh} (+{large_size} bytes) is out of range for a {}-byte image",
            buf.len()
        )));
    }

    let u32_at = |buf: &[u8], pos: usize| u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
    let body = u32_at(buf, lh) as usize;
    let new_body = shifted(body, r, 32, "large header body offset")?;
    buf[lh..lh + 4].copy_from_slice(&(new_body as u32).to_le_bytes());
    if !modern {
        let info = u32_at(buf, lh + 16) as usize;
        if info != 0 {
            let new_info = shifted(info, r, 32, "large header info offset")?;
            buf[lh + 16..lh + 20].copy_from_slice(&(new_info as u32).to_le_bytes());
        }
    }
    if let Some(sz) = new_size {
        let pos = lh
            + if modern {
                MODERN_LARGE_BYTECODE_SIZE
            } else {
                8
            };
        buf[pos..pos + 4].copy_from_slice(&sz.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::file::BytecodeFile;
    use crate::opcode::BytecodeFormat;
    use crate::write::patch::patch_function_bytes;

    // The first non-overflowed function's body grown to `new_len` bytes by
    // appending AsyncBreakCheck, or `None` when the fixture is absent and optional.
    // An overflowed function keeps its size in a 32-bit large header, so only a
    // small header has a size field to overflow.
    fn grow_first_small(
        rel: &str,
        new_len: impl Fn(usize) -> usize,
    ) -> Option<crate::Result<(Vec<u8>, usize)>> {
        let path = format!(
            "{}/../../examples/react-native/{rel}",
            env!("CARGO_MANIFEST_DIR")
        );
        if !crate::write::corpus_fixture_present(&path) {
            return None;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let abc = format
            .definitions
            .iter()
            .find(|d| d.name == "AsyncBreakCheck")
            .unwrap()
            .opcode;
        let id = file
            .function_headers
            .iter()
            .position(|h| !h.is_overflowed())
            .expect("a non-overflowed function");
        let h = &file.function_headers[id];
        let (off, size) = (h.offset() as usize, h.bytecode_size_in_bytes() as usize);
        let mut body = bytes[off..off + size].to_vec();
        body.resize(new_len(size), abc);
        Some(patch_function_bytes(&mut file, id as u32, &body).map(|out| (out, id)))
    }

    // The smallest length >= `at_least` whose delta from `size` keeps I5.
    fn aligned(size: usize, at_least: usize) -> usize {
        at_least + (4 - (at_least - size) % 4) % 4
    }

    #[test]
    fn a_delta_that_breaks_alignment_is_refused() {
        let Some(res) = grow_first_small("v96/expressions/generator/bytecode.hbc", |s| s + 1)
        else {
            return;
        };
        let err = res.expect_err("a 1-byte grow must be refused").to_string();
        assert!(err.contains("multiple of 4"), "{err}");
    }

    // A size the small header's field cannot hold used to be masked to its low
    // bits, leaving a header that reparsed with the wrong size.
    #[test]
    fn a_size_past_the_legacy_small_field_is_refused() {
        let Some(res) = grow_first_small("v96/expressions/generator/bytecode.hbc", |s| {
            aligned(s, 1 << 15)
        }) else {
            return;
        };
        let err = res
            .expect_err("a 32 KiB body cannot fit 15 bits")
            .to_string();
        assert!(err.contains("15-bit"), "{err}");
    }

    #[test]
    fn a_size_past_the_modern_small_field_is_refused() {
        let Some(res) = grow_first_small("v98/bytecode.hbc", |s| aligned(s, 1 << 14)) else {
            return;
        };
        let err = res
            .expect_err("a 16 KiB body cannot fit 14 bits")
            .to_string();
        assert!(err.contains("14-bit"), "{err}");
    }

    #[test]
    fn the_largest_size_that_fits_is_accepted() {
        // Aligned down from the field maximum, so it is the largest legal grow.
        let Some(res) = grow_first_small("v96/expressions/generator/bytecode.hbc", |s| {
            let max = (1 << 15) - 1;
            max - (max - s) % 4
        }) else {
            return;
        };
        let (out, id) = res.expect("a body that fits must be accepted");
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.function_headers[id].bytecode_size_in_bytes() > (1 << 15) - 5);
    }
}
