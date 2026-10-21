// Inject a stub into a function body: a runtime no-op pad, or a real
// print(<function name>) prologue for entry logging.

use crate::error::{Error, Result};
use crate::file::{BytecodeFile, Instruction};
use crate::format::FunctionHeader;
use crate::opcode::BytecodeFormat;

use crate::modern_layout::{
    MODERN_LARGE_FRAME_SIZE, MODERN_SMALL_FLAGS_POS, MODERN_SMALL_HEADER_SIZE,
};
use crate::write::encode::encode_function_body;
use crate::write::header_write::read_modern_large_pointer;
use crate::write::serialize::{commit_image, section_offset, HeaderEdit};

use super::debug_reloc;
use super::functions::patch_function_body_with;
use super::handlers::HandlerUpdate;
use super::PatchOptions;

// The frame the log stub needs, and whether it may have it.
//
// A call puts the callee's frame header (six slots) and `this` plus its arguments
// in the *top* registers of the caller's frame (`StackFrameLayout.h`), so the
// stub's `Call2` needs the top 8, and its own r0..r3 must sit below them: 12 in
// all. A frame that already has 12 is left alone. A smaller one has to grow --
// but `Call`, `Construct` and their relatives take their arguments "from the end
// of the current frame" (BytecodeList.def), so growing the frame of a function
// that uses one moves its arguments out from under it. That function is refused.
// (The old rule grew every frame by 8, which broke exactly those: a v96 function
// doing `new Error(...)` crashed the engine.)
const LOG_FRAME_NEEDED: u32 = 4 + 6 + 2;

// Whether an opcode reads its arguments from the end of the frame. Every call or
// construct does except those that name their argument registers.
fn reads_args_from_frame_end(name: &str) -> bool {
    (name.starts_with("Call") || name.starts_with("Construct"))
        && !matches!(name, "Call1" | "Call2" | "Call3" | "Call4" | "CallRequire")
}

fn log_frame(format: &BytecodeFormat, body: &[Instruction], frame_now: u32) -> Result<u32> {
    if frame_now >= LOG_FRAME_NEEDED {
        return Ok(frame_now);
    }
    let fixed = body.iter().find_map(|i| {
        let name = format.definitions.get(i.opcode as usize)?.name.as_str();
        reads_args_from_frame_end(name).then_some(name)
    });
    match fixed {
        Some(name) => Err(Error::Write(format!(
            "inject log: the frame has {frame_now} registers and the stub needs \
             {LOG_FRAME_NEEDED}, but growing it would move the arguments this function's \
             {name} reads from the end of its frame"
        ))),
        None => Ok(LOG_FRAME_NEEDED),
    }
}

