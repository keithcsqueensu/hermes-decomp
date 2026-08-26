// Replace whole function bodies. Same-size bodies patch in place; different-size
// bodies splice the code section, shift later function offsets, and fix the
// debug-info offset. Legacy, non-overflowed headers.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::format::FunctionHeader;
use crate::opcode::BytecodeFormat;

use crate::write::encode::encode_function_body;
use crate::write::serialize::{commit_image, section_offset};

use super::strings::legacy_debug_info_offset_pos;
use super::PatchOptions;

// Replace the instruction stream of `function_id`. Same-size bodies patch in place;
// longer bodies expand the code section and shift subsequent function offsets.
pub fn patch_function_body(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    new_body: &[Instruction],
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let old_size = file
        .function_headers
        .get(function_id as usize)
        .map(|h| h.bytecode_size_in_bytes() as usize)
        .unwrap_or(0);
    let encoded = encode_function_body(format, new_body)?;
    let delta = encoded.len() as i64 - old_size as i64;

    // A size-changing edit shifts every body-relative offset. Exception-handler
    // tables (start/end/target) are body-relative and are NOT relocated yet
    // (WRITE_PATH_GUIDE Q3), so refuse to resize a function that declares one rather than
    // ship stale handler offsets. Keyed on the parser's own "has handlers" gate,
    // FLAG_HAS_EXCEPTION_HANDLER (bit 3) — this is precise for both layouts and,
    // unlike `info_offset != 0`, does not over-reject debug-only legacy functions
    // or (fatally) every overflowed modern function. Same-size edits are allowed:
    // they neither move the table nor shift body offsets.
    if delta != 0 {
        if let Some(fh) = file.function_headers.get(function_id as usize) {
            if fh.flags() & crate::format::FLAG_HAS_EXCEPTION_HANDLER != 0 {
                return Err(Error::Write(format!(
                    "function {function_id} has an exception-handler table; \
                     size-changing edits are not supported (handler offsets are \
                     body-relative and would be left stale). See WRITE_PATH_GUIDE Q3."
                )));
            }
        }
        // The same defect in a second structure (R24). A location stream stores
        // bytecode addresses *within* the function as SLEB128 deltas, and a resize
        // rewrites none of them, so every location past the edit point maps to the
        // wrong instruction -- silently, since the stream still decodes and still
        // terminates. Checked after the handler guard so a function carrying both
        // reports the handler reason, which is the one that breaks execution rather
        // than only debugging.
        //
        // Ordinary React Native bundles are unaffected: FLAG_HAS_DEBUG_INFO is set
        // on 0 of the Equinox bundle's 62,909 functions [measured]. This fires on
        // debug-built bundles, which is where it should.
        // `debug_info_offset == 0` means the file carries no debug section at all, so
        // there is nothing for the edit to invalidate and refusing would be theatre.
        // That case is not hypothetical: `create` emits no debug info but sets flags
        // `0x12` on its legacy global function, which includes FLAG_HAS_DEBUG_INFO --
        // the image claims debug info it does not have (the modern path emits `0x22`
        // and does not claim it). Keying on both means the guard follows the data
        // rather than a flag that can be wrong.
        if !options.allow_stale_debug_info && file.header.debug_info_offset != 0 {
            if let Some(fh) = file.function_headers.get(function_id as usize) {
                if fh.flags() & crate::format::FLAG_HAS_DEBUG_INFO != 0 {
                    return Err(Error::Write(format!(
                        "function {function_id} carries debug info; size-changing \
                         edits are not supported (its source locations are \
                         body-relative and would be left pointing at the wrong \
                         instructions). Pass allow_stale_debug_info / \
                         --allow-stale-debug-info to discard that function's line \
                         numbers and proceed. See WRITE_PATH_GUIDE R24."
                    )));
                }
            }
        }
    }

    // Keep the size delta a multiple of 4 so the FunctionInfo region that follows
    // the code stays 4 byte aligned. Pad with the 1 byte AsyncBreakCheck (a runtime
    // no-op) inserted just before the terminator so the function still ends on a
    // terminating instruction. When padding is required but this bytecode version
    // has no AsyncBreakCheck to pad with, fail loudly rather than silently emit a
    // non-4-aligned delta that misaligns every downstream large header (I5 / Q8).
    if delta != 0 && delta.rem_euclid(4) != 0 {
        let Some(op_abc) = format
            .definitions
            .iter()
            .find(|d| d.name == "AsyncBreakCheck")
            .map(|d| d.opcode)
        else {
            return Err(Error::Write(format!(
                "cannot 4-byte-align function {function_id} body (size delta {delta} \
                 is not a multiple of 4) — this bytecode version has no \
                 AsyncBreakCheck instruction to pad with"
            )));
        };
        let pad = (4 - delta.rem_euclid(4)) as usize;
        let mut padded = new_body.to_vec();
        let insert_at = padded.len().saturating_sub(1);
        for _ in 0..pad {
            padded.insert(
                insert_at,
                Instruction {
                    offset: 0,
                    opcode: op_abc,
                    operands: vec![],
                    length: 1,
                },
            );
        }
        let encoded = encode_function_body(format, &padded)?;
        return patch_function_bytes(file, function_id, &encoded);
    }
    patch_function_bytes(file, function_id, &encoded)
}

