//! Run the read and write paths over a real production bundle.
//!
//! The committed fixtures are three-function programs with six strings. A shipped
//! React Native bundle is a different object: ~63,000 functions, ~99,000 strings,
//! and — the reason this file exists — **1,449 overflowed string-table entries**,
//! of which the fixtures contain exactly zero.
//!
//! Overflow handling (I8) is where the recurring bug class lives. Finding F1 #3
//! was a dead `offset == 0x800000` overflow test that could never fire, because
//! the offset field is masked to 23 bits and the real sentinel is
//! `length == 0xff`. `create` cannot emit an overflowed entry at all, so no
//! synthetic fixture can reach that code. Until this harness, nothing did.
//!
//! Scale is not the point on its own. What a corpus buys is *variety that cannot
//! be constructed*: overflowed entries, UTF-16 strings, functions with real
//! exception tables, string ids wide enough to stress operand widths (I11), and
//! every opcode the compiler actually emits rather than the dozen a fixture uses.
//!
//! ## Running it
//!
//! ```text
//! HBC_CORPUS_BUNDLE=C:\...\index.android.bundle.backup
//! HBC_CORPUS_LIMIT=2000            # functions to sweep; 0 = all (default 2000)
//! HERMES_HBCDUMP_V96=C:\src\hermes-v96\build\bin\Release\hbcdump.exe
//! ```
//!
//! The bundle is a third-party artifact and is deliberately not committed, so
//! everything here skips cleanly when `HBC_CORPUS_BUNDLE` is unset.
//! `HBC_REQUIRE_ORACLES=corpus,hbcdump` makes those skips failures where the
//! artifacts are expected to be present. See `common`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

mod common;
use common::Oracle;

use hbc_decomp::write::patch::PatchOptions;
use hbc_decomp::{
    add_string, encode_function_body, patch_function_bytes, regexp_payload, repoint_regexp,
    retarget_string, set_regexp, BytecodeFile, BytecodeFormat, FunctionHeader, OperandTarget,
    RegexpSetMode,
};

fn corpus() -> Option<(Vec<u8>, PathBuf)> {
    let path = common::oracle_path(Oracle::Corpus, None, |p| p.is_file(), "an existing file")?;
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("HBC_CORPUS_BUNDLE={}: {e}", path.display()));
    Some((bytes, path))
}

/// How many functions to sweep in the per-function tests. Bounded by default so
/// `cargo test` stays quick; `HBC_CORPUS_LIMIT=0` sweeps the whole bundle.
fn limit() -> usize {
    match std::env::var("HBC_CORPUS_LIMIT").ok().as_deref() {
        Some("0") => usize::MAX,
        Some(n) => n.parse().unwrap_or(2000),
        None => 2000,
    }
}

macro_rules! corpus_or_skip {
    () => {
        match corpus() {
            Some(v) => v,
            None => {
                common::skip_or_fail(Oracle::Corpus, None, "HBC_CORPUS_BUNDLE not set");
                return;
            }
        }
    };
}

fn section(file: &BytecodeFile, name: &str) -> Option<(usize, usize)> {
    file.sections
        .iter()
        .find(|s| s.name == name)
        .map(|s| (s.offset as usize, s.size as usize))
}

// ---------------------------------------------------------------------------
// I8 -- overflowed string entries, against real ones
// ---------------------------------------------------------------------------

/// Independently scan the small string table for the overflow sentinel and
/// require it to agree with the header's count.
///
/// This is deliberately a *second* implementation of the rule rather than a call
/// into the crate: the point is to check the crate's sentinel against the format,
/// on 1,449 real entries. A regression to the dead `offset == 0x800000` test would
/// find zero of them and fail here.
#[test]
fn overflow_entries_are_found_by_the_length_sentinel() {
    let (bytes, path) = corpus_or_skip!();
    let file = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let (off, size) = section(&file, "small_string_table").expect("small_string_table section");

    // SmallStringTableEntry: isUTF16:1 | offset:23 | length:8, and an entry is
    // overflowed exactly when its length field reads 0xff (Hermes'
    // `isOverflowed()` is literally `getLength() == INVALID_LENGTH`).
    let mut overflowed = 0usize;
    let mut utf16 = 0usize;
    for i in 0..(size / 4) {
        let at = off + i * 4;
        let raw = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        if (raw >> 24) & 0xff == 0xff {
            overflowed += 1;
        }
        if raw & 1 == 1 {
            utf16 += 1;
        }
    }

    assert_eq!(
        overflowed,
        file.header.overflow_string_count as usize,
        "{}: found {overflowed} entries with length == 0xff but the header declares \
         {} overflowed strings",
        path.display(),
        file.header.overflow_string_count
    );
    assert!(
        overflowed > 0,
        "this corpus has no overflowed string entries, so it does not exercise I8 — \
         the whole reason for using a real bundle. Point HBC_CORPUS_BUNDLE at a \
         production bundle."
    );
    println!("  corpus: {overflowed} overflowed entries, {utf16} UTF-16 entries");
}

