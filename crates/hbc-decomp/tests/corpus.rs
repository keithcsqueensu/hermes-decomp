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
use hbc_decomp::{add_string, encode_function_body, retarget_string, BytecodeFile, BytecodeFormat};

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
