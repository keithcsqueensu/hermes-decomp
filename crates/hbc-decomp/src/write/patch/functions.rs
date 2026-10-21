// Replace whole function bodies. Same-size bodies patch in place; different-size
// bodies splice the code section and hand the shift of everything after it to
// `write::reloc::relocate`. Legacy and modern, overflowed or not.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, ExceptionHandler, Instruction};
use crate::opcode::BytecodeFormat;

use super::handlers::{self, HandlerUpdate};
use super::PatchOptions;
use crate::write::encode::encode_function_body;
use crate::write::reloc::{relocate, Reloc};
use crate::write::serialize::{commit_image_checked, HeaderEdit};

// Replace the instruction stream of `function_id`. Same-size bodies patch in place;
// longer bodies expand the code section and shift subsequent function offsets. A
// function with an exception-handler table keeps it as it is, so its size cannot
// change; `patch_function_body_with` says where the handlers go.
pub fn patch_function_body(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    new_body: &[Instruction],
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    patch_function_body_with(
        file,
        format,
        function_id,
        new_body,
        options,
        HandlerUpdate::Keep,
    )
}

// `patch_function_body`, with what to do about the function's handler table.
pub(crate) fn patch_function_body_with(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    new_body: &[Instruction],
    options: &PatchOptions,
    handlers: HandlerUpdate,
) -> Result<Vec<u8>> {
    let old_size = file
        .function_headers
        .get(function_id as usize)
        .map(|h| h.bytecode_size_in_bytes() as usize)
        .unwrap_or(0);
    let encoded = encode_function_body(format, new_body)?;
    let delta = encoded.len() as i64 - old_size as i64;
    let has_handlers = handlers::has_handlers(file, function_id);

    // Handler offsets are body-relative, so a size-changing edit has to say where
    // they went (Q3): an insertion shifts them, a HASM body states them. One that
    // says neither is refused rather than shipped with stale offsets. Keyed on the
    // parser's own "has handlers" gate, FLAG_HAS_EXCEPTION_HANDLER (bit 3) -- precise
    // for both layouts and, unlike `info_offset != 0`, not over-rejecting
    // debug-only legacy functions or every overflowed modern one.
    match &handlers {
        HandlerUpdate::Keep if delta != 0 && has_handlers => {
            return Err(Error::Write(format!(
                "function {function_id} has an exception-handler table, and this edit \
                 changes the body's size without saying where the handlers went. Give the \
                 HASM `.handler start, end, target` lines (emit-hasm writes them), or keep \
                 the body the same size. See plan_guides/06_write/RISKS.md Q3."
            )));
        }
        HandlerUpdate::Replace(_) if !has_handlers => {
            return Err(Error::Write(format!(
                "function {function_id} has no exception-handler table, and a body edit \
                 cannot add one (it would resize the function's info region)"
            )));
        }
        _ => {}
    }
    if delta != 0 {
        // The same defect in a second structure (R24). A location stream stores
        // bytecode addresses *within* the function as SLEB128 deltas, and a resize
        // rewrites none of them, so every location past the edit point maps to the
        // wrong instruction -- silently, since the stream still decodes and still
        // terminates.
        //
        // Ordinary React Native bundles are unaffected: FLAG_HAS_DEBUG_INFO is set
        // on 0 of the Equinox bundle's 62,909 functions [measured]. This fires on
        // debug-built bundles, which is where it should.
        // `debug_info_offset == 0` means the file carries no debug section at all, so
        // there is nothing for the edit to invalidate and refusing would be theatre.
        // That case was not hypothetical: `create` used to set flags `0x12` on its
        // legacy global function, which includes FLAG_HAS_DEBUG_INFO -- an image
        // claiming debug info it does not have. It no longer does, but a flag can be
        // wrong in any file, so the guard follows the data rather than the flag.
        if !options.allow_stale_debug_info && file.header.debug_info_offset != 0 {
            if let Some(fh) = file.function_headers.get(function_id as usize) {
                if fh.flags() & crate::format::FLAG_HAS_DEBUG_INFO != 0 {
                    return Err(Error::Write(format!(
                        "function {function_id} carries debug info, and replacing a \
                         body wholesale cannot preserve it: source locations are \
                         addresses *within* the function, and there is no mapping \
                         from an old address to a new one when the code is different. \
                         An insertion does have one, which is why `inject-stub` \
                         relocates rather than refusing (R24 P2). Pass \
                         allow_stale_debug_info / --allow-stale-debug-info to discard \
                         this function's line numbers and proceed."
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
    let (encoded, pad_at, pad) = if delta != 0 && delta.rem_euclid(4) != 0 {
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
        let pad_at = encode_function_body(format, &new_body[..insert_at])?.len() as u32;
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
        (encode_function_body(format, &padded)?, pad_at, pad as i64)
    } else {
        (encoded, 0, 0)
    };

    // Where the handlers end up, as offsets in the final body -- when the edit
    // moves them at all. Kept handlers on a same-size body stay untouched.
    let final_delta = encoded.len() as i64 - old_size as i64;
    let table = match handlers {
        _ if !has_handlers => None,
        HandlerUpdate::Keep => None,
        HandlerUpdate::Insert { .. } if final_delta == 0 => None,
        HandlerUpdate::Insert { at } => {
            if pad != 0 {
                return Err(Error::Write(
                    "an insertion must keep the body 4-byte aligned itself".into(),
                ));
            }
            let old = handlers::read_table(
                file.raw_bytes.as_deref().unwrap_or_default(),
                handlers::table_pos(file, function_id)?,
            )?;
            Some(handlers::shifted(&old, at, final_delta))
        }
        // The padding went in at `pad_at`, after the HASM was written.
        HandlerUpdate::Replace(given) => Some(handlers::shifted(&given, pad_at, pad)),
    };
    if let Some(new) = &table {
        handlers::check_within(new, encoded.len() as u32)?;
    }
    patch_function_bytes_with(file, function_id, &encoded, table.as_deref())
}

// Low-level: replace function body with raw bytes. A function with handlers keeps
// them unchanged, so its size cannot change.
pub fn patch_function_bytes(
    file: &mut BytecodeFile,
    function_id: u32,
    new_body: &[u8],
) -> Result<Vec<u8>> {
    if handlers::has_handlers(file, function_id) {
        let old_size = file.function_headers[function_id as usize].bytecode_size_in_bytes();
        if new_body.len() != old_size as usize {
            return Err(Error::Write(format!(
                "function {function_id} has an exception-handler table; patch_function_bytes \
                 cannot change its size (its handler offsets would go stale). See \
                 plan_guides/06_write/RISKS.md Q3."
            )));
        }
    }
    patch_function_bytes_with(file, function_id, new_body, None)
}

// Splice `new_body` in, relocate everything after it, and -- when the function
// has a handler table -- write `handlers` into it, all in one buffer before the
// single commit, which then checks the table reads back as written.
fn patch_function_bytes_with(
    file: &mut BytecodeFile,
    function_id: u32,
    new_body: &[u8],
    handlers: Option<&[ExceptionHandler]>,
) -> Result<Vec<u8>> {
    let (old_size, abs_off) = {
        let fh = file
            .function_headers
            .get(function_id as usize)
            .ok_or_else(|| Error::Write(format!("invalid function id {function_id}")))?;
        (fh.bytecode_size_in_bytes() as usize, fh.offset() as usize)
    };
    let table_before = match handlers {
        Some(_) => Some(handlers::table_pos(file, function_id)?),
        None => None,
    };
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    if abs_off + old_size > raw.len() {
        return Err(Error::Write("function body out of range".into()));
    }
    let moved_from = abs_off + old_size;
    let delta = new_body.len() as i64 - old_size as i64;

    let (mut image, edit) = if delta == 0 {
        raw[abs_off..abs_off + old_size].copy_from_slice(new_body);
        (raw, HeaderEdit::default())
    } else {
        // Grow / shrink. Function bodies form one contiguous region followed by the
        // FunctionInfo region (large headers, exception tables, debug info), which
        // is 4 byte aligned. Callers align the body delta to a multiple of 4 so the
        // whole tail shifts by a 4 aligned amount; `relocate` refuses any other
        // delta (I5).
        let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
        rebuilt.extend_from_slice(&raw[..abs_off]);
        rebuilt.extend_from_slice(new_body);
        rebuilt.extend_from_slice(&raw[moved_from..]);
        // Everything at or after the end of the patched body moved by `delta`.
        // The patched function's own body did not move; only its size changed.
        relocate(
            file,
            &mut rebuilt,
            &Reloc {
                moved_from,
                delta,
                resized: Some((function_id, new_body.len() as u32)),
            },
        )?;
        (rebuilt, HeaderEdit::shifted(delta))
    };

    // The table lies in the FunctionInfo region, after all the code, so it moved
    // with the tail.
    if let (Some(handlers), Some(before)) = (handlers, table_before) {
        if before < moved_from {
            return Err(Error::Write(format!(
                "function {function_id}'s handler table at {before} precedes its body's end"
            )));
        }
        let pos = (before as i64 + delta) as usize;
        handlers::write_table(&mut image, pos, handlers)?;
    }

    commit_image_checked(file, image, edit, |after| {
        let Some(want) = handlers else {
            return Ok(());
        };
        let got = after
            .exception_handlers
            .get(&function_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if got == want {
            Ok(())
        } else {
            Err(Error::Write(format!(
                "function {function_id}'s handler table reads back as {got:?}, not {want:?}"
            )))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::FunctionHeader;
    use crate::write::footer::verify_footer;

    #[test]
    fn patch_function_same_size_roundtrip() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
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
    // plan_guides/06_write/RISKS.md flags as never independently tested.
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
        assert_eq!(
            re.function_headers[0].bytecode_size_in_bytes(),
            old_size + 4
        );
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
        if !format
            .definitions
            .iter()
            .any(|d| d.name == "AsyncBreakCheck")
        {
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
        assert_eq!(
            delta % 4,
            0,
            "size delta {delta} must be 4-aligned after padding"
        );
        assert!(
            delta >= 4,
            "the +2 body should have been padded up to +4, got {delta}"
        );
    }

    // Modern (v98) global is overflowed, so a body resize goes through
    // write::reloc::relocate on the modern large-header layout.
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
        assert_eq!(
            re.function_headers[0].bytecode_size_in_bytes(),
            old_size + 4
        );
    }

    // A grow must shift debug_info_offset (in the header and in the model) by the
    // body delta. create_minimal images carry no debug info, so this needs a real
    // corpus fixture (see `corpus_fixture_present`).
    #[test]
    fn debug_info_offset_shifts_on_grow() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
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
        // hermesc gives function 0 debug info, so a wholesale replacement is refused
        // by default (R24). This test is about the section offset moving, not about
        // the line table surviving, so it takes the documented opt-out.
        let opts = PatchOptions {
            allow_stale_debug_info: true,
            ..Default::default()
        };
        let out = patch_function_body(&mut file, &format, 0, &body, &opts).unwrap();
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
            !format
                .definitions
                .iter()
                .any(|d| d.name == "AsyncBreakCheck"),
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
