//! Re-derive this crate's view of the bytecode format from a Hermes checkout and
//! assert it still agrees.
//!
//! This is the tripwire for R19, and it is the only test here that guards against
//! a *repeat* of the v99 episode rather than a regression of it.
//!
//! Upstream reshaped the modern function header twice inside the "modern" era and
//! bumped `BYTECODE_VERSION` for neither. Nothing in this crate noticed, because
//! nothing re-derives: `modern_layout.rs` was hand-transcribed from the header at
//! some point, `resources/bytecode/Bytecode*.json` was generated from a different
//! upstream commit, and the two drifted apart without a signal. `Bytecode99.json`
//! even recorded the commit it came from, and nothing read it — that is now
//! `BytecodeFormat::git_commit_hash`, checked by
//! `tables_record_the_commit_they_came_from` below.
//!
//! These tests parse the upstream sources directly, so they fail when a checkout
//! disagrees with what we encode — including the case where upstream changes the
//! format *without* changing the version, which is the case that bit.
//!
//! ## Running them
//!
//! One env var per checkout, pointing at a Hermes source tree:
//!
//! ```text
//! HERMES_SRC_V96=C:\src\hermes-v96
//! HERMES_SRC_V97=C:\src\hermes-v97
//! HERMES_SRC_V98=C:\src\hermes-v98
//! HERMES_SRC_V99=C:\src\hermes-v99
//! ```
//!
//! `scripts/fetch_pinned_hermes.py` creates them from the commits the tables record
//! (source only, no build); `scripts/build_hermes_vm.ps1` creates them as worktrees
//! of an existing clone, with a VM built beside them. With none set the tests pass
//! while asserting nothing and print a skip note — same trade as `vm_verify`, and
//! `HBC_REQUIRE_ORACLES=src` turns that skip into a failure (see `common`).
//!
//! ⚠️ `HERMES_SRC_V99` must be the React Native release branch,
//! `origin/260318099.0.0-stable`, and *not* `static_h`. Both declare
//! `BYTECODE_VERSION = 99` and their `BytecodeFileFormat.h` is byte-identical, so
//! `modern_layout_matches_upstream_headers` passes against either — only
//! `opcode_tables_match_upstream` can tell them apart, because `static_h` carries a
//! later `NewFastArray` taking a third operand (upstream d4f5193f0). Pointing this
//! at `static_h` therefore fails with an operand-count mismatch on exactly one
//! opcode, which is the signal working as intended, not a misconfiguration to
//! silence.
//!
//! `scripts/gen_bytecode_table.py` is the other half of this loop: when a checkout
//! legitimately moves, it re-derives `Bytecode<N>.json` from that checkout rather
//! than leaving 220 entries to be hand-edited.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hbc_decomp::modern_layout::ModernLayout;
use hbc_decomp::BytecodeFormat;

mod common;
use common::Oracle;

const CHECKOUT_VERSIONS: [u32; 4] = [96, 97, 98, 99];

/// `git rev-parse HEAD` in a checkout, if it is a git tree at all.
///
/// `None` covers the legitimate cases -- a source tarball, an export, no git on
/// PATH -- which are not failures; the content assertions still run. What it must
/// not do is quietly return the wrong commit.
fn checkout_head(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", &root.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let head = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (head.len() == 40).then_some(head)
}

