//! P2: an insertion into a function body keeps its line table correct.
//!
//! R24 is that a resize shifts nothing inside the debug info, so every location
//! past the edit point silently describes the wrong instruction. P0 refused such
//! edits. This is the other half: for an *insertion*, where old address A maps to
//! A or A+delta, the addresses can be corrected instead of refused.
//!
//! The assertion that matters is not "the numbers changed" but **"each location
//! still names the instruction it named before"**. A test that only checked lines
//! were unchanged would pass even if nothing had been relocated at all, because
//! relocation moves addresses and leaves lines alone — so every test here compares
//! line-for-a-given-instruction, before against after.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hbc_decomp::write::patch::PatchOptions;
use hbc_decomp::{inject_stub, BytecodeFile, BytecodeFormat, InjectStubKind};

const VERSIONS: [u32; 3] = [96, 98, 99];

fn fixture(version: u32) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("locations.debug.v{version}.hbc"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn parse(bytes: &[u8]) -> BytecodeFile {
    BytecodeFile::parse_auto(bytes).expect("parse")
}

fn format_for(file: &BytecodeFile) -> BytecodeFormat {
    BytecodeFormat::for_version(file.header.version).expect("opcode table")
}

/// address → line, for one function.
fn locations(file: &BytecodeFile, function_id: u32) -> BTreeMap<u32, u32> {
    file.debug_info
        .as_ref()
        .and_then(|d| d.source_locations.get(&function_id))
        .map(|locs| locs.iter().map(|l| (l.bytecode_offset, l.line)).collect())
        .unwrap_or_default()
}

/// The offset of the last `Ret` in a function body — where `inject-stub NopPad`
/// puts its padding.
fn last_ret_offset(file: &BytecodeFile, format: &BytecodeFormat, function_id: u32) -> Option<u32> {
    let mut at = 0u32;
    let mut last = None;
    for insn in file.decode_function_instructions(format, function_id).ok()? {
        if format
            .definitions
            .get(insn.opcode as usize)
            .is_some_and(|d| d.name == "Ret")
        {
            last = Some(at);
        }
        at += insn.length;
    }
    last
}

/// A function with a location stream and no exception handlers, so `inject-stub`
/// is not refused for the *other* reason.
fn subject(file: &BytecodeFile) -> u32 {
    let debug = file.debug_info.as_ref().expect("debug info");
    (0..file.function_headers.len() as u32)
        .find(|id| {
            debug
                .source_locations
                .get(id)
                .is_some_and(|l| l.len() >= 3)
                && !file.function_headers[*id as usize].has_exception_handler()
        })
        .expect("a function with several locations and no handlers")
}

#[test]
fn an_insertion_no_longer_needs_the_opt_out() {
    for version in VERSIONS {
        let bytes = fixture(version);
        let mut file = parse(&bytes);
        let format = format_for(&file);
        let id = subject(&file);
        // The default options: no opt-out. Before P2 this was refused by P0's guard.
        inject_stub(
            &mut file,
            &format,
            id,
            InjectStubKind::NopPad,
            &PatchOptions::default(),
        )
        .unwrap_or_else(|e| panic!("v{version}: an insertion should relocate, not refuse: {e}"));
    }
}

/// The acceptance criterion from the plan, stated so it cannot pass by accident:
/// every location that existed before the edit must exist after it, at the address
/// the insertion moved it to, carrying the same line.
///
/// The mapping is the whole claim — an old address `A` becomes `A` when it precedes
/// the insertion point and `A + delta` when it does not — so this fails if
/// relocation does nothing, which is what an earlier version of this test did not.
#[test]
fn locations_still_name_the_same_instructions() {
    for version in VERSIONS {
        let bytes = fixture(version);
        let before_file = parse(&bytes);
        let format = format_for(&before_file);
        let id = subject(&before_file);

        let before_locs = locations(&before_file, id);
        let old_size = before_file.function_headers[id as usize].bytecode_size_in_bytes();
        // `NopPad` inserts before the final Ret, so that is the insertion point.
        let insert_at = last_ret_offset(&before_file, &format, id).expect("a final Ret");

        let mut file = parse(&bytes);
        let out = inject_stub(
            &mut file,
            &format,
            id,
            InjectStubKind::NopPad,
            &PatchOptions::default(),
        )
        .unwrap_or_else(|e| panic!("v{version}: {e}"));

        let after_file = parse(&out);
        let new_size = after_file.function_headers[id as usize].bytecode_size_in_bytes();
        let delta = new_size as i64 - old_size as i64;
        assert!(
            delta > 0,
            "v{version}: the fixture edit must actually grow the body"
        );
        let after_locs = locations(&after_file, id);

        let mut moved = 0;
        for (&addr, &line) in &before_locs {
            let want_at = if addr >= insert_at {
                moved += 1;
                (addr as i64 + delta) as u32
            } else {
                addr
            };
            let got = after_locs.get(&want_at).copied().unwrap_or_else(|| {
                panic!(
                    "v{version}: the location at {addr} (line {line}) should have moved to                      {want_at} after inserting {delta} bytes at {insert_at}; the stream now                      reads {after_locs:?}"
                )
            });
            assert_eq!(
                got, line,
                "v{version}: address {want_at} carries line {got}, expected {line}"
            );
        }
        assert!(
            moved > 0,
            "v{version}: no location sat at or after the insertion point, so this test              cannot tell relocation from doing nothing"
        );
    }
}

/// Relocation must be surgical: no other function's line table may move.
#[test]
fn other_functions_are_untouched() {
    for version in VERSIONS {
        let bytes = fixture(version);
        let before_file = parse(&bytes);
        let format = format_for(&before_file);
        let id = subject(&before_file);

        let others: Vec<u32> = (0..before_file.function_headers.len() as u32)
            .filter(|f| *f != id)
            .collect();
        let before: BTreeMap<u32, BTreeMap<u32, u32>> = others
            .iter()
            .map(|&f| (f, locations(&before_file, f)))
            .collect();

        let mut file = parse(&bytes);
        let out = inject_stub(
            &mut file,
            &format,
            id,
            InjectStubKind::NopPad,
            &PatchOptions::default(),
        )
        .unwrap_or_else(|e| panic!("v{version}: {e}"));
        let after_file = parse(&out);

        for &f in &others {
            assert_eq!(
                locations(&after_file, f),
                before[&f],
                "v{version}: fn#{f} is not the function that was edited, so its \
                 locations must be byte-identical"
            );
        }
    }
}

/// A wholesale body replacement is still refused, and that is deliberate: there is
/// no mapping from an old address to a new one when the body is different code, so
/// "relocating" it would mean inventing a correspondence. P2 covers insertions.
#[test]
fn a_wholesale_replacement_is_still_refused() {
    for version in VERSIONS {
        let bytes = fixture(version);
        let mut file = parse(&bytes);
        let format = format_for(&file);
        let id = subject(&file);

        let mut body = file
            .decode_function_instructions(&format, id)
            .expect("decode");
        // Any size-changing rewrite that is not an insertion at a known point.
        body.truncate(body.len().saturating_sub(2));
        let err = hbc_decomp::write::patch::patch_function_body(
            &mut file,
            &format,
            id,
            &body,
            &PatchOptions::default(),
        )
        .expect_err("a wholesale resize of a debug-bearing function must still be refused");
        assert!(
            err.to_string().contains("debug info"),
            "v{version}: the refusal should still name debug info: {err}"
        );
    }
}
