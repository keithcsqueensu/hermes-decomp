//! The read path degrades rather than failing — and says so.
//!
//! Every case here used to resolve to a value indistinguishable from success:
//! a file decoded under the layout its version contradicts, a stale SHA-1 footer,
//! a truncated image, a debug section this build cannot read. See
//! `docs/READ_PATH_GUIDE.md` F1, F5, F6, F10, F11.

mod common;
use common::Oracle;

use hbc_decomp::{BytecodeFile, Diagnostic, FunctionHeaderLayout, HeaderLayout};

fn fx(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

const CLEAN: [&str; 4] = [
    "plain.v96.hbc",
    "plain.v98.hbc",
    "plain.v99.hbc",
    "handlers.v96.hbc",
];

// Deterministic bit-flipper, so a failure is reproducible.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn an_intact_fixture_reports_nothing() {
    for name in CLEAN {
        let f = BytecodeFile::parse_auto(&fx(name)).unwrap();
        assert!(
            f.is_clean(),
            "{name} should read clean, got {:?}",
            f.warnings()
        );
    }
}

// F1. The declared version decides the layout; the other one is a *reported*
// fallback, never a silent substitution.
#[test]
fn the_version_decides_the_layout() {
    for name in ["plain.v98.hbc", "plain.v99.hbc"] {
        let f = BytecodeFile::parse_auto(&fx(name)).unwrap();
        assert_eq!(f.header.layout, HeaderLayout::Modern, "{name}");
        assert_eq!(
            f.header.function_header_layout,
            FunctionHeaderLayout::Modern12,
            "{name}"
        );
    }
    // The legacy layout *also* parses a clean v99 file. That is precisely why the
    // version cannot be a tie-break: "it parsed" is not evidence on a modern file.
    assert!(BytecodeFile::parse_with_layout(
        &fx("plain.v99.hbc"),
        HeaderLayout::Legacy,
        FunctionHeaderLayout::Legacy16
    )
    .is_ok());
}

// F1, the case that was actually broken: corruption that makes only the *wrong*
// layout parse. Previously returned silently; must now carry a LayoutFallback.
#[test]
fn a_layout_contradicting_the_version_is_always_reported() {
    for name in ["plain.v98.hbc", "plain.v99.hbc"] {
        let base = fx(name);
        let mut rng = Rng(0xabcd_1234_5678_9f01);
        let mut reported = 0usize;
        for _ in 0..4000 {
            let mut b = base.clone();
            // Never touch magic or version: the declared version must stay fixed,
            // or "the wrong layout" is not a meaningful claim.
            let pos = 12 + (rng.next() as usize) % (b.len() - 12);
            b[pos] ^= 1u8 << (rng.next() % 8);

            let Ok(f) = BytecodeFile::parse_auto(&b) else {
                continue;
            };
            let implied_modern = f.header.version >= 97;
            let got_modern = f.header.layout == HeaderLayout::Modern;
            if implied_modern != got_modern {
                assert!(
                    f.diagnostics
                        .iter()
                        .any(|d| matches!(d, Diagnostic::LayoutFallback { .. })),
                    "{name}: flipped byte {pos} parsed as {:?} against a v{} header \
                     with no LayoutFallback diagnostic",
                    f.header.layout,
                    f.header.version
                );
                reported += 1;
            }
        }
        // The measured rates were 95/4000 (v98) and 76/4000 (v99); assert only
        // that the path is exercised, so the test does not pin a fuzz artefact.
        assert!(
            reported > 0,
            "{name}: the wrong-layout path was never hit, so this test proved nothing"
        );
    }
}

// F1. A file that parses under neither layout must say what each one complained
// about, not return one fixed string with both diagnoses thrown away.
#[test]
fn an_unparseable_file_names_both_layouts() {
    let mut b = fx("plain.v96.hbc");
    b[32..128].fill(0xff); // good magic and version, wrecked header body
    let e = BytecodeFile::parse_auto(&b).unwrap_err().to_string();
    assert!(e.contains("Legacy"), "{e}");
    assert!(e.contains("Modern"), "{e}");
    assert!(e.contains("version 96"), "{e}");
}

// F6. A hand-patched image whose footer was never refreshed is the single most
// likely way a bundle reaches this tool broken, and it used to read as clean.
#[test]
fn a_stale_footer_is_reported() {
    let mut b = fx("plain.v96.hbc");
    let mid = b.len() / 2;
    b[mid] ^= 0x01; // change the body, leave the trailing SHA-1 alone

    let f = BytecodeFile::parse_auto(&b).unwrap();
    assert!(
        f.diagnostics.contains(&Diagnostic::FooterMismatch),
        "got {:?}",
        f.diagnostics
    );
    assert!(f.warnings().iter().any(|w| w.contains("SHA-1")));
}

// F6. `header.file_length` was parsed and never compared to anything.
#[test]
fn a_length_mismatch_is_reported() {
    let full = fx("plain.v96.hbc");
    let b = &full[..full.len() - 8];
    let f = BytecodeFile::parse_auto(b).unwrap();
    let found = f.diagnostics.iter().find_map(|d| match d {
        Diagnostic::LengthMismatch { declared, actual } => Some((*declared, *actual)),
        _ => None,
    });
    let (declared, actual) = found.expect("truncation must be reported");
    assert_eq!(declared as usize, full.len());
    assert_eq!(actual, full.len() - 8);
}