/// `retarget_string` documents that it refuses overflowed entries (v1 scope).
/// Assert that against real ones, and that it does *not* refuse ordinary entries —
/// a refusal that fires on everything would satisfy the first half alone.
#[test]
fn retarget_refuses_overflowed_entries_and_accepts_normal_ones() {
    let (bytes, _) = corpus_or_skip!();
    let file = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(file.header.version).expect("opcode table");
    let (off, size) = section(&file, "small_string_table").expect("small_string_table section");

    let mut overflowed_ids = Vec::new();
    let mut normal_ids = Vec::new();
    for i in 0..(size / 4) {
        let raw = u32::from_le_bytes(bytes[off + i * 4..off + i * 4 + 4].try_into().unwrap());
        if (raw >> 24) & 0xff == 0xff {
            overflowed_ids.push(i as u32);
        } else {
            normal_ids.push(i as u32);
        }
    }
    assert!(
        !overflowed_ids.is_empty(),
        "corpus has no overflowed entries"
    );

    // A handful of each is enough; every op now re-derives the model, so these are
    // full write cycles rather than cheap probes.
    let target = normal_ids[0];
    for &id in overflowed_ids.iter().take(5) {
        let mut f = BytecodeFile::parse_auto(&bytes).unwrap();
        let err = retarget_string(&mut f, &format, id, target, &PatchOptions::default())
            .expect_err("retarget must refuse an overflowed source entry");
        assert!(
            err.to_string().contains("overflow"),
            "string {id} is overflowed; the refusal should say so, got: {err}"
        );
    }
    for &id in normal_ids.iter().skip(1).take(5) {
        let mut f = BytecodeFile::parse_auto(&bytes).unwrap();
        retarget_string(&mut f, &format, id, target, &PatchOptions::default())
            .unwrap_or_else(|e| panic!("string {id} is not overflowed but was refused: {e}"));
    }
}

// ---------------------------------------------------------------------------
// Encode/decode symmetry, over every opcode the compiler actually emits
// ---------------------------------------------------------------------------

/// Decode each function body and re-encode it; the bytes must come back
/// identical.
///
/// A fixture exercises maybe a dozen opcodes. A shipped bundle exercises whatever
/// the compiler emits across 63,000 functions, which is the only way to notice an
/// operand-width or instruction-length error in an opcode nothing small happens to
/// use. That is the same failure mode as the v99 `NewFastArray` drift: one wrong
/// operand count silently desynchronises the rest of a body.
#[test]
fn every_function_body_round_trips_through_encode() {
    let (bytes, _) = corpus_or_skip!();
    let file = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(file.header.version).expect("opcode table");

    let total = file.header.function_count as usize;
    let n = limit().min(total);
    let mut failures = Vec::new();
    let mut checked = 0usize;

    for id in 0..n as u32 {
        let Some(header) = file.function_headers.get(id as usize) else {
            continue;
        };
        let start = header.offset() as usize;
        let end = start + header.bytecode_size_in_bytes() as usize;
        if end > bytes.len() {
            failures.push(format!(
                "fn#{id}: body [{start}, {end}) is outside the image"
            ));
            continue;
        }
        let original = &bytes[start..end];

        let instrs = match file.decode_function_instructions(&format, id) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("fn#{id}: decode failed: {e}"));
                continue;
            }
        };
        match encode_function_body(&format, &instrs) {
            Ok(re) if re == original => checked += 1,
            Ok(re) => {
                let at = re
                    .iter()
                    .zip(original)
                    .position(|(a, b)| a != b)
                    .unwrap_or(re.len().min(original.len()));
                failures.push(format!(
                    "fn#{id}: re-encode differs at byte {at} (orig {} bytes, re-encoded {})",
                    original.len(),
                    re.len()
                ));
            }
            Err(e) => failures.push(format!("fn#{id}: encode failed: {e}")),
        }
        if failures.len() > 20 {
            break;
        }
    }

    assert!(
        failures.is_empty(),
        "encode/decode is not symmetric for {} of {checked} functions checked:\n{}",
        failures.len(),
        failures.join("\n")
    );
    println!("  round-tripped {checked} of {total} function bodies");
}

