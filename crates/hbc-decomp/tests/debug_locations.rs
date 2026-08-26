//! P1: the location streams are readable, and the names they lead to are real.
//!
//! `DebugInfo::source_locations` was declared, consumed by two call sites, and
//! never populated — `parse` simply never assigned it, so debug-driven variable
//! naming had never produced a name in the life of this crate (DI1). The streams
//! were always in the file; what was missing was the index into them, each
//! function's `DebugOffsets.sourceLocations`.
//!
//! These tests assert against `locations.debug.js`, whose line numbers are the
//! ground truth. Its shape matters:
//!
//! ```text
//!  8  function classify(n) {      10  if (n > 0) {       12  } else if (n < 0) {
//! 15    return label;             18  function total(a, b) {   19..21 its body
//! 24  print(classify(1), ...)     30  function makeCounter(startValue) {
//! 31    var count = startValue;   32    function bump(amount) {
//! ```
//!
//! so a decoder that is off by an entry, or that mixes up the two "previous"
//! cursors at v98+, produces line numbers that do not match the file — which is
//! the only check that distinguishes "decoded" from "decoded correctly".

use std::collections::BTreeSet;
use std::path::PathBuf;

use hbc_decomp::BytecodeFile;

const VERSIONS: [u32; 3] = [96, 98, 99];