// Modern functions keep frame_size and the read cache count in the out-of-line
// large header (overflowed) or packed in the 12-byte small header (not
// overflowed). Patch them in `raw_bytes` before the resize splice, which copies
// those bytes verbatim, so the runtime allocates the registers the log call needs.
// Returns the read cache slot index to use for the injected TryGetById.
fn reserve_modern_log_regs(
    file: &mut BytecodeFile,
    function_id: u32,
    new_frame: u32,
) -> Result<u32> {
    let cache_now = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Modern(m)) => m.read_cache_size as u32,
        _ => return Err(Error::Write("inject log: not a modern function".into())),
    };
    if cache_now + 1 > u8::MAX as u32 {
        return Err(Error::Write("inject log: read cache full".into()));
    }
    // Version-keyed large-header layout (plan_guides/06_write/RISKS.md R8/R11). The eight u32
    // fields are the same in every supported modern version, so frame_size and
    // read_cache_size happen not to have moved -- but go through the descriptor
    // so that stays a checked fact rather than a lucky constant.
    let layout = crate::modern_layout::ModernLayout::for_version(file.header.version)?;
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let slot = fh_sec + function_id as usize * 12;
    let raw = file
        .raw_bytes
        .as_mut()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    // The overflow bit lives in the small header (byte 11). For an overflowed
    // function the parsed struct flags come from the large header instead, which
    // does not carry the bit, so read it straight from the small header here.
    let overflowed = raw[slot + MODERN_SMALL_FLAGS_POS] & crate::format::FLAG_OVERFLOWED != 0;
    if overflowed {
        let lp = read_modern_large_pointer(&raw[slot..slot + MODERN_SMALL_HEADER_SIZE])? as usize;
        if lp + layout.large_size() > raw.len() {
            return Err(Error::Write(format!(
                "inject log: large header at {lp} (+{} bytes) is out of range",
                layout.large_size()
            )));
        }
        let frame_pos = lp + MODERN_LARGE_FRAME_SIZE;
        raw[frame_pos..frame_pos + 4].copy_from_slice(&new_frame.to_le_bytes());
        raw[lp + layout.large_read_cache_size_pos()] = (cache_now + 1) as u8;
    } else {
        // Small 12-byte header: frame_size at bits 64..72, read_cache_size at bits
        // 72..80 (the ninth and tenth bytes).
        if new_frame > u8::MAX as u32 {
            return Err(Error::Write(format!(
                "inject log: frame size {new_frame} does not fit the small header's 8 bits"
            )));
        }
        raw[slot + 8] = new_frame as u8;
        raw[slot + 9] = (cache_now + 1) as u8;
    }
    // Keep the parsed struct in sync for any later reads in this session.
    if let Some(FunctionHeader::Modern(m)) = file.function_headers.get_mut(function_id as usize) {
        m.frame_size = new_frame;
        m.read_cache_size = (cache_now + 1) as u8;
    }
    Ok(cache_now)
}

// The legacy counterpart: a non-overflowed Legacy16 small header keeps
// frame_size in 7 bits at bit 89 and highestReadCacheIndex in the byte at bit
// 104. They are written into `raw_bytes`, as the modern ones are. Editing only
// the parsed struct relied on `patch_function_bytes` re-encoding every legacy
// header from the model; once it shifted headers in place instead (R26), the
// bump was silently dropped and the stub wrote registers past the frame -- a
// native crash in the v96 engine, with no JS stack.
//
// Unlike modern's `read_cache_size`, which is a count, the legacy field is the
// *highest index in use*. The stub must take the next index, not that one:
// sharing a slot with the function's own last property read let the cache
// answer that read with `print` (`greet` printed `tag` as `print`, and the
// global called `print` for `twice`). Returns the read cache slot index to use
// for the injected TryGetById.
fn reserve_legacy_log_regs(
    file: &mut BytecodeFile,
    function_id: u32,
    new_frame: u32,
) -> Result<u32> {
    let highest = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Legacy(l)) => l.highest_read_cache_index,
        _ => return Err(Error::Write("inject log: not a legacy function".into())),
    };
    let cache = highest + 1;
    if cache > u8::MAX as u32 {
        return Err(Error::Write("inject log: read cache full".into()));
    }
    if new_frame >= 1 << 7 {
        return Err(Error::Write(format!(
            "inject log: frame size {new_frame} does not fit the legacy small header's 7 bits"
        )));
    }
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let slot = fh_sec + function_id as usize * 16;
    let raw = file
        .raw_bytes
        .as_mut()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let mut bits = u128::from_le_bytes(raw[slot..slot + 16].try_into().unwrap());
    bits = (bits & !(0x7fu128 << 89)) | ((new_frame as u128) << 89);
    bits = (bits & !(0xffu128 << 104)) | ((cache as u128) << 104);
    raw[slot..slot + 16].copy_from_slice(&bits.to_le_bytes());
    Ok(cache)
}

// What an injected operand holds. The opcode table says how it is encoded.
#[derive(Debug, Clone, Copy)]
enum StubVal {
    Reg(u32),
    Cache(u32),
    Str(u32),
}