// ---------------------------------------------------------------------------
// I6 -- relocation across a real 63,000-function bundle
// ---------------------------------------------------------------------------

/// Appending a string grows the string region, which precedes all code, so
/// **every** function offset must shift by exactly the same delta (I6's
/// string-region case).
///
/// On the fixtures this moves three functions. Here it moves ~63,000, including
/// overflowed ones whose real offsets live in an out-of-line large header — the
/// case a small fixture cannot produce at all.
#[test]
fn appending_a_string_shifts_every_function_offset_uniformly() {
    let (bytes, _) = corpus_or_skip!();
    let before = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(before.header.version).expect("opcode table");
    let before_offsets: Vec<u32> = before.function_headers.iter().map(|h| h.offset()).collect();
    let before_sizes: Vec<u32> = before
        .function_headers
        .iter()
        .map(|h| h.bytecode_size_in_bytes())
        .collect();

    let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
    let (out, new_id) = add_string(
        &mut file,
        &format,
        "HBC_CORPUS_PROBE",
        false,
        &PatchOptions::default(),
    )
    .expect("add_string on the corpus");
    assert_eq!(
        new_id, before.header.string_count,
        "I10: a new string takes the next id and nothing is renumbered"
    );

    let after = BytecodeFile::parse_auto(&out).expect("patched corpus reparses");
    assert_eq!(
        after.function_headers.len(),
        before_offsets.len(),
        "function count changed"
    );

    let deltas: BTreeMap<i64, usize> = after.function_headers.iter().zip(&before_offsets).fold(
        BTreeMap::new(),
        |mut acc, (h, &was)| {
            *acc.entry(h.offset() as i64 - was as i64).or_default() += 1;
            acc
        },
    );
    assert_eq!(
        deltas.len(),
        1,
        "every function body should shift by the same delta; saw {deltas:?}"
    );

    for (id, (h, &was)) in after
        .function_headers
        .iter()
        .zip(&before_sizes)
        .enumerate()
        .take(limit())
    {
        assert_eq!(
            h.bytecode_size_in_bytes(),
            was,
            "fn#{id}: a string append must not change any body size"
        );
    }
    println!(
        "  {} functions all shifted by {}",
        after.function_headers.len(),
        deltas.keys().next().unwrap()
    );
}

// string_packing/PLAN.md P1 at full scale: a rebuild that packs suffixes must
// recover a real share of what the unpacked rebuild costs against hermesc's own
// (fully optimised) layout, and every entry must still read back as before --
// which `add_string` checks on every write (P0), so success here is that check
// passing over ~99,000 entries.
#[test]
fn a_packed_rebuild_recovers_storage_the_unpacked_one_spends() {
    let (bytes, _) = corpus_or_skip!();
    let original = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(original.header.version).expect("opcode table");
    let storage = |unpacked: bool| {
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let opts = PatchOptions {
            unpacked_strings: unpacked,
            ..PatchOptions::default()
        };
        let (out, _) = add_string(&mut file, &format, "HBC_CORPUS_PACK_PROBE", false, &opts)
            .expect("add_string on the corpus");
        let after = BytecodeFile::parse_auto(&out).unwrap();
        // Same ids, same text: packing moves offsets, never content.
        for (i, (a, b)) in after.strings.iter().zip(&original.strings).enumerate() {
            assert_eq!(a.value, b.value, "unpacked={unpacked}: string {i}");
        }
        after.header.string_storage_size as i64
    };
    let hermesc = original.header.string_storage_size as i64;
    let unpacked = storage(true);
    let packed = storage(false);
    let cost = unpacked - hermesc;
    let recovered = unpacked - packed;
    println!(
        "  storage: hermesc {hermesc}, unpacked rebuild {unpacked} (+{cost}), \
         packed rebuild {packed} (recovers {recovered}, {:.0}%)",
        100.0 * recovered as f64 / cost as f64
    );
    assert!(
        packed < unpacked,
        "packing saved nothing: the sort is backwards"
    );
    // Suffix merge is about a quarter of what hermesc's three-stage packer
    // finds (plan: ~100 KB of ~431 KB on 11.39.0). Well under that means the
    // merge is missing containers it should find.
    assert!(
        recovered * 5 >= cost,
        "packing recovered {recovered} of {cost}; expected at least a fifth"
    );
}