/// The recorded provenance has to be true.
///
/// R19 in one sentence: two artifacts derived from *different* upstream commits,
/// with nothing checking either claim. The content tests below compare us against
/// whatever checkout is configured, which is the real assertion -- but on its own
/// it cannot tell "upstream reshaped the format" from "you pointed the env var at
/// the wrong tree", and those want opposite responses. This separates them, and it
/// is why `GitCommitHash` is now parsed into `BytecodeFormat` instead of sitting in
/// the JSON as a comment nothing read.
#[test]
fn tables_record_the_commit_they_came_from() {
    let mut checked = 0;
    for version in CHECKOUT_VERSIONS {
        let format = BytecodeFormat::for_version(version)
            .unwrap_or_else(|e| panic!("v{version} table failed to load: {e}"));
        let recorded = format.git_commit_hash.unwrap_or_else(|| {
            panic!(
                "Bytecode{version}.json records no GitCommitHash. Every table must say \
                 which upstream commit it came from, or its agreement with a checkout \
                 proves nothing about which dialect it encodes. Regenerate it with \
                 scripts/gen_bytecode_table.py --commit <sha>."
            )
        });
        assert_eq!(
            recorded.len(),
            40,
            "v{version}: GitCommitHash is not a full 40-character sha: {recorded}"
        );

        let Some(root) = checkout_for(version) else {
            continue;
        };
        let Some(head) = checkout_head(&root) else {
            common::skip_or_fail(
                Oracle::Src,
                Some(version),
                &format!("HERMES_SRC_V{version} is not a git checkout; provenance unchecked"),
            );
            continue;
        };
        assert_eq!(
            head,
            recorded,
            "v{version}: HERMES_SRC_V{version} points at {} which is at {head}, but \
             Bytecode{version}.json was derived from {recorded}.\n\
             Both may legitimately declare BYTECODE_VERSION {version} and still encode \
             different dialects -- that is exactly the v99 episode. Point the env var at \
             {recorded}, or regenerate the table from this checkout if the move is intended.",
            root.display()
        );
        checked += 1;
    }
    if checked == 0 {
        common::skip_or_fail(
            Oracle::Src,
            None,
            "no HERMES_SRC_V* checkouts configured; provenance unchecked",
        );
    }
}

fn checkout_for(version: u32) -> Option<PathBuf> {
    common::oracle_path(
        Oracle::Src,
        Some(version),
        |p| p.join("include/hermes/BCGen/HBC/BytecodeFileFormat.h").is_file(),
        "a Hermes source tree (no include/hermes/BCGen/HBC/BytecodeFileFormat.h under it)",
    )
}