// One injected instruction, built from the opcode table's own operand list rather
// than an assumed one (R12): the table supplies each operand's type, and a version
// whose shape differs -- another arity, an operand of another kind, or a width too
// narrow for the value -- is refused instead of encoded as if it matched.
fn stub_insn(format: &BytecodeFormat, name: &str, vals: &[StubVal]) -> Result<Instruction> {
    use crate::opcode::{Operand, OperandType as T, OperandValue as V};
    let def = format
        .definitions
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| Error::Write(format!("inject log: no {name} at this bytecode version")))?;
    if def.operand_types.len() != vals.len() {
        return Err(Error::Write(format!(
            "inject log: {name} takes {} operands at this version, the stub supplies {}",
            def.operand_types.len(),
            vals.len()
        )));
    }
    let operands = def
        .operand_types
        .iter()
        .zip(vals)
        .enumerate()
        .map(|(i, (&ty, &val))| {
            let (kind_fits, x, what) = match val {
                StubVal::Reg(x) => (matches!(ty, T::Reg8 | T::Reg32), x, "a register"),
                StubVal::Cache(x) => (
                    matches!(ty, T::UInt8 | T::UInt16 | T::UInt32),
                    x,
                    "a cache index",
                ),
                StubVal::Str(x) => (
                    matches!(ty, T::UInt8S | T::UInt16S | T::UInt32S),
                    x,
                    "a string id",
                ),
            };
            if !kind_fits {
                return Err(Error::Write(format!(
                    "inject log: operand {i} of {name} is {ty:?} at this version, not {what}"
                )));
            }
            let value = match ty {
                T::Reg8 | T::UInt8 | T::UInt8S => u8::try_from(x).ok().map(V::U8),
                T::UInt16 | T::UInt16S => u16::try_from(x).ok().map(V::U16),
                _ => Some(V::U32(x)),
            }
            .ok_or_else(|| {
                Error::Write(format!(
                    "inject log: {what} {x} does not fit operand {i} of {name} ({ty:?})"
                ))
            })?;
            Ok(Operand { ty, value })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Instruction {
        offset: 0,
        opcode: def.opcode,
        operands,
        length: 0,
    })
}

// The log prologue: `print(<function name>)` in r0..r3. r0 = global object,
// r1 = print, r2 = this (undefined), r3 = the name.
fn log_prologue(
    format: &BytecodeFormat,
    cache: u32,
    print_id: u32,
    name_id: u32,
) -> Result<Vec<Instruction>> {
    use StubVal::{Cache, Reg, Str};
    Ok(vec![
        stub_insn(format, "GetGlobalObject", &[Reg(0)])?,
        stub_insn(
            format,
            "TryGetById",
            &[Reg(1), Reg(0), Cache(cache), Str(print_id)],
        )?,
        stub_insn(format, "LoadConstUndefined", &[Reg(2)])?,
        stub_insn(format, "LoadConstString", &[Reg(3), Str(name_id)])?,
        stub_insn(format, "Call2", &[Reg(0), Reg(1), Reg(2), Reg(3)])?,
    ])
}

// Kinds of bytecode stubs we plan to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectStubKind {
    // Minimal no-op: AsyncBreakCheck if available, else leave unchanged.
    NopPad,
    // Prepend LoadConstUndefined; Ret path unchanged, only works if frame has room
    // and we use same-size replace of first instruction with a longer sequence, hard.
    // Instead: append AsyncBreakCheck before Ret when Ret is last and we grow.
    LogEntry,
}