/// A header with everything a relocation may change blanked out: the body
/// offset, the info offset and the body size. What is left must never move.
fn fixed_fields(h: &FunctionHeader) -> String {
    match h.clone() {
        FunctionHeader::Legacy(mut l) => {
            (l.offset, l.info_offset, l.bytecode_size_in_bytes) = (0, 0, 0);
            format!("{l:?}")
        }
        FunctionHeader::Modern(mut m) => {
            (m.offset, m.info_offset, m.bytecode_size_in_bytes) = (0, 0, 0);
            format!("{m:?}")
        }
    }
}

fn info_offset(h: &FunctionHeader) -> u32 {
    match h {
        FunctionHeader::Legacy(l) => l.info_offset,
        FunctionHeader::Modern(m) => m.info_offset,
    }
}

// R26 -- the string paths and the body path relocate through one primitive, and
// this is the assertion three hand-written copies could never make: the same
// delta, spliced at two different points, must move every header past both
// points identically. It also pins P2: a body resize rewrites only the offset
// and size bits, so no other decoded field of any header may change.
#[test]
fn a_string_append_and_a_body_grow_relocate_identically() {
    let (bytes, _) = corpus_or_skip!();
    let before = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(before.header.version).expect("opcode table");

    let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
    let (by_string, _) = add_string(
        &mut file,
        &format,
        "HBC_CORPUS_RELOC_PROBE",
        false,
        &PatchOptions::default(),
    )
    .expect("add_string on the corpus");
    let by_string = BytecodeFile::parse_auto(&by_string).unwrap();
    let delta =
        by_string.function_headers[0].offset() as i64 - before.function_headers[0].offset() as i64;
    assert!(delta > 0 && delta % 4 == 0, "string append delta {delta}");

    // Grow the earliest body that has room, so nearly every header is past both
    // splice points. The padding is never executed; only the headers matter here.
    let small_max = if before.header.function_header_layout
        == hbc_decomp::format::FunctionHeaderLayout::Modern12
    {
        1 << 14
    } else {
        1 << 15
    };
    let target = (0..before.function_headers.len())
        .filter(|&i| {
            let h = &before.function_headers[i];
            h.is_overflowed() || (h.bytecode_size_in_bytes() as i64 + delta) < small_max
        })
        .min_by_key(|&i| before.function_headers[i].offset())
        .expect("a growable function");
    let abc = format
        .definitions
        .iter()
        .find(|d| d.name == "AsyncBreakCheck")
        .expect("AsyncBreakCheck")
        .opcode;
    let th = &before.function_headers[target];
    let (t_off, t_size) = (th.offset() as usize, th.bytecode_size_in_bytes() as usize);
    let mut body = bytes[t_off..t_off + t_size].to_vec();
    body.resize(t_size + delta as usize, abc);
    let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
    let by_body = patch_function_bytes(&mut file, target as u32, &body).expect("body grow");
    let by_body = BytecodeFile::parse_auto(&by_body).unwrap();

    assert_eq!(
        by_string.header.debug_info_offset, by_body.header.debug_info_offset,
        "debug_info_offset relocated differently"
    );
    let mut past_both = 0usize;
    for (i, was) in before.function_headers.iter().enumerate() {
        let (s, b) = (&by_string.function_headers[i], &by_body.function_headers[i]);
        assert_eq!(
            fixed_fields(was),
            fixed_fields(s),
            "fn#{i}: add_string changed a fixed field"
        );
        assert_eq!(
            fixed_fields(was),
            fixed_fields(b),
            "fn#{i}: body grow changed a fixed field"
        );
        // FunctionInfo (and every large header) sits after all the code, so it is
        // past both splice points for every function, the grown one included.
        assert_eq!(
            info_offset(s),
            info_offset(b),
            "fn#{i}: info offset relocated differently"
        );
        if i == target {
            assert_eq!(
                b.offset(),
                was.offset(),
                "the grown body itself must not move"
            );
            assert_eq!(b.bytecode_size_in_bytes() as usize, t_size + delta as usize);
        } else if was.offset() as usize >= t_off + t_size {
            past_both += 1;
            assert_eq!(
                s.offset(),
                b.offset(),
                "fn#{i}: body offset relocated differently"
            );
        } else {
            assert_eq!(
                b.offset(),
                was.offset(),
                "fn#{i} sits before the grown body"
            );
        }
    }
    assert!(
        past_both > 0,
        "no header past both splices; the differential checked nothing"
    );
    println!(
        "  {past_both} of {} headers past both splices relocated identically (delta {delta}, grew fn#{target})",
        before.function_headers.len()
    );
}

