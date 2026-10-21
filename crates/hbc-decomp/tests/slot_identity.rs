//! The slot identity check (closure model K4.3) over a real bundle, against a
//! recorded ceiling.
//!
//! Each violation the checker counts is a variable the output prints under the
//! wrong name, or under one name meaning two things (`K4_PLAN.md`, I1-I3). The
//! counts are not zero yet; later K4 stages are judged by them falling. So the
//! test fails on the way *up*, the pattern `scripts/build/syntax_baseline.tsv`
//! uses for parse failures: lower a ceiling whenever the real number drops, so a
//! regression cannot hide under an old allowance.
//!
//! Ceilings live in `slot_identity_baseline.tsv`, keyed by the bundle's SHA-256,
//! because `HBC_CORPUS_BUNDLE` can name any bundle. One the file does not list
//! prints its counts, to be recorded, and passes. Skips cleanly when
//! `HBC_CORPUS_BUNDLE` is unset (see `common`).

mod common;
use common::Oracle;

use hbc_decomp::{BytecodeFile, BytecodeFormat, PipelineContext};

const BASELINE: &str = include_str!("slot_identity_baseline.tsv");

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// (label, I1, I2, I3) for a bundle digest.
fn ceiling(digest: &str) -> Option<(String, usize, usize, usize)> {
    BASELINE.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 || cols[0] != digest {
            return None;
        }
        Some((
            cols[1].to_string(),
            cols[2].parse().ok()?,
            cols[3].parse().ok()?,
            cols[4].parse().ok()?,
        ))
    })
}

#[test]
fn slot_identity_violations_do_not_rise() {
    let Some(path) = common::oracle_path(Oracle::Corpus, None, |p| p.is_file(), "an existing file")
    else {
        common::skip_or_fail(Oracle::Corpus, None, "HBC_CORPUS_BUNDLE not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read corpus bundle");
    let file = BytecodeFile::parse_auto(&bytes).expect("corpus parses");
    let format = BytecodeFormat::for_version(file.header.version).expect("opcode table");
    let ctx = PipelineContext::build(&file, &format).expect("pipeline builds");
    let v = ctx
        .check_slot_identity(&file)
        .expect("the pipeline builds a closure context");
    let (i1, i2, i3) = v.counts();
    let digest = sha256_hex(&bytes);
    println!(
        "  {}: I1 {i1}  I2 {i2}  I3 {i3}  (aliases {})",
        path.display(),
        v.aliases
    );
    let Some((label, c1, c2, c3)) = ceiling(&digest) else {
        println!(
            "  no ceiling recorded; add to tests/slot_identity_baseline.tsv:\n  \
             {digest}\t<label>\t{i1}\t{i2}\t{i3}"
        );
        return;
    };
    assert!(
        i1 <= c1 && i2 <= c2 && i3 <= c3,
        "{label}: slot identity violations rose above the recorded ceiling: \
         I1 {i1} (ceiling {c1}), I2 {i2} (ceiling {c2}), I3 {i3} (ceiling {c3}). \
         Run with RUST_LOG=slots=trace and HD_SLOTS_DUMP=<path> to list them."
    );
    if (i1, i2, i3) < (c1, c2, c3) {
        println!("  {label}: below the ceiling ({c1}, {c2}, {c3}); lower it to the new counts");
    }
}