fn read(root: &Path, rel: &str) -> String {
    let path = root.join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// `BYTECODE_VERSION` as the checkout declares it.
fn upstream_version(root: &Path) -> u32 {
    let src = read(root, "include/hermes/BCGen/HBC/BytecodeVersion.h");
    src.split("BYTECODE_VERSION")
        .nth(1)
        .and_then(|rest| rest.split('=').nth(1))
        .and_then(|rest| {
            rest.trim()
                .trim_end_matches(';')
                .split(|c: char| !c.is_ascii_digit())
                .next()
        })
        .and_then(|n| n.parse().ok())
        .expect("BYTECODE_VERSION constant")
}

/// Strip C block and line comments so they cannot contribute fake matches.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let (mut i, mut block, mut line) = (0usize, false, false);
    while i < bytes.len() {
        let two = bytes.get(i..i + 2);
        if block {
            if two == Some(b"*/") {
                block = false;
                i += 2;
                continue;
            }
        } else if line {
            if bytes[i] == b'\n' {
                line = false;
                out.push('\n');
            }
        } else if two == Some(b"/*") {
            block = true;
            i += 2;
            continue;
        } else if two == Some(b"//") {
            line = true;
            i += 2;
            continue;
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

// ---------------------------------------------------------------------------
// The function-header layout (what ModernLayout encodes)
// ---------------------------------------------------------------------------

/// One entry of upstream's `FUNC_HEADER_FIELDS` X-macro.
#[derive(Debug)]
struct HeaderField {
    name: String,
    /// The *api* type, which is what `struct FunctionHeader` (the large header)
    /// stores. The separate storage type only affects the small header's
    /// bitfield packing.
    api_bytes: usize,
}

/// Parse `FUNC_HEADER_FIELDS` out of BytecodeFileFormat.h.
///
/// Entries look like
/// ```text
///   F(uint32_t, w1, uint32_t, Offset, 25)   \
///   N(Offset, w1, uint32_t, ParamCount, 5)  \
/// ```
/// i.e. `MACRO(<storage-or-prev>, <storage-name>, <api-type>, <Name>, <bits>)`.
/// The api type is always arg 3 and the field name arg 4 in both forms, which is
/// what makes this derivable rather than transcribed.
fn parse_func_header_fields(root: &Path) -> Vec<HeaderField> {
    let src = strip_comments(&read(root, "include/hermes/BCGen/HBC/BytecodeFileFormat.h"));
    let start = src
        .find("#define FUNC_HEADER_FIELDS")
        .expect("FUNC_HEADER_FIELDS macro");
    // The X-macro is one logical line continued with backslashes; it ends at the
    // first line that does not continue.
    let mut body = String::new();
    for line in src[start..].lines() {
        body.push_str(line.trim_end().trim_end_matches('\\'));
        body.push('\n');
        if !line.trim_end().ends_with('\\') {
            break;
        }
    }

    let mut fields = Vec::new();
    for (idx, _) in body.match_indices('(') {
        let Some(close) = body[idx..].find(')') else {
            continue;
        };
        let args: Vec<&str> = body[idx + 1..idx + close]
            .split(',')
            .map(str::trim)
            .collect();
        // Two macro shapes exist. Pre-2025-02-25 (so v96, v97 and early v98) it is
        // the 4-arg `V(storageType, apiType, name, bits)`; after the BitField
        // rewrite it is the 5-arg `F/N(<storage-or-prev>, storageName, apiType,
        // Name, bits)`. In both the api type immediately precedes the field name,
        // which is the only thing this needs.
        let (api, name) = match args.len() {
            4 => (args[1], args[2]),
            5 => (args[2], args[3]),
            // The `#define FUNC_HEADER_FIELDS(F, N)` / `(V)` line itself.
            _ => continue,
        };
        let api_bytes = match api {
            "uint32_t" => 4,
            "uint16_t" => 2,
            "uint8_t" => 1,
            other => panic!("unhandled api type in FUNC_HEADER_FIELDS: {other}"),
        };
        fields.push(HeaderField {
            name: name.to_string(),
            api_bytes,
        });
    }
    assert!(
        fields.len() >= 4,
        "parsed only {} FUNC_HEADER_FIELDS entries; the macro shape probably changed",
        fields.len()
    );
    fields
}

/// `sizeof(SmallFuncHeader)` as upstream's own static_assert states it. Read the
/// assertion rather than recomputing the bitfield packing: it is upstream's own
/// claim, so it cannot drift from upstream.
fn upstream_small_header_size(root: &Path) -> Option<usize> {
    let src = strip_comments(&read(root, "include/hermes/BCGen/HBC/BytecodeFileFormat.h"));
    let marker = "sizeof(SmallFuncHeader) ==";
    // Not every vintage states the size. v96 asserts only divisibility --
    // `32 % sizeof(SmallFuncHeader) == 0` -- which contains this marker and whose
    // trailing literal is 0, so a naive search reads the size as zero. v97 makes no
    // claim at all. Require the marker to start the expression, and treat its
    // absence as "upstream does not say" rather than as a parse failure: this reads
    // upstream's own assertion precisely so it cannot drift, and inventing a number
    // where upstream states none would defeat that.
    for (at, _) in src.match_indices(marker) {
        let before = src[..at].trim_end();
        if before.ends_with('%') {
            continue;
        }
        let literal: String = src[at + marker.len()..]
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(n) = literal.parse::<usize>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    None
}

/// The load-bearing test: for every configured checkout, re-derive the modern
/// large-header size from upstream's macro and require `ModernLayout` to match.
///
/// The v99 defect was exactly this quantity being 37 where upstream said 36.
#[test]
fn modern_layout_matches_upstream_headers() {
    let mut checked = 0;
    for version in CHECKOUT_VERSIONS {
        let Some(root) = checkout_for(version) else {
            common::skip_or_fail(Oracle::Src, Some(version), &format!("no HERMES_SRC_V{version}"));
            continue;
        };
        let declared = upstream_version(&root);
        assert_eq!(
            declared, version,
            "HERMES_SRC_V{version} points at a checkout declaring BYTECODE_VERSION {declared}"
        );

        let fields = parse_func_header_fields(&root);
        // struct FunctionHeader is LLVM_PACKED and is the api-typed fields plus a
        // FunctionHeaderFlag byte, so its size is just their sum plus one.
        let derived_large: usize = fields.iter().map(|f| f.api_bytes).sum::<usize>() + 1;
        let has_num_cache_new_object = fields.iter().any(|f| f.name == "NumCacheNewObject");

        match ModernLayout::for_version(version) {
            Ok(layout) => {
                assert_eq!(
                    layout.large_size(),
                    derived_large,
                    "v{version}: ModernLayout says the large header is {} bytes, but \
                     FUNC_HEADER_FIELDS in {} derives {}. Upstream reshaped the header; \
                     update ModernLayout::for_version.",
                    layout.large_size(),
                    root.display(),
                    derived_large
                );
                assert_eq!(
                    layout.has_num_cache_new_object(),
                    has_num_cache_new_object,
                    "v{version}: NumCacheNewObject presence disagrees with upstream"
                );
                if let Some(upstream_small) = upstream_small_header_size(&root) {
                    assert_eq!(
                        hbc_decomp::modern_layout::MODERN_SMALL_HEADER_SIZE,
                        upstream_small,
                        "v{version}: small header size disagrees with upstream's static_assert"
                    );
                } else {
                    println!(
                        "  [note] v{version} states no sizeof(SmallFuncHeader); size unchecked"
                    );
                }
            }
            Err(_) => {
                // A refused version is fine, but only if it genuinely differs from
                // everything we support -- refusing a layout we could have handled
                // is a gap, not safety.
                let supported: Vec<usize> = [98, 99]
                    .iter()
                    .filter_map(|v| ModernLayout::for_version(*v).ok())
                    .map(|l| l.large_size())
                    .collect();
                assert!(
                    !supported.contains(&derived_large) || version < 97,
                    "v{version} is refused by ModernLayout, but its large header is \
                     {derived_large} bytes -- the same as a layout we already support. \
                     It should probably be in the allow-list."
                );
            }
        }
        checked += 1;
    }
    if checked == 0 {
        common::skip_or_fail(
            Oracle::Src,
            None,
            "no HERMES_SRC_V* checkouts configured; asserted nothing",
        );
    }
}

// ---------------------------------------------------------------------------
// The opcode table (what resources/bytecode/Bytecode*.json encodes)
// ---------------------------------------------------------------------------

/// Expand upstream's opcode list into name -> operand types.
///
/// `DEFINE_OPCODE_n(Name, T1, ..., Tn)` is literal. Jumps go through
/// `DEFINE_JUMP_n(Name)`, which upstream expands to a short `Addr8` form plus a
/// `Long` `Addr32` form; replicate that expansion rather than skipping them, or
/// the whole jump family goes unchecked.
fn parse_bytecode_list(root: &Path) -> BTreeMap<String, Vec<String>> {
    let src = strip_comments(&read(root, "include/hermes/BCGen/HBC/BytecodeList.def"));
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(open) = line.find('(') else { continue };
        let Some(close) = line.rfind(')') else {
            continue;
        };
        let head = &line[..open];
        let args: Vec<String> = line[open + 1..close]
            .split(',')
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect();

        // The file both *defines* these macros and *uses* them, and the
        // definitions contain lines like `DEFINE_OPCODE_1(name, Addr8)` inside
        // `#define DEFINE_JUMP_1(name)`. Real opcode names are CamelCase, so
        // require that and the macro parameters (`name`, `name##Long`) drop out.
        let is_opcode_name =
            |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !s.contains('#');

        if let Some(n) = head.strip_prefix("DEFINE_OPCODE_") {
            if n.parse::<usize>().is_err() || args.is_empty() || !is_opcode_name(&args[0]) {
                continue;
            }
            out.insert(args[0].clone(), args[1..].to_vec());
        } else if let Some(n) = head.strip_prefix("DEFINE_JUMP_") {
            let Ok(n) = n.parse::<usize>() else { continue };
            if args.len() != 1 || !is_opcode_name(&args[0]) {
                continue;
            }
            let name = &args[0];
            let extra = vec!["Reg8".to_string(); n.saturating_sub(1)];
            let mut short = vec!["Addr8".to_string()];
            short.extend(extra.clone());
            let mut long = vec!["Addr32".to_string()];
            long.extend(extra);
            out.insert(name.clone(), short);
            out.insert(format!("{name}Long"), long);
        }
    }
    assert!(
        out.len() > 100,
        "parsed only {} opcodes from BytecodeList.def; the macro shape probably changed",
        out.len()
    );
    out
}

/// This crate annotates string-id operands with a trailing `S` (`UInt16S`), which
/// is a semantic marker of identical width, not a different encoding. Normalise it
/// away before comparing, or every string-bearing opcode reads as a mismatch.
fn strip_string_marker(ty: &str) -> &str {
    match ty {
        "UInt8S" => "UInt8",
        "UInt16S" => "UInt16",
        "UInt32S" => "UInt32",
        other => other,
    }
}

/// The other half of R19: the bundled opcode table must describe the same
/// instructions as the checkout.
///
/// This is what would have caught `NewFastArray` gaining a `Reg8` operand
/// upstream — a 4-byte instruction becoming 5, which desynchronises decoding for
/// the rest of any function body containing one.
#[test]
fn opcode_tables_match_upstream() {
    let mut checked = 0;
    for version in CHECKOUT_VERSIONS {
        let Some(root) = checkout_for(version) else {
            common::skip_or_fail(Oracle::Src, Some(version), &format!("no HERMES_SRC_V{version}"));
            continue;
        };
        let upstream = parse_bytecode_list(&root);
        let ours = BytecodeFormat::for_version(version)
            .unwrap_or_else(|e| panic!("bundled opcode table for v{version}: {e}"));
        let ours: BTreeMap<String, Vec<String>> = ours
            .definitions
            .iter()
            .map(|d| {
                (
                    d.name.clone(),
                    d.operand_types
                        .iter()
                        .map(|t| strip_string_marker(&format!("{t:?}")).to_string())
                        .collect(),
                )
            })
            .collect();

        let mut problems = Vec::new();
        for (name, up_types) in &upstream {
            match ours.get(name) {
                None => problems.push(format!("  {name}: in upstream, missing from our table")),
                Some(our_types) => {
                    let normalised: Vec<&str> =
                        our_types.iter().map(|t| strip_string_marker(t)).collect();
                    let up: Vec<&str> = up_types.iter().map(String::as_str).collect();
                    if normalised != up {
                        problems.push(format!(
                            "  {name}: upstream {up:?}, ours {normalised:?}\
                             {}",
                            if normalised.len() != up.len() {
                                "   <-- OPERAND COUNT DIFFERS: instruction length changed, \
                                 decoding will desynchronise"
                            } else {
                                ""
                            }
                        ));
                    }
                }
            }
        }
        for name in ours.keys() {
            if !upstream.contains_key(name) {
                problems.push(format!("  {name}: in our table, missing from upstream"));
            }
        }

        assert!(
            problems.is_empty(),
            "v{version} opcode table disagrees with {} ({} problems):\n{}\n\
             Regenerate resources/bytecode/Bytecode{version}.json from this checkout.",
            root.display(),
            problems.len(),
            problems.join("\n")
        );
        checked += 1;
    }
    if checked == 0 {
        common::skip_or_fail(
            Oracle::Src,
            None,
            "no HERMES_SRC_V* checkouts configured; asserted nothing",
        );
    }
}