// P4a, archetype C on a production bundle: appending a regex stream shifts the
// whole tail -- every function body sits after regexp storage -- so it is run
// here once at full scale, not just on the three-function fixtures.
#[test]
fn a_regexp_append_moves_the_tail_and_nothing_else() {
    let (bytes, _) = corpus_or_skip!();
    let before = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let donor_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/regexp_donor.v96.hbc");
    let donor_bytes = std::fs::read(&donor_path).expect("regexp_donor fixture");
    let donor = BytecodeFile::parse_auto(&donor_bytes).unwrap();
    let payload = regexp_payload(&donor, 1).unwrap().to_vec();
    assert_eq!(payload[0], 0, "donor entry 1 has no capture group");

    // The first entry the donor may replace and cannot fit: no capture groups to
    // lose, and a slot smaller than the payload, so it must append.
    let index = (0..before.reg_exp_table.len() as u32)
        .find(|&i| {
            let p = regexp_payload(&before, i).unwrap();
            p.len() < payload.len() && p[0] == 0 && p[1] == 0
        })
        .expect("a small capture-free regex in the corpus");

    let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
    let (out, report) =
        set_regexp(&mut file, index, &payload, &PatchOptions::default()).expect("regexp-set");
    assert_eq!(report.mode, RegexpSetMode::Appended);
    let after = BytecodeFile::parse_auto(&out).expect("patched corpus reparses");

    assert_eq!(regexp_payload(&after, index).unwrap(), &payload[..]);
    for i in (0..before.reg_exp_table.len() as u32).filter(|&i| i != index) {
        assert_eq!(
            regexp_payload(&after, i).unwrap(),
            regexp_payload(&before, i).unwrap(),
            "regexp {i} changed"
        );
    }
    let deltas: BTreeSet<i64> = after
        .function_headers
        .iter()
        .zip(&before.function_headers)
        .map(|(a, b)| a.offset() as i64 - b.offset() as i64)
        .collect();
    assert_eq!(
        deltas.len(),
        1,
        "every body should shift by one delta; saw {deltas:?}"
    );
    let delta = *deltas.iter().next().unwrap();
    assert!(delta > 0 && delta % 4 == 0, "delta {delta}");
    for (i, (a, b)) in after
        .function_headers
        .iter()
        .zip(&before.function_headers)
        .enumerate()
    {
        assert_eq!(
            fixed_fields(a),
            fixed_fields(b),
            "fn#{i}: a fixed field changed"
        );
        assert_eq!(
            a.bytecode_size_in_bytes(),
            b.bytecode_size_in_bytes(),
            "fn#{i}: size"
        );
    }
    // Each body moved; none changed. (Not `instructions`: that runs to EOF and
    // takes in the large headers, whose offsets rightly changed.)
    let body = |img: &[u8], h: &FunctionHeader| {
        let o = h.offset() as usize;
        img[o..o + h.bytecode_size_in_bytes() as usize].to_vec()
    };
    let changed = (after.function_headers.iter().zip(&before.function_headers))
        .filter(|(a, b)| body(&out, a) != body(&bytes, b))
        .count();
    assert_eq!(
        changed, 0,
        "{changed} function bodies changed; they must only move"
    );
    println!(
        "  regexp {index} appended ({} -> {} bytes); {} function bodies shifted by {delta}",
        report.old_len,
        report.new_len,
        after.function_headers.len()
    );
}

