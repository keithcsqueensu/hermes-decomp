//! The R24 guard: a size-changing edit must not silently invalidate debug info.
//!
//! A location stream stores bytecode addresses *within* a function as SLEB128
//! deltas. Resizing a body rewrites none of them, so every location past the edit
//! point maps to the wrong instruction — and nothing fails, because the stream
//! still decodes and still terminates. That is R24, and it is the exception-handler
//! defect (R9) in a second structure; the difference was only that one had a guard.
//!
//! These tests are the acceptance criteria from `docs/UNMODELED_REGIONS_PLAN.md` P0.
//!
//! ## Why a separate fixture
//!
//! Not because the others lack debug info — measured, **every** committed fixture
//! carries `FLAG_HAS_DEBUG_INFO` on **every** function, because `hermesc` emits
//! per-function debug info without being asked. `locations.debug.js` is the only one
//! built with `-g3`, which adds the *full* apparatus (scope descriptors, a populated
//! debug string table, several statements per function to be wrong about), so it is
//! the fixture P1's reader will need and the honest one to guard against now.
//! `fixture_actually_carries_debug_info` checks that premise first; the rest of this
//! file asserts nothing if it ever fails.
//!
//! ## Why refusing by default is affordable
//!
//! Measured on the 11.39.0 Equinox bundle: **0 of 62,909 functions** carry
//! `FLAG_HAS_DEBUG_INFO`. React Native ships bundles with per-function debug info
//! stripped, so the default-refuse costs the workflow this crate exists for
//! nothing. It fires on debug-built bundles, where the edit really would corrupt
//! something.

use std::path::PathBuf;

use hbc_decomp::write::patch::PatchOptions;
use hbc_decomp::{
    create_minimal, inject_stub, BytecodeFile, BytecodeFormat, CreateOptions, InjectStubKind,
};

const VERSIONS: [u32; 3] = [96, 98, 99];

fn fixture(name: &str, version: u32) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("{name}.v{version}.hbc"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn format_for(file: &BytecodeFile) -> BytecodeFormat {
    BytecodeFormat::for_version(file.header.version).expect("bundled opcode table")
}

/// The premise. Every other test here is vacuous if this one is not true.
#[test]
fn fixture_actually_carries_debug_info() {
    for version in VERSIONS {
        let bytes = fixture("locations.debug", version);
        let file = BytecodeFile::parse_auto(&bytes).expect("parse");
        assert_ne!(
            file.header.debug_info_offset, 0,
            "v{version}: locations.debug has no debug info section — was it built without -g3?"
        );
        let flagged = file
            .function_headers
            .iter()
            .filter(|h| h.has_debug_info())
            .count();
        assert_eq!(
            flagged,
            file.function_headers.len(),
            "v{version}: -g3 should flag every function; {flagged} of {} carry \
             FLAG_HAS_DEBUG_INFO",
            file.function_headers.len()
        );
    }
}

/// A *wholesale* body replacement is what the guard covers. An insertion is
/// relocated instead (P2, `tests/debug_relocation.rs`) — the difference is whether
/// old addresses map to new ones at all.
#[test]
fn size_changing_edit_on_a_debug_bearing_function_is_refused() {
    for version in VERSIONS {
        let bytes = fixture("locations.debug", version);
        let mut file = BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = format_for(&file);
        let mut body = file
            .decode_function_instructions(&format, 1)
            .expect("decode function 1");
        body.truncate(body.len().saturating_sub(2));
        let err = hbc_decomp::write::patch::patch_function_body(
            &mut file,
            &format,
            1,
            &body,
            &PatchOptions::default(),
        )
        .expect_err("a wholesale resize of a debug-bearing function must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("debug info"),
            "v{version}: the refusal must name debug info, got: {msg}"
        );
        assert!(
            msg.contains("--allow-stale-debug-info"),
            "v{version}: the refusal must name the opt-out, got: {msg}"
        );
    }
}

#[test]
fn the_opt_out_permits_the_same_edit() {
    for version in VERSIONS {
        let bytes = fixture("locations.debug", version);
        let mut file = BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = format_for(&file);
        let mut body = file
            .decode_function_instructions(&format, 1)
            .expect("decode function 1");
        body.truncate(body.len().saturating_sub(2));
        let opts = PatchOptions {
            allow_stale_debug_info: true,
            ..Default::default()
        };
        let out = hbc_decomp::write::patch::patch_function_body(&mut file, &format, 1, &body, &opts)
            .unwrap_or_else(|e| panic!("v{version}: opt-out should permit the edit: {e}"));
        BytecodeFile::parse_auto(&out).expect("the patched image still reparses");
    }
}

/// Same-size edits are safe by construction — no address inside the function moves —
/// so the guard must not touch them. This is the same carve-out the handler guard
/// makes, and for the same reason.
#[test]
fn a_same_size_edit_is_unaffected() {
    for version in VERSIONS {
        let bytes = fixture("locations.debug", version);
        let mut file = BytecodeFile::parse_auto(&bytes).expect("parse");
        let format = format_for(&file);
        let body = file
            .decode_function_instructions(&format, 1)
            .expect("decode function 1");
        let out = hbc_decomp::write::patch::patch_function_body(
            &mut file,
            &format,
            1,
            &body,
            &PatchOptions::default(),
        )
        .unwrap_or_else(|e| panic!("v{version}: identical body is a same-size edit: {e}"));
        BytecodeFile::parse_auto(&out).expect("reparses");
    }
}

/// A file with no debug section at all must be editable, and this is the case that
/// says why the guard keys on `debug_info_offset` as well as on the flag.
///
/// It cannot be tested with a fixture: **every** committed fixture has debug info on
/// **every** function, `plain` included — `hermesc` emits it without being asked, and
/// only `-g0`-style stripping (which is what ships in a React Native bundle) clears
/// it. So the plan's premise that "every committed fixture is built without debug
/// info" was exactly backwards.
///
/// A `create`d image is the real no-debug-section case: `debug_info_offset == 0` and
/// no debug bytes anywhere. Its legacy global function nonetheless has
/// `FLAG_HAS_DEBUG_INFO` set (flags `0x12`), so on the flag alone this edit would be
/// refused over debug info the file does not contain.
#[test]
fn a_file_with_no_debug_section_is_unaffected() {
    for version in VERSIONS {
        let created = create_minimal(&CreateOptions {
            version,
            ..Default::default()
        })
        .unwrap_or_else(|e| panic!("v{version}: create: {e}"));
        let mut file = BytecodeFile::parse_auto(&created).expect("parse created image");
        assert_eq!(
            file.header.debug_info_offset, 0,
            "v{version}: a created image should carry no debug section"
        );
        let format = format_for(&file);
        inject_stub(
            &mut file,
            &format,
            0,
            InjectStubKind::NopPad,
            &PatchOptions::default(),
        )
        .unwrap_or_else(|e| {
            panic!("v{version}: no debug section, so the edit must not be refused: {e}")
        });
    }
}