// Low-level: replace function body with raw bytes.
pub fn patch_function_bytes(
    file: &mut BytecodeFile,
    function_id: u32,
    new_body: &[u8],
) -> Result<Vec<u8>> {
    let (old_size, abs_off, patched_offset) = {
        let fh = file
            .function_headers
            .get(function_id as usize)
            .ok_or_else(|| Error::Write(format!("invalid function id {function_id}")))?;
        (
            fh.bytecode_size_in_bytes() as usize,
            fh.offset() as usize,
            fh.offset(),
        )
    };
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    if new_body.len() == old_size {
        if abs_off + old_size > raw.len() {
            return Err(Error::Write("function body out of range".into()));
        }
        raw[abs_off..abs_off + old_size].copy_from_slice(new_body);
        // Update instructions cache
        let rel = abs_off
            .checked_sub(file.instruction_offset as usize)
            .ok_or_else(|| Error::Write("offset underflow".into()))?;
        if rel + old_size <= file.instructions.len() {
            file.instructions[rel..rel + old_size].copy_from_slice(new_body);
        }
        let out = commit_image(file, raw)?;
        return Ok(out);
    }

    // Grow / shrink. Function bodies form one contiguous region followed by the
    // FunctionInfo region (large headers, exception tables, debug info), which is
    // 4 byte aligned. Callers align the body delta to a multiple of 4 so the whole
    // tail shifts by a 4 aligned amount and every large header stays aligned.
    let delta = new_body.len() as i64 - old_size as i64;
    if abs_off + old_size > raw.len() {
        return Err(Error::Write("function body out of range".into()));
    }

    // Splice body
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..abs_off]);
    rebuilt.extend_from_slice(new_body);
    rebuilt.extend_from_slice(&raw[abs_off + old_size..]);

    // Patch function headers section: update this function size and all later offsets.
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let header_size = match file.header.function_header_layout {
        crate::format::FunctionHeaderLayout::Legacy16 => 16,
        crate::format::FunctionHeaderLayout::Modern12 => 12,
    };

    // Everything at or after the end of the patched body moved by `delta`. For
    // each function we shift its body offset (except the patched one, whose body
    // did not move but whose size changed) and, when overflowed, relocate the
    // large header and its internal offset / size / info fields.
    let threshold = abs_off + old_size;
    let modern = header_size == 12;
    // Version-keyed byte layout of the out-of-line large header. Resolved once so
    // an unsupported modern version fails here rather than mis-encoding silently
    // (WRITE_PATH_GUIDE R8/R15).
    let layout = if modern {
        Some(crate::modern_layout::ModernLayout::for_version(file.header.version)?)
    } else {
        None
    };
    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * header_size;
        if slot + header_size > rebuilt.len() {
            break;
        }
        let flag_byte = if modern { 11 } else { 15 };
        let overflowed = rebuilt[slot + flag_byte] & crate::format::FLAG_OVERFLOWED != 0;
        let is_target = i as u32 == function_id;
        if overflowed {
            resize_overflowed_function(
                &mut rebuilt,
                slot,
                layout,
                threshold,
                delta,
                is_target.then_some(new_body.len() as u32),
            )?;
        } else if modern {
            // Body offset lives in the 12-byte header (bits 0..24) and size in
            // bits 32..45.
            resize_modern_small(
                &mut rebuilt[slot..slot + 12],
                threshold,
                delta,
                is_target.then_some(new_body.len() as u32),
            )?;
        } else {
            let leg = match &file.function_headers[i] {
                FunctionHeader::Legacy(l) => l,
                _ => unreachable!(),
            };
            let new_offset = if leg.offset as usize >= threshold {
                (leg.offset as i64 + delta) as u32
            } else {
                leg.offset
            };
            let new_size = if is_target {
                new_body.len() as u32
            } else {
                leg.bytecode_size_in_bytes
            };
            let new_info = if leg.info_offset != 0 && leg.info_offset as usize >= threshold {
                (leg.info_offset as i64 + delta) as u32
            } else {
                leg.info_offset
            };
            let bytes = crate::write::header_write::write_function_header_legacy_small(
                new_offset,
                leg.param_count,
                new_size,
                leg.function_name,
                new_info,
                leg.frame_size,
                leg.environment_size,
                leg.highest_read_cache_index,
                leg.highest_write_cache_index,
                leg.flags,
            );
            rebuilt[slot..slot + 16].copy_from_slice(&bytes);
        }
    }
    let _ = patched_offset;

    // The debug info section sits after the code, so its header offset shifts too.
    if file.header.debug_info_offset != 0 {
        let dpos = if modern {
            108
        } else {
            legacy_debug_info_offset_pos(&file.header)
        };
        let shifted = (file.header.debug_info_offset as i64 + delta) as u32;
        if dpos + 4 <= rebuilt.len() {
            rebuilt[dpos..dpos + 4].copy_from_slice(&shifted.to_le_bytes());
        }
        file.header.debug_info_offset = shifted;
    }

    // Update instruction cache roughly
    file.instructions = rebuilt[file.instruction_offset as usize..].to_vec();
    // Drop footer if present from old slice, finalize will rehash
    let out = commit_image(file, rebuilt)?;
    Ok(out)
}