// P4a, archetype A on a production bundle: repointing one CreateRegExp scans the
// whole bundle for a site that already uses the target entry, so this is where
// that scan's cost and its answer are checked at scale.
#[test]
fn a_regexp_repoint_edits_one_instruction_and_copies_its_strings() {
    let (bytes, _) = corpus_or_skip!();
    let before = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(before.header.version).expect("opcode table");

    // Every CreateRegExp site as (absolute offset, pattern, flags, index).
    let mut sites = Vec::new();
    for id in 0..before.function_headers.len() as u32 {
        let base = before.function_headers[id as usize].offset();
        for insn in before.decode_function_instructions(&format, id).unwrap() {
            if format.definitions[insn.opcode as usize].name == "CreateRegExp" {
                let v = |i: usize| insn.operands[i].value.as_u32().unwrap();
                sites.push((base + insn.offset, v(1), v(2), v(3)));
            }
        }
    }
    let marked = |i: u32| regexp_payload(&before, i).unwrap()[0];
    // A site, and an entry another site already uses that keeps its group count.
    let (site, donor) = sites
        .iter()
        .find_map(|s| {
            sites
                .iter()
                .find(|d| d.3 != s.3 && marked(d.3) >= marked(s.3))
                .map(|d| (*s, *d))
        })
        .expect("two CreateRegExp sites with different entries");

    let t = std::time::Instant::now();
    let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
    let (out, report) = repoint_regexp(
        &mut file,
        &format,
        OperandTarget::AbsoluteOffset(site.0),
        donor.3,
        &PatchOptions::default(),
    )
    .expect("repoint");
    let took = t.elapsed();

    assert_eq!(report.new_index, donor.3);
    let from = report.strings_from.expect("a site already uses the entry");
    let used = sites.iter().find(|s| s.0 == from).unwrap();
    assert_eq!(
        used.3, donor.3,
        "strings must come from a site using the new entry"
    );
    assert_eq!(out.len(), bytes.len(), "a repoint must not resize");
    let differing = bytes.iter().zip(&out).filter(|(a, b)| a != b).count();
    assert!(differing <= 12 + 20, "{differing} bytes changed");

    // Read the edited instruction back with the bundle's own decoder.
    let after = BytecodeFile::parse_auto(&out).unwrap();
    let owner = (0..after.function_headers.len() as u32)
        .filter(|&id| after.function_headers[id as usize].offset() <= site.0)
        .max_by_key(|&id| after.function_headers[id as usize].offset())
        .unwrap();
    let base = after.function_headers[owner as usize].offset();
    let insn = after
        .decode_function_instructions(&format, owner)
        .unwrap()
        .into_iter()
        .find(|i| base + i.offset == site.0)
        .expect("the edited instruction");
    let v = |i: usize| insn.operands[i].value.as_u32().unwrap();
    assert_eq!((v(1), v(2), v(3)), (used.1, used.2, donor.3));
    println!(
        "  CreateRegExp at 0x{:x}: regexp {} -> {}, strings from 0x{from:x}; {} sites, scan {took:.2?}",
        site.0,
        site.3,
        donor.3,
        sites.len()
    );
}

// ---------------------------------------------------------------------------
// The differential: our disassembly vs the engine's own
// ---------------------------------------------------------------------------