fn fixture(version: u32) -> BytecodeFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(format!("locations.debug.v{version}.hbc"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    BytecodeFile::parse_auto(&bytes).expect("parse fixture")
}

fn lines_of(file: &BytecodeFile, function_id: u32) -> BTreeSet<u32> {
    file.debug_info
        .as_ref()
        .and_then(|d| d.source_locations.get(&function_id))
        .map(|locs| locs.iter().map(|l| l.line).collect())
        .unwrap_or_default()
}

#[test]
fn streams_are_decoded_at_every_supported_version() {
    for version in VERSIONS {
        let file = fixture(version);
        let debug = file.debug_info.as_ref().expect("debug info");
        assert!(
            !debug.source_locations.is_empty(),
            "v{version}: no location streams decoded — DI1 is back"
        );
        for (id, locs) in &debug.source_locations {
            assert!(
                !locs.is_empty(),
                "v{version}: fn#{id} has an entry but no locations"
            );
            assert!(
                locs.windows(2).all(|w| w[0].bytecode_offset <= w[1].bytecode_offset),
                "v{version}: fn#{id} addresses must be non-decreasing: {:?}",
                locs.iter().map(|l| l.bytecode_offset).collect::<Vec<_>>()
            );
        }
    }
}

/// The assertion that distinguishes "it decoded" from "it decoded correctly":
/// the lines have to be the lines in the `.js`.
#[test]
fn decoded_lines_match_the_source() {
    for version in VERSIONS {
        let file = fixture(version);
        // fn#1 is `classify`, whose statements sit on lines 8, 10, 12 and 15.
        let classify = lines_of(&file, 1);
        for expected in [8u32, 10, 12, 15] {
            assert!(
                classify.contains(&expected),
                "v{version}: classify should carry line {expected}; got {classify:?}"
            );
        }
        assert!(
            classify.iter().all(|&l| (8..=16).contains(&l)),
            "v{version}: every classify line must fall inside the function (8..=16); got {classify:?}"
        );

        // fn#2 is `total`: declaration on 18, body on 19..=21.
        let total = lines_of(&file, 2);
        for expected in [18u32, 19, 20, 21] {
            assert!(
                total.contains(&expected),
                "v{version}: total should carry line {expected}; got {total:?}"
            );
        }
    }
}

/// Two encodings, one source. v96 uses the legacy stream (3-int prologue, 1-bit
/// line delta, an absolute scopeAddress and envReg per entry) and v98/v99 the
/// modern one (4-int prologue, 3-bit line delta, location-less entries). If both
/// decoders are right they must agree about which lines a function touches; if
/// either is subtly wrong — a missed prologue field, a collapsed address cursor —
/// they will not.
#[test]
fn the_two_encodings_agree_about_the_same_program() {
    let v96 = fixture(96);
    for version in [98u32, 99] {
        let modern = fixture(version);
        for id in [1u32, 2] {
            let a = lines_of(&v96, id);
            let b = lines_of(&modern, id);
            assert_eq!(
                a, b,
                "fn#{id}: v96 and v{version} disagree about the lines of the same source"
            );
        }
    }
}

/// DI1's actual promise: a real name for a real variable, recovered from debug
/// info rather than synthesised.
///
/// Only *captured* variables are named — Hermes records a scope entry for a
/// `Variable`, and plain locals live in registers — which is why the fixture has a
/// closure. `count` is captured by `bump`.
#[test]
fn v96_recovers_a_real_variable_name() {
    let file = fixture(96);
    let debug = file.debug_info.as_ref().expect("debug info");
    assert!(
        !debug.function_scopes.is_empty(),
        "no function → scope links read from DebugOffsets.scopeDescData"
    );

    let names: Vec<String> = (0..file.function_headers.len() as u32)
        .flat_map(|id| {
            debug
                .variable_map_for_function(id)
                .into_values()
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "count"),
        "expected the captured variable `count` to be recovered; got {names:?}"
    );
}

/// Names inside a scope descriptor are byte *offsets* into the debug string table,
/// not indices into a list of decoded strings.
///
/// Upstream's `appendString` writes `stringTable_.size()` at the moment the string
/// was first appended, and `decodeString` seeks to that byte and reads a LEB128
/// length. Reading it as an index — which this crate did until P1 — resolves only
/// the string that happens to live at offset 0 and yields empty for the rest. The
/// failure looks like Hermes having named one variable out of three, which is why
/// it needs a test with three.
#[test]
fn every_captured_name_resolves_not_just_the_first() {
    let file = fixture(96);
    let debug = file.debug_info.as_ref().expect("debug info");

    let scope = debug
        .scope_descriptors
        .iter()
        .find(|s| s.names.len() >= 3)
        .unwrap_or_else(|| {
            panic!(
                "expected a scope with three captured variables; got {:?}",
                debug
                    .scope_descriptors
                    .iter()
                    .map(|s| &s.names)
                    .collect::<Vec<_>>()
            )
        });
    for want in ["first", "second", "third"] {
        assert!(
            scope.names.iter().any(|n| n == want),
            "`{want}` should resolve; got {:?} — an index-based read gives the first \
             name and empty strings after it",
            scope.names
        );
    }
    assert!(
        !scope.names.iter().any(|n| n.is_empty()),
        "no name should decode to empty: {:?}",
        scope.names
    );
}

/// The two candidate scope links are not the same link, and the plan picked the
/// wrong one.
///
/// `DebugOffsets.scopeDescData` names the function's own scope. The stream's
/// `scopeAddress` names the innermost scope live at one instruction, and upstream
/// defaults it to the shared empty descriptor at offset 0 — so scanning a stream
/// for it, which is what P1 was originally specified to do, resolves most functions
/// to a scope that names nothing while occasionally being right. This test pins the
/// disagreement: if it stops holding, the choice made in `DebugInfo::function_scopes`
/// deserves revisiting rather than assuming.
#[test]
fn the_scope_link_does_not_come_from_the_stream() {
    let file = fixture(96);
    let debug = file.debug_info.as_ref().expect("debug info");

    let mut disagreements = 0;
    for (&id, &own_scope) in &debug.function_scopes {
        let via_stream = debug
            .source_locations
            .get(&id)
            .and_then(|locs| locs.iter().find_map(|l| l.scope_offset));
        if via_stream != Some(own_scope) {
            disagreements += 1;
            // The direction matters too: the stream is the one that points at a
            // scope naming nothing.
            if let Some(streamed) = via_stream {
                let from_stream = debug.build_variable_map(Some(streamed));
                let from_offsets = debug.build_variable_map(Some(own_scope));
                assert!(
                    from_stream.len() <= from_offsets.len(),
                    "fn#{id}: the stream link should not out-name the DebugOffsets link"
                );
            }
        }
    }
    assert!(
        disagreements > 0,
        "the two links agreed everywhere, so this fixture no longer demonstrates why \
         function_scopes exists; revisit the note on DebugInfo::function_scopes"
    );

    let via_offsets: BTreeSet<u32> = debug.function_scopes.values().copied().collect();
    assert!(
        via_offsets.iter().any(|&s| s != 0),
        "DebugOffsets must point at real scopes, not just the empty one; got {via_offsets:?}"
    );
}

/// v98 and v99 have no scope table at all: upstream removed the whole apparatus
/// after v96. The reader must produce nothing rather than inventing a link.
#[test]
fn modern_versions_have_no_scope_table() {
    for version in [98u32, 99] {
        let file = fixture(version);
        let debug = file.debug_info.as_ref().expect("debug info");
        assert!(
            debug.function_scopes.is_empty() && debug.scope_descriptors.is_empty(),
            "v{version}: modern files carry no scope descriptors"
        );
        assert!(
            debug
                .source_locations
                .values()
                .all(|locs| locs.iter().all(|l| l.scope_offset.is_none())),
            "v{version}: modern streams have no per-location scope link to report"
        );
    }
}

/// R25's regression test. The header is 28 bytes at v96 and 16 at v98+; reading a
/// modern file with the v96 shape does not fail, it silently yields an empty
/// `DebugInfo` — which is exactly what this crate did before the reader was keyed.
/// The v98/v99 fixtures decoding to real streams *is* the assertion.
#[test]
fn the_modern_header_size_is_keyed_by_version() {
    for version in [98u32, 99] {
        let file = fixture(version);
        let debug = file.debug_info.as_ref().expect("debug info");
        assert!(
            !debug.source_locations.is_empty(),
            "v{version}: empty debug info is what reading a 16-byte header as 28 looks like"
        );
    }
}