// Shift the body offset (bits 0..24) of a non-overflowed Modern12 small header
// when it sits at or past `threshold`, and optionally set the size (bits 32..45).
fn resize_modern_small(
    slot: &mut [u8],
    threshold: usize,
    delta: i64,
    new_size: Option<u32>,
) -> Result<()> {
    if slot.len() < 12 {
        return Err(Error::Write("modern header slot too small".into()));
    }
    let mut bytes = [0u8; 16];
    bytes[..12].copy_from_slice(&slot[..12]);
    let mut raw = u128::from_le_bytes(bytes);
    let off = (raw & ((1u128 << 25) - 1)) as usize;
    if off >= threshold {
        let new_off = (off as i64 + delta) as u128 & ((1u128 << 25) - 1);
        raw = (raw & !((1u128 << 25) - 1)) | new_off;
    }
    if let Some(sz) = new_size {
        raw &= !(((1u128 << 14) - 1) << 32);
        raw |= ((sz as u128) & ((1u128 << 14) - 1)) << 32;
    }
    let out = raw.to_le_bytes();
    slot[..12].copy_from_slice(&out[..12]);
    Ok(())
}

// Relocate an overflowed function during a body resize: shift the small header
// pointer and the large header's body offset when they sit past `threshold`, set
// the size when this is the patched function, and shift the legacy info offset.
// `layout` is `Some` for Modern images and carries that version's large-header
// byte layout; `None` means Legacy16.
fn resize_overflowed_function(
    rebuilt: &mut [u8],
    slot: usize,
    layout: Option<crate::modern_layout::ModernLayout>,
    threshold: usize,
    delta: i64,
    new_size: Option<u32>,
) -> Result<()> {
    use crate::modern_layout::MODERN_LARGE_BYTECODE_SIZE;
    use crate::write::header_write as hw;
    let modern = layout.is_some();
    // Legacy large headers are a fixed 20 bytes up to and including info_offset
    // at +16; modern is version-dependent.
    let large_size = layout.map_or(20, |l| l.large_size());
    let large_ptr = if modern {
        hw::read_modern_large_pointer(&rebuilt[slot..slot + 12])?
    } else {
        hw::read_legacy_large_pointer(&rebuilt[slot..slot + 16])?
    } as usize;
    let new_lh = if large_ptr >= threshold {
        if modern {
            hw::shift_modern_large_pointer(&mut rebuilt[slot..slot + 12], delta)?;
        } else {
            hw::shift_legacy_large_pointer(&mut rebuilt[slot..slot + 16], delta)?;
        }
        (large_ptr as i64 + delta) as usize
    } else {
        large_ptr
    };
    if new_lh + large_size > rebuilt.len() {
        return Err(Error::Write(format!(
            "large header at {new_lh} (+{large_size} bytes) is out of range for a              {}-byte image",
            rebuilt.len()
        )));
    }
    // Body offset is the first u32; shift it if the body moved.
    let body_off = u32::from_le_bytes(rebuilt[new_lh..new_lh + 4].try_into().unwrap()) as usize;
    if body_off >= threshold {
        hw::shift_u32_at(rebuilt, new_lh, delta)?;
    }
    // Size field: legacy at +8, modern at MODERN_LARGE_BYTECODE_SIZE.
    if let Some(sz) = new_size {
        let size_pos = new_lh + if modern { MODERN_LARGE_BYTECODE_SIZE } else { 8 };
        rebuilt[size_pos..size_pos + 4].copy_from_slice(&sz.to_le_bytes());
    }
    // Legacy large headers keep info_offset at +16.
    if !modern {
        let info_pos = new_lh + 16;
        let info = u32::from_le_bytes(rebuilt[info_pos..info_pos + 4].try_into().unwrap()) as usize;
        if info != 0 && info >= threshold {
            hw::shift_u32_at(rebuilt, info_pos, delta)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    #[test]
    fn patch_function_same_size_roundtrip() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let body = file.decode_function_instructions(&format, 0).unwrap();
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        let body2 = re.decode_function_instructions(&format, 0).unwrap();
        assert_eq!(body.len(), body2.len());
    }

    // The fixture-based test above silently skips without a checked-in .hbc. The
    // tests below build a real image with `create_minimal`, so they run in CI and
    // exercise the grow / shrink / alignment-pad / modern-resize branches that
    // WRITE_PATH_GUIDE flags as never independently tested.
    use crate::write::create::{create_minimal, CreateOptions};

    fn make(version: u32) -> (BytecodeFile, BytecodeFormat) {
        let bytes = create_minimal(&CreateOptions {
            version,
            ..Default::default()
        })
        .expect("create_minimal");
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created file");
        let format = BytecodeFormat::for_version_or_latest(version)
            .expect("format")
            .0;
        (file, format)
    }

    // Grow a body by a 4-aligned delta: the size field is rewritten and the image
    // reparses with the larger instruction stream.
    #[test]
    fn grow_body_reparses() {
        let (mut file, format) = make(96);
        let old_size = file.function_headers[0].bytecode_size_in_bytes();
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        // Duplicate the leading (non-terminator) instruction twice → +4 bytes.
        let first = body[0].clone();
        body.insert(0, first.clone());
        body.insert(0, first);
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.function_headers[0].bytecode_size_in_bytes(), old_size + 4);
        let body2 = re.decode_function_instructions(&format, 0).unwrap();
        assert_eq!(body2.len(), body.len());
    }

    // Shrink a previously-grown body: the size field decreases and the image still
    // reparses.
    #[test]
    fn shrink_body_reparses() {
        let (mut file, format) = make(96);
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        let orig_len = body.len();
        let first = body[0].clone();
        body.insert(0, first.clone());
        body.insert(0, first);
        let grown =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        let mut file2 = BytecodeFile::parse_auto(&grown).unwrap();
        let grown_size = file2.function_headers[0].bytecode_size_in_bytes();
        let mut small = file2.decode_function_instructions(&format, 0).unwrap();
        small.drain(0..2); // remove the two duplicates
        assert_eq!(small.len(), orig_len);
        let out =
            patch_function_body(&mut file2, &format, 0, &small, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.function_headers[0].bytecode_size_in_bytes() < grown_size);
    }

    // A non-4-aligned raw delta must be padded (AsyncBreakCheck) so the emitted
    // size delta is a multiple of 4 (I5).
    #[test]
    fn alignment_pad_makes_delta_4_aligned() {
        let (mut file, format) = make(96);
        if !format.definitions.iter().any(|d| d.name == "AsyncBreakCheck") {
            return; // pad instruction unavailable on this version
        }
        let old_size = file.function_headers[0].bytecode_size_in_bytes();
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        // Add one 2-byte instruction → raw delta +2 (not 4-aligned).
        let first = body[0].clone();
        body.insert(0, first);
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        let delta = re.function_headers[0].bytecode_size_in_bytes() as i64 - old_size as i64;
        assert_eq!(delta % 4, 0, "size delta {delta} must be 4-aligned after padding");
        assert!(delta >= 4, "the +2 body should have been padded up to +4, got {delta}");
    }

    // Modern (v98) global is overflowed, so a body resize goes through
    // resize_overflowed_function on the modern large-header layout.
    #[test]
    fn modern_v98_overflowed_resize_reparses() {
        let (mut file, format) = make(98);
        assert!(matches!(
            file.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));
        let old_size = file.function_headers[0].bytecode_size_in_bytes();
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        let first = body[0].clone();
        body.insert(0, first.clone());
        body.insert(0, first);
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.function_headers.len(), file.function_headers.len());
        assert_eq!(re.function_headers[0].bytecode_size_in_bytes(), old_size + 4);
    }

    // A grow must shift debug_info_offset (in the header and in the model) by the
    // body delta. create_minimal images carry no debug info, so this needs a real
    // fixture and skips when one is absent.
    #[test]
    fn debug_info_offset_shifts_on_grow() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !std::path::Path::new(path).exists() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        if file.header.debug_info_offset == 0 {
            return;
        }
        // The Q3 guard rejects size-changing edits on functions that declare an
        // exception-handler table; skip if function 0 happens to have one.
        if file.function_headers[0].flags() & crate::format::FLAG_HAS_EXCEPTION_HANDLER != 0 {
            return;
        }
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let old_debug = file.header.debug_info_offset;
        let old_size = file.function_headers[0].bytecode_size_in_bytes();
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        let first = body[0].clone();
        body.insert(0, first.clone());
        body.insert(0, first);
        let out =
            patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default()).unwrap();
        let re = BytecodeFile::parse_auto(&out).unwrap();
        let delta = re.function_headers[0].bytecode_size_in_bytes() as i64 - old_size as i64;
        assert!(delta > 0);
        assert_eq!(
            re.header.debug_info_offset as i64,
            old_debug as i64 + delta,
            "debug_info_offset in the reparsed image must shift by the body delta"
        );
        assert_eq!(
            file.header.debug_info_offset as i64,
            old_debug as i64 + delta,
            "the in-memory header must be updated too"
        );
    }

    // Q4/Q3 guard: a size-changing edit on a function that declares an
    // exception-handler table is rejected (handler offsets are body-relative and
    // not yet relocated). A same-size edit is still allowed.
    #[test]
    fn size_change_on_function_with_handlers_is_rejected() {
        let (mut file, format) = make(96);
        match &mut file.function_headers[0] {
            FunctionHeader::Legacy(l) => l.flags |= crate::format::FLAG_HAS_EXCEPTION_HANDLER,
            FunctionHeader::Modern(m) => m.flags |= crate::format::FLAG_HAS_EXCEPTION_HANDLER,
        }
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        let first = body[0].clone();
        body.insert(0, first.clone());
        body.insert(0, first);
        let err = patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default())
            .expect_err("size change on a handler function must be rejected");
        assert!(
            err.to_string().contains("exception-handler"),
            "error should mention the exception-handler table, got: {err}"
        );
        // The guard did not mutate the file, so a same-size edit still works.
        let same = file.decode_function_instructions(&format, 0).unwrap();
        patch_function_body(&mut file, &format, 0, &same, &PatchOptions::default())
            .expect("same-size edit on a handler function should be allowed");
    }

    // Q8: when a size-changing edit needs 4-byte padding but the version has no
    // AsyncBreakCheck (v40–60), fail loudly instead of silently emitting a
    // non-4-aligned delta.
    #[test]
    fn missing_asyncbreakcheck_pad_is_hard_error() {
        let (mut file, format) = make(56); // v56 has no AsyncBreakCheck
        assert!(
            !format.definitions.iter().any(|d| d.name == "AsyncBreakCheck"),
            "v56 unexpectedly has AsyncBreakCheck"
        );
        let mut body = file.decode_function_instructions(&format, 0).unwrap();
        // Add one 2-byte instruction → delta +2, not a multiple of 4.
        let first = body[0].clone();
        body.insert(0, first);
        let err = patch_function_body(&mut file, &format, 0, &body, &PatchOptions::default())
            .expect_err("non-4-aligned delta with no AsyncBreakCheck must hard-error");
        assert!(
            err.to_string().contains("AsyncBreakCheck"),
            "error should mention AsyncBreakCheck, got: {err}"
        );
    }
}