/// Normalise one disassembly line to the part both dialects agree on.
///
/// Returns `None` for anything that is not an instruction. The two dialects
/// render the same decoded values differently in three ways, none of which is a
/// decoding difference, so all three are normalised out:
///
///   * label names -- hbcdump numbers labels sequentially, this crate names them
///     by byte offset, so the names can never match;
///   * closure targets -- hbcdump prints `Function<name>12` (and a name can contain
///     spaces), this crate prints the bare function id `12`;
///   * string literals -- hbcdump clamps them to 17 characters and the two
///     dialects escape quotes, newlines and backslashes differently, so operands
///     from the first quote onward collapse to one opaque marker. That is not a
///     loss of coverage: `every_function_body_round_trips_through_encode` already
///     re-encodes every body to byte-identical bytes, which proves string ids (and
///     every other operand) are decoded correctly. Re-deriving hbcdump's escaping
///     here would test our imitation of its formatter, not our decoder.
///
/// hbcdump also prints each function's exception-handler table after the code
/// (`0: start = L1, end = L2, target = L3`); this crate's disassembly does not
/// include it, so those lines are dropped rather than compared.
fn normalise(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty()
        || line.starts_with(';')
        || line.starts_with("Function<")
        || line.starts_with("Offset in debug table")
        || line.ends_with(':')
    {
        return None;
    }
    // Exception-handler table rows: "<n>: start = ..., end = ..., target = ...".
    if line.contains("start = ") && line.contains("target = ") {
        return None;
    }

    // This crate prefixes a hex byte offset; hbcdump does not.
    let body = match line.split_once(char::is_whitespace) {
        Some((first, rest)) if first.chars().all(|c| c.is_ascii_hexdigit()) && first.len() >= 4 => {
            rest
        }
        _ => line,
    };

    // `Function<get registerCallableModule>249` -> `249`, and hbcdump's
    // `NCFunction<...>1142` (non-callable/generator inner) likewise. Done before
    // tokenising because a function name can contain spaces.
    let mut body = body.replace("NCFunction<", "Function<");
    while let Some(at) = body.find("Function<") {
        match body[at..].find('>') {
            Some(rel) => body.replace_range(at..at + rel + 1, ""),
            None => break,
        }
    }
    let body = body.as_str();

    // Everything from the first quote is string-ish and collapses to one marker.
    // Doing it before tokenising matters: string contents contain spaces and
    // commas, so they cannot be tokenised as operands.
    let (body, has_string) = match body.find('"') {
        Some(q) => (&body[..q], true),
        None => (body, false),
    };

    let mut out = String::new();
    #[allow(clippy::needless_range_loop)]
    for (i, tok) in body.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let (core, comma) = match tok.strip_suffix(',') {
            Some(c) => (c, ","),
            None => (tok, ""),
        };
        if core.len() > 1 && core.starts_with('L') && core[1..].chars().all(|c| c.is_ascii_digit())
        {
            // L12 / L1234 -- the name is dialect-specific, its presence is not.
            out.push_str("L?");
            out.push_str(comma);
        } else if let Ok(v) = core.parse::<f64>() {
            // The two dialects format numeric literals differently (notably -0
            // versus 0). Canonicalise rather than compare their spellings; the
            // encode round-trip test is what proves the value itself is right.
            let v = if v == 0.0 { 0.0 } else { v };
            out.push_str(&format!("{v}"));
            out.push_str(comma);
        } else {
            out.push_str(tok);
        }
    }
    if has_string {
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str("<str>");
    }
    // hbcdump resolves a builtin id to its name ("HermesBuiltin.apply"), which
    // this crate prints as the raw number. Across the whole Equinox bundle this
    // is the only remaining rendering difference, so keep the opcode and its
    // destination register and drop the operands that differ only in spelling.
    if out.starts_with("CallBuiltin") {
        let keep: Vec<&str> = out.split_whitespace().take(2).collect();
        out = keep.join(" ");
    }
    Some(out)
}

/// Split hbcdump's `disassemble` output into per-function instruction streams.
///
/// hbcdump prints string operands with real newlines rather than escapes, so one
/// instruction can span several output lines, and it does not escape embedded
/// quotes either -- so neither line breaks nor quote counting can be trusted to
/// find the end of a literal.
///
/// Use the opcode table instead: a real instruction line begins with a known
/// mnemonic, and a stray fragment of a string does not. A function containing any
/// line that is neither an instruction nor recognised furniture is reported as
/// unalignable and skipped rather than compared, because a desynchronised stream
/// would report every following instruction as a mismatch.
///
/// Keyed by *function id*, parsed from the header line, never by position:
/// hbcdump omits the outer stubs of generator functions from `disassemble`
/// output (it jumps 1137 -> 1139 -> 1141), so the two lists have different
/// lengths and positional alignment silently compares different functions.
///
/// Returns (by-id, skipped).
fn hbcdump_functions(
    text: &str,
    mnemonics: &BTreeSet<String>,
) -> (BTreeMap<u32, Option<Vec<String>>>, usize) {
    let mut out: BTreeMap<u32, Option<Vec<String>>> = BTreeMap::new();
    let mut current: Option<(u32, Vec<String>)> = None;
    let mut usable = true;
    let mut skipped = 0usize;

    // `Function<some name>1139(2 params, ...)` -> 1139
    let header_id = |line: &str| -> Option<u32> {
        let after = line.rsplit_once('>')?.1;
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    };

    for line in text.lines() {
        // Function headers are the one anchor that is reliable: they start at
        // column 0, while every instruction and continuation is indented.
        if line.starts_with("Function<") || line.starts_with("NCFunction<") {
            if let Some((id, f)) = current.take() {
                if usable {
                    out.insert(id, Some(f));
                } else {
                    out.insert(id, None);
                    skipped += 1;
                }
            }
            usable = true;
            current = header_id(line).map(|id| (id, Vec::new()));
            continue;
        }
        let Some((_, f)) = current.as_mut() else {
            continue;
        };

        let t = line.trim();
        if t.is_empty()
            || t.starts_with(';')
            || t.starts_with("Offset in debug table")
            || t.ends_with(':')
            || (t.contains("start = ") && t.contains("target = "))
        {
            continue;
        }
        let first = t.split_whitespace().next().unwrap_or("");
        if !mnemonics.contains(first) {
            // A fragment of a multi-line string literal, so this function's line
            // stream cannot be aligned with ours.
            usable = false;
            continue;
        }
        if let Some(n) = normalise(line) {
            f.push(n);
        }
    }
    if let Some((id, f)) = current.take() {
        if usable {
            out.insert(id, Some(f));
        } else {
            out.insert(id, None);
            skipped += 1;
        }
    }
    (out, skipped)
}