// Build a real `print(<function name>)` prologue and prepend it to `body`.
//
// Uses registers r0..r3 (free at entry, Hermes reads params via LoadParam, not
// pre-loaded frame registers) and one read-cache slot, which
// `reserve_legacy_log_regs` / `reserve_modern_log_regs` write into the function
// header's bytes so the runtime allocates them. Needs a `"print"` string in the
// table; refuses an overflowed legacy function.
fn build_log_entry(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    body: &mut Vec<Instruction>,
) -> Result<()> {
    let print_id = file
        .strings
        .iter()
        .position(|s| s.value == "print")
        .ok_or_else(|| {
            Error::Write("inject log: no \"print\" string in the table to build a log call".into())
        })? as u32;
    let msg_id = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Legacy(leg)) => {
            if leg.flags & crate::format::FLAG_OVERFLOWED != 0 {
                return Err(Error::Write(
                    "inject log: overflowed (large) legacy function headers not yet supported"
                        .into(),
                ));
            }
            leg.function_name
        }
        Some(FunctionHeader::Modern(m)) => m.function_name,
        None => return Err(Error::Write(format!("invalid function id {function_id}"))),
    };

    // Check the prologue's shape at this version before anything is written: the
    // reservation below edits the header's bytes.
    log_prologue(format, 0, print_id, msg_id)?;
    let frame_now = file.function_headers[function_id as usize].frame_size();
    let new_frame = log_frame(format, body, frame_now)?;
    let cache_idx = match file.function_headers.get(function_id as usize) {
        Some(FunctionHeader::Legacy(_)) => reserve_legacy_log_regs(file, function_id, new_frame)?,
        _ => reserve_modern_log_regs(file, function_id, new_frame)?,
    };
    let mut seq = log_prologue(format, cache_idx, print_id, msg_id)?;

    // Keep the injected size a multiple of 4 so downstream functions shift by a
    // 4-aligned delta and their SwitchImm jump tables stay aligned. Pad with the
    // 1-byte AsyncBreakCheck (a runtime no-op). If padding is actually required but
    // this version has no AsyncBreakCheck, fail loudly rather than silently emit a
    // non-4-aligned prologue that misaligns every downstream large header (I5 / Q8).
    let injected_len = encode_function_body(format, &seq)?.len();
    if injected_len % 4 != 0 {
        let pad = stub_insn(format, "AsyncBreakCheck", &[]).map_err(|_| {
            Error::Write(format!(
                "cannot 4-byte-align injected prologue ({injected_len} bytes) — this \
                 bytecode version has no AsyncBreakCheck instruction to pad with"
            ))
        })?;
        for _ in 0..4 - injected_len % 4 {
            seq.push(pad.clone());
        }
    }

    for (i, ins) in seq.into_iter().enumerate() {
        body.insert(i, ins);
    }
    Ok(())
}

