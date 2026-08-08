use crate::cli_args::{FunctionLayoutArg, LayoutArg};
use crate::tui::diff::{compare_functions, DiffMode, DiffStatus};
use hbc_decomp::{decompile_function_v2, BytecodeFile, DecompileOptionsV2};
use std::collections::HashMap;
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

    // Pair positionally within each name group. Both sides are in ascending id
    // order, so for two builds of the same bundle this pairs like with like;
    // whatever a group has left over is an add or a remove.
    for (name, ids1) in &groups1 {
        let ids2 = groups2.get(name).map(Vec::as_slice).unwrap_or(&[]);
        let paired = ids1.len().min(ids2.len());

        for k in 0..paired {
            let (id1, id2) = (ids1[k], ids2[k]);
            let status = compare_functions(&file1, &format1, id1, &file2, &format2, id2, mode);
            compared += 1;
            if status != DiffStatus::Identical {
                modified.push((display_name(name, id1), id1, id2));
            } else {
                identical += 1;
            }
        }
        for &id in &ids1[paired..] {
            removed.push((display_name(name, id), id));
        }
    }

    for (name, ids2) in &groups2 {
        let len1 = groups1.get(name).map_or(0, Vec::len);
        for &id in ids2.iter().skip(len1) {
            added.push((display_name(name, id), id));
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