/// Compare this crate's disassembly against `hbcdump`'s, function by function.
///
/// hbcdump is an independent implementation reading the same bytes, which makes it
/// the only oracle available for the read path that does not simply restate our own
/// assumptions. It is also the check that generalises: the fixtures exercise a
/// dozen opcodes, while a differential over a real bundle covers every opcode the
/// compiler emits — which is the class the v99 opcode-table drift belonged to.
#[test]
fn disassembly_matches_hbcdump() {
    let (bytes, path) = corpus_or_skip!();
    let file = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let version = file.header.version;

    let Some(dump) = common::oracle_path(
        Oracle::HbcDump,
        Some(version),
        |p| p.is_file(),
        "an existing file",
    ) else {
        common::skip_or_fail(
            Oracle::HbcDump,
            Some(version),
            &format!("no HERMES_HBCDUMP_V{version}"),
        );
        return;
    };

    // hbcdump is an interactive REPL; feed it one command on stdin.
    let mut child = Command::new(&dump)
        .arg("-mode=objdump")
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("spawning {}: {e}", dump.display()));
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("hbcdump stdin");
        writeln!(stdin, "disassemble").expect("write hbcdump command");
    }
    let out = child.wait_with_output().expect("hbcdump output");
    let text = String::from_utf8_lossy(&out.stdout);
    let format = BytecodeFormat::for_version(version).expect("opcode table");
    let mnemonics: BTreeSet<String> = format.definitions.iter().map(|d| d.name.clone()).collect();
    let (theirs, unalignable) = hbcdump_functions(&text, &mnemonics);
    assert!(
        theirs.len() > 1,
        "hbcdump produced {} functions; it may have failed on this bundle",
        theirs.len()
    );
    let options = hbc_decomp::DisasmOptions {
        show_offsets: true,
        show_labels: true,
        resolve_strings: true,
        enable_color: false,
    };

    let ids: Vec<u32> = theirs
        .keys()
        .copied()
        .filter(|id| *id < file.header.function_count)
        .take(limit())
        .collect();
    let n = ids.len();
    let mut compared = 0usize;
    let mut mismatches = Vec::new();
    for &id in &ids {
        let ours_text = match hbc_decomp::disassemble_function(&file, &format, id, &options) {
            Ok(t) => t,
            Err(e) => {
                mismatches.push(format!("fn#{id}: our disassembly failed: {e}"));
                continue;
            }
        };
        let Some(theirs) = theirs[&id].as_ref() else {
            continue; // multi-line string literal; not alignable, counted below
        };
        compared += 1;
        let ours: Vec<String> = ours_text.lines().filter_map(normalise).collect();
        if &ours != theirs {
            let at = ours
                .iter()
                .zip(theirs)
                .position(|(a, b)| a != b)
                .unwrap_or(ours.len().min(theirs.len()));
            mismatches.push(format!(
                "fn#{id} line {at}:\n     ours: {}\n  hbcdump: {}",
                ours.get(at).map(String::as_str).unwrap_or("<end>"),
                theirs.get(at).map(String::as_str).unwrap_or("<end>")
            ));
        }
        if mismatches.len() > 10 {
            break;
        }
    }

    assert!(
        mismatches.is_empty(),
        "disassembly disagrees with hbcdump ({} of {n} functions):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    println!(
        "  {compared} of {n} functions match hbcdump instruction for instruction          ({unalignable} skipped: hbcdump split a string literal across lines)"
    );
}
