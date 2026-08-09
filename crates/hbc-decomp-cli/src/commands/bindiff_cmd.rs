use crate::cli_args::{FunctionLayoutArg, LayoutArg};
use crate::tui::diff::{compare_functions, strip_offsets, DiffMode, DiffStatus};
use crate::tui::disasm_or_log;
use hbc_decomp::{decompile_function_v2, BytecodeFile, BytecodeFormat, DecompileOptionsV2};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub fn run_bindiff(
    path1: &PathBuf,
    path2: &PathBuf,
    layout: LayoutArg,
    function_layout: FunctionLayoutArg,
    format_version: Option<u32>,
    diff_code: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading {}...", path1.display());
    let file1 = crate::helpers::load_file(path1, layout, function_layout)?;
    let format1 = crate::helpers::load_format(&file1, format_version)?;

    println!("Loading {}...", path2.display());
    let file2 = crate::helpers::load_file(path2, layout, function_layout)?;
    let format2 = crate::helpers::load_format(&file2, format_version)?;

    println!("Comparing functions...");

    // Name -> every FunctionID carrying that name, ascending
    let groups1 = build_function_groups(&file1);
    let groups2 = build_function_groups(&file2);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let mut identical = 0usize;
    let mut compared = 0usize;

    let mode = if diff_code {
        DiffMode::Code
    } else {
        DiffMode::Assembly
    };

    for (name, ids1) in &groups1 {
        let empty = Vec::new();
        let ids2 = groups2.get(name).unwrap_or(&empty);
        let (pairs, only1, only2) = pair_group(&file1, &format1, ids1, &file2, &format2, ids2);

        for (id1, id2) in pairs {
            let status = compare_functions(&file1, &format1, id1, &file2, &format2, id2, mode);
            compared += 1;
            if status != DiffStatus::Identical {
                modified.push((display_name(name, id1), id1, id2));
            } else {
                identical += 1;
            }
        }
        for id in only1 {
            removed.push((display_name(name, id), id));
        }
        for id in only2 {
            added.push((display_name(name, id), id));
        }
    }

    // A name present only on the right is entirely new.
    for (name, ids2) in &groups2 {
        if !groups1.contains_key(name) {
            for &id in ids2 {
                added.push((display_name(name, id), id));
            }
        }
    }

    // HashMap iteration order is arbitrary; sort so runs are reproducible.
    modified.sort_by_key(|&(_, id1, _)| id1);
    removed.sort_by_key(|&(_, id)| id);
    added.sort_by_key(|&(_, id)| id);

    println!("\n--- BinDiff Result ---");
    println!(
        "Compared:  {compared} pairs ({} functions in base, {} in new)",
        file1.function_headers.len(),
        file2.function_headers.len()
    );
    println!("Identical: {identical}");
    println!("Modified:  {}", modified.len());
    println!("Removed:   {}", removed.len());
    println!("Added:     {}", added.len());

    if !modified.is_empty() {
        println!("\nModified Functions:");
        for (name, id1, id2) in &modified {
            println!("  - {name} (ID: {id1} -> {id2})");

            if diff_code {
                println!("\n    --- LEFT (v1) ---");
                let code1 =
                    decompile_function_v2(&file1, &format1, *id1, &DecompileOptionsV2::default())
                        .unwrap_or_else(|e| format!("Error: {e}"));
                for line in code1.lines() {
                    println!("    {line}");
                }

                println!("\n    --- RIGHT (v2) ---");
                let code2 =
                    decompile_function_v2(&file2, &format2, *id2, &DecompileOptionsV2::default())
                        .unwrap_or_else(|e| format!("Error: {e}"));
                for line in code2.lines() {
                    println!("    {line}");
                }
                println!("\n    ------------------");
            }
        }
    }

    Ok(())
}

// Group every function id under its name.
//
// This used to be a `HashMap<String, u32>`, which silently dropped most of a
// real bundle: an anonymous function carries the *empty* name rather than a
// missing one, so the `f{i}` fallback never fired and all of them collapsed
// onto the single `""` key, as did every set of same-named functions. On a
// 62,526-function bundle that left 17,262 pairs actually compared and 45,264
// functions never looked at — with nothing in the output saying so, which is
// the worst way to miss a patched function.
fn build_function_groups(file: &BytecodeFile) -> HashMap<String, Vec<u32>> {
    let mut map: HashMap<String, Vec<u32>> = HashMap::new();
    for (i, header) in file.function_headers.iter().enumerate() {
        let name = file
            .string_at(header.function_name())
            .map(|e| e.value.clone())
            .unwrap_or_default();
        map.entry(name).or_default().push(i as u32);
    }
    map
}

// Identity key for pairing: the disassembly with offsets stripped, hashed. Two
// functions with the same key are byte-identical code wherever they sit.
fn body_key(file: &BytecodeFile, format: &BytecodeFormat, id: u32) -> u64 {
    let mut h = DefaultHasher::new();
    strip_offsets(&disasm_or_log(file, format, id)).hash(&mut h);
    h.finish()
}

// Pair up one name group across the two bundles: (pairs, only-in-1, only-in-2).
//
// Content first, position second. Pairing purely by position is exact for two
// builds of the *same* bundle, where ids line up -- but across a version bump a
// single inserted function shifts every later ordinal, and since ~28k functions
// in this bundle share the one empty name, that mis-pairs the whole tail and
// reports tens of thousands of spurious modifications. So identical bodies claim
// each other first, in order, and only what is left over is matched positionally.
//
// A hash collision is harmless: it just yields a pair that `compare_functions`
// then reports as Modified, exactly as the positional fallback would have.
fn pair_group(
    file1: &BytecodeFile,
    format1: &BytecodeFormat,
    ids1: &[u32],
    file2: &BytecodeFile,
    format2: &BytecodeFormat,
    ids2: &[u32],
) -> (Vec<(u32, u32)>, Vec<u32>, Vec<u32>) {
    // Single-element groups are the overwhelmingly common case (a real name);
    // skip the hashing entirely.
    if ids1.len() == 1 && ids2.len() == 1 {
        return (vec![(ids1[0], ids2[0])], Vec::new(), Vec::new());
    }

    let mut available: HashMap<u64, VecDeque<u32>> = HashMap::new();
    for &id in ids2 {
        available
            .entry(body_key(file2, format2, id))
            .or_default()
            .push_back(id);
    }

    let mut pairs = Vec::new();
    let mut left1 = Vec::new();
    let mut claimed = HashSet::new();
    for &id1 in ids1 {
        match available
            .get_mut(&body_key(file1, format1, id1))
            .and_then(VecDeque::pop_front)
        {
            Some(id2) => {
                claimed.insert(id2);
                pairs.push((id1, id2));
            }
            None => left1.push(id1),
        }
    }

    // Whatever nobody claimed, in id order, paired positionally with the rest.
    let mut left2: Vec<u32> = ids2.iter().copied().filter(|i| !claimed.contains(i)).collect();
    let n = left1.len().min(left2.len());
    for k in 0..n {
        pairs.push((left1[k], left2[k]));
    }
    (pairs, left1.split_off(n), left2.split_off(n))
}

// Anonymous functions all share the empty name, so it can't identify one on its
// own; fall back to the id.
fn display_name(name: &str, id: u32) -> String {
    if name.is_empty() {
        format!("fn#{id} (anonymous)")
    } else {
        name.to_string()
    }
}

// are_functions_identical and strip_offsets removed (using shared module)