// Inject a stub into a function body, then serialize.
//
// LogEntry injects a real `print(<function name>)` prologue (resize path).
// Prefer functions without overflow.
pub fn inject_stub(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    kind: InjectStubKind,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let mut body = file.decode_function_instructions(format, function_id)?;
    let old_size: i64 = body.iter().map(|i| i.length as i64).sum();
    // Where the new bytes go in. Both stubs insert at exactly one point, which is
    // what makes the line table relocatable afterwards (R24 P2): every location at
    // or past this offset moves by the size delta, and every location before it
    // stays put. A wholesale body replacement has no such point, which is why
    // `patch_function_body` still refuses those.
    let mut insert_at: u32 = 0;
    match kind {
        InjectStubKind::NopPad => {
            // Find AsyncBreakCheck opcode by name if present; else no-op success with identity.
            if let Some(op) = format
                .definitions
                .iter()
                .find(|d| d.name == "AsyncBreakCheck")
                .map(|d| d.opcode)
            {
                // Insert before final Ret if present
                let nop_at = body
                    .iter()
                    .rposition(|i| {
                        format
                            .definitions
                            .get(i.opcode as usize)
                            .map(|d| d.name == "Ret")
                            .unwrap_or(false)
                    })
                    .unwrap_or(body.len());
                insert_at = body.iter().take(nop_at).map(|i| i.length).sum();
                // Four of them: the body stays 4-byte aligned without the resize
                // padding elsewhere, so `insert_at` is the one point everything
                // moved at -- the contract the line table and the handler table
                // are both relocated by.
                for _ in 0..4 {
                    body.insert(
                        nop_at,
                        Instruction {
                            offset: 0,
                            opcode: op,
                            operands: vec![],
                            length: 1,
                        },
                    );
                }
            }
        }
        InjectStubKind::LogEntry => {
            // The prologue goes in at the front, so everything in the function
            // moves by its length.
            build_log_entry(file, format, function_id, &mut body)?;
        }
    }

    // The body edit itself. `allow_stale_debug_info` here is not "the line table
    // does not matter" -- it is "this caller is about to fix it", two lines down.
    // Going through the guarded path and then relocating would just refuse.
    let body_options = PatchOptions {
        allow_stale_debug_info: true,
        ..options.clone()
    };
    // Recompute offsets in the instruction list for encode (encode ignores insn.offset).
    // Handlers at or past the insertion point move with the code (Q3 phase 1).
    let out = patch_function_body_with(
        file,
        format,
        function_id,
        &body,
        &body_options,
        HandlerUpdate::Insert { at: insert_at },
    )?;

    // `file` was refreshed by the commit inside `patch_function_body`, so its
    // headers and debug_info_offset describe the image we are holding.
    let new_size = file
        .function_headers
        .get(function_id as usize)
        .map(|h| h.bytecode_size_in_bytes() as i64)
        .unwrap_or(old_size);
    let delta = new_size - old_size;

    match debug_reloc::relocate_locations_for_insertion(
        file,
        out.clone(),
        function_id,
        insert_at,
        delta,
    ) {
        // The resize above was committed already; relocating the line table
        // touches the debug region's contents, never the header.
        Ok(relocated) => commit_image(file, relocated, HeaderEdit::default()),
        // The only failure is a version whose debug layout is not modelled. If the
        // caller explicitly accepted a stale line table, honour that; otherwise the
        // refusal is the point.
        Err(e) if options.allow_stale_debug_info => {
            let _ = e;
            Ok(out)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    // R12: the prologue is built from each version's opcode table. Its shape is
    // the same from v41 to v99; v40 has no Call2, and says so rather than
    // encoding something else.
    #[test]
    fn the_log_prologue_follows_the_opcode_table_at_every_version() {
        for version in 40..=99 {
            let format = BytecodeFormat::for_version(version).unwrap();
            match log_prologue(&format, 1, 2, 3) {
                Ok(seq) => {
                    assert_ne!(version, 40);
                    encode_function_body(&format, &seq)
                        .unwrap_or_else(|e| panic!("v{version}: built but does not encode: {e}"));
                }
                Err(e) => {
                    assert_eq!(version, 40, "v{version}: {e}");
                    assert!(e.to_string().contains("no Call2"), "{e}");
                }
            }
        }
    }

    #[test]
    fn a_value_too_wide_for_its_operand_is_refused() {
        let format = BytecodeFormat::for_version(96).unwrap();
        // TryGetById's property-name operand is UInt16S at v96.
        let err = log_prologue(&format, 1, 70_000, 3).unwrap_err().to_string();
        assert!(err.contains("string id 70000 does not fit"), "{err}");
        let err = log_prologue(&format, 256, 2, 3).unwrap_err().to_string();
        assert!(err.contains("cache index 256 does not fit"), "{err}");
    }

    #[test]
    fn an_operand_of_another_kind_or_count_is_refused() {
        use StubVal::{Cache, Reg};
        let format = BytecodeFormat::for_version(96).unwrap();
        let err = stub_insn(&format, "Call2", &[Cache(0), Reg(1), Reg(2), Reg(3)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("operand 0 of Call2 is Reg8"), "{err}");
        let err = stub_insn(&format, "Call2", &[Reg(0)])
            .unwrap_err()
            .to_string();
        assert!(err.contains("Call2 takes 4 operands"), "{err}");
    }

    #[test]
    fn inject_stub_grows_and_reparses() {
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
        let out = inject_stub(
            &mut file,
            &format,
            0,
            InjectStubKind::NopPad,
            &PatchOptions::default(),
        )
        .unwrap();
        assert!(verify_footer(&out));
        BytecodeFile::parse_auto(&out).expect("reparse after inject");
    }

    // Modern (v97+) headers keep the body region contiguous with a 4 byte aligned
    // FunctionInfo region after it, so a resize has to shift the tail by a 4
    // aligned amount and relocate every overflowed large header. Exercise a nop
    // and a log stub on a real v98 file and confirm both still parse.
    #[test]
    fn inject_stub_modern_v98_grows_and_reparses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v98/expressions/class_basic/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let file0 = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file0.header.version).unwrap();
        assert!(matches!(
            file0.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));

        for kind in [InjectStubKind::NopPad, InjectStubKind::LogEntry] {
            let mut file = file0.clone();
            let out = inject_stub(&mut file, &format, 0, kind, &PatchOptions::default()).unwrap();
            assert!(verify_footer(&out), "footer invalid for {kind:?}");
            let reparsed = BytecodeFile::parse_auto(&out)
                .unwrap_or_else(|e| panic!("reparse after modern inject {kind:?}: {e}"));
            // The body region and the FunctionInfo region that follows must both
            // still be reachable, so every function large header parses.
            assert_eq!(
                reparsed.function_headers.len(),
                file0.function_headers.len(),
                "function count changed for {kind:?}"
            );
        }
    }

    // The two tests above are fixture-gated and skip in CI. The tests below build
    // a real legacy image with `create_minimal`, exercising the legacy LogEntry
    // branch and its precondition errors that plan_guides/06_write/RISKS.md flags as untested.
    use crate::write::create::{create_minimal, CreateOptions};

    fn make_legacy(strings: Vec<String>) -> (BytecodeFile, BytecodeFormat) {
        let bytes = create_minimal(&CreateOptions {
            version: 96,
            strings,
            ..Default::default()
        })
        .expect("create_minimal v96");
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created file");
        let format = BytecodeFormat::for_version_or_latest(96).expect("format").0;
        (file, format)
    }

    // Legacy LogEntry: a non-overflowed v96 function with a "print" string in the
    // table takes the legacy frame/cache branch of build_log_entry, grows, and
    // reparses.
    #[test]
    fn legacy_log_entry_grows_and_reparses() {
        let (mut file, format) = make_legacy(vec!["global".into(), "print".into()]);
        // Skip if this version lacks any opcode the log prologue needs.
        let has = |n: &str| format.definitions.iter().any(|d| d.name == n);
        if ![
            "GetGlobalObject",
            "TryGetById",
            "LoadConstUndefined",
            "LoadConstString",
            "Call2",
        ]
        .iter()
        .all(|n| has(n))
        {
            return;
        }
        assert!(matches!(
            file.function_headers[0],
            FunctionHeader::Legacy(_)
        ));
        let before = file.function_headers[0].bytecode_size_in_bytes();
        let out = inject_stub(
            &mut file,
            &format,
            0,
            InjectStubKind::LogEntry,
            &PatchOptions::default(),
        )
        .expect("legacy log entry inject");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).expect("reparse after legacy log inject");
        assert!(
            re.function_headers[0].bytecode_size_in_bytes() > before,
            "the injected prologue must grow the body"
        );
    }

    // LogEntry with no "print" string in the table must be rejected before any
    // bytes are written.
    #[test]
    fn log_entry_without_print_string_errors() {
        let (mut file, format) = make_legacy(vec!["global".into()]);
        let err = inject_stub(
            &mut file,
            &format,
            0,
            InjectStubKind::LogEntry,
            &PatchOptions::default(),
        )
        .expect_err("must fail without a print string");
        assert!(
            err.to_string().contains("print"),
            "error should mention the missing print string, got: {err}"
        );
    }

    // An overflowed legacy function is refused by build_log_entry. We set the
    // overflow flag on the in-memory header to hit the guard deterministically
    // without needing a real overflowed fixture.
    #[test]
    fn overflowed_legacy_log_entry_refused() {
        let (mut file, format) = make_legacy(vec!["global".into(), "print".into()]);
        match &mut file.function_headers[0] {
            FunctionHeader::Legacy(leg) => leg.flags |= crate::format::FLAG_OVERFLOWED,
            _ => panic!("expected a legacy function header"),
        }
        let err = inject_stub(
            &mut file,
            &format,
            0,
            InjectStubKind::LogEntry,
            &PatchOptions::default(),
        )
        .expect_err("overflowed legacy must be refused");
        assert!(
            err.to_string().contains("overflow"),
            "error should mention overflow, got: {err}"
        );
    }
}