// F10. Five different situations used to collapse onto "no debug info".
#[test]
fn debug_info_absence_carries_a_reason() {
    use hbc_decomp::DebugInfoStatus;

    // The committed fixtures all carry debug info.
    for name in CLEAN {
        let f = BytecodeFile::parse_auto(&fx(name)).unwrap();
        assert_eq!(f.debug_info_status, DebugInfoStatus::Present, "{name}");
        assert!(!f.debug_info_status.is_failure());
    }

    // A debug offset past EOF is a corruption, not an absence, and is named as one.
    let mut b = fx("plain.v96.hbc");
    b[104..108].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
    let f = BytecodeFile::parse_auto(&b).unwrap();
    assert_eq!(f.debug_info_status, DebugInfoStatus::OffsetOutOfRange);
    assert!(f.debug_info_status.is_failure());
    assert!(f
        .diagnostics
        .iter()
        .any(|d| matches!(d, Diagnostic::DebugInfoUnreadable(_))));

    // Genuine absence is distinguishable from all of the above, and is not a
    // failure worth warning about.
    let mut b = fx("plain.v96.hbc");
    b[104..108].copy_from_slice(&0u32.to_le_bytes());
    let f = BytecodeFile::parse_auto(&b).unwrap();
    assert_eq!(f.debug_info_status, DebugInfoStatus::Absent);
    assert!(!f.debug_info_status.is_failure());
    assert!(!f
        .diagnostics
        .iter()
        .any(|d| matches!(d, Diagnostic::DebugInfoUnreadable(_))));
}

// F11. The `<string:N>` placeholder is counted, because a non-zero count is the
// strongest available signal that the buffer sections were read at wrong offsets.
#[test]
fn unresolved_literal_string_ids_are_counted() {
    use std::sync::atomic::Ordering;

    let mut f = BytecodeFile::parse_auto(&fx("plain.v96.hbc")).unwrap();
    assert_eq!(f.unresolved_string_ids.load(Ordering::Relaxed), 0);

    // A ByteString tag (0x60) with length 1 naming an id far past the table.
    // Reading it back through the public accessor is what a caller would do.
    f.obj_key_buffer = vec![0x61u8, 0xff];
    let values = f.read_key_buffer_series(0, 1).unwrap();
    assert!(matches!(&values[0],
        hbc_decomp::file::LiteralValue::String(s) if s.starts_with("<string:")));
    assert_eq!(
        f.unresolved_string_ids.load(Ordering::Relaxed),
        1,
        "the placeholder must be counted, not just returned"
    );
    assert!(!f.is_clean());
    assert!(f.warnings().iter().any(|w| w.contains("<string:N>")));
}

// F5. The overflow bit is set only on the *small* header on disk; the parser has
// to reinstate it when it follows the pointer to a large one. The modern path did;
// the legacy path did not, and legacy is the v96 case this repo patches.
#[test]
fn overflowed_legacy_headers_report_themselves_as_overflowed() {
    // The committed fixtures are three-function programs and cannot overflow a
    // header, so this needs the production bundle. Gated through the shared oracle
    // helper so `HBC_REQUIRE_ORACLES=corpus` makes an absent one a hard failure --
    // a test that silently proves nothing is the defect that module exists for.
    let Some(path) = common::oracle_path(Oracle::Corpus, None, |p| p.is_file(), "an existing file")
    else {
        common::skip_or_fail(Oracle::Corpus, None, "HBC_CORPUS_BUNDLE not set");
        return;
    };
    let bytes = std::fs::read(&path).unwrap();
    let file = BytecodeFile::parse_auto(&bytes).unwrap();
    if file.header.function_header_layout != FunctionHeaderLayout::Legacy16 {
        eprintln!("[skip] {} is not a Legacy16 image", path.display());
        return;
    }

    // Ground truth straight from the raw table: byte 15 of each 16-byte small
    // header is `flags`, and 0x20 is Overflowed.
    let table = 128usize;
    let n = file.header.function_count as usize;
    let raw = (0..n)
        .filter(|i| {
            bytes
                .get(table + i * 16 + 15)
                .is_some_and(|f| f & 0x20 != 0)
        })
        .count();
    let parsed = file
        .function_headers
        .iter()
        .filter(|h| h.is_overflowed())
        .count();

    assert_eq!(
        raw, parsed,
        "the Overflowed bit must survive following the large-header pointer"
    );
    // A frame_size above 127 cannot fit the small header's 7-bit field, so any
    // such function demonstrably came from a large header and must be flagged.
    for h in file.function_headers.iter().filter(|h| h.frame_size() > 127) {
        assert!(
            h.is_overflowed(),
            "function {} has frame_size {} (impossible in a small header) but \
             is_overflowed() is false",
            h.function_id(),
            h.frame_size()
        );
    }
}
