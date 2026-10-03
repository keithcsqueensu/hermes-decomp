use crate::cli_args::FormatArgs;
use crate::tui::diff::{compare_functions, strip_offsets, DiffMode, DiffStatus};
use crate::tui::disasm_or_log;
use hbc_decomp::{decompile_function_v2, BytecodeFile, BytecodeFormat, DecompileOptionsV2};
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

#[derive(Serialize, Debug, Clone)]
pub struct ModifiedFunction {
    pub name: String,
    pub base_id: u32,
    pub new_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_code: Option<String>,
}

#[derive(Serialize, Debug, Clone)]
pub struct DiffReport {
    pub base: String,
    pub new: String,
    pub base_functions: usize,
    pub new_functions: usize,
    // Function pairs actually compared. Every function is either in a pair or
    // listed as removed/added, so a shortfall here would be a silent skip.
    pub compared: usize,
    pub identical: usize,
    pub modified: Vec<ModifiedFunction>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

impl DiffReport {
    // The report with every list in name order, so two runs over the same
    // bundles produce the same document.
    pub fn sorted(mut self) -> Self {
        self.modified.sort_by(|a, b| a.name.cmp(&b.name));
        self.removed.sort();
        self.added.sort();
        self
    }
}

pub fn run_bindiff(
    path1: &PathBuf,
    path2: &PathBuf,
    args: &FormatArgs,
    diff_code: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !json {
        println!("Loading {}...", path1.display());
    }
    let file1 = crate::helpers::load_file(path1, args)?;
    let format1 = crate::helpers::load_format(&file1, args.format_version)?;

    if !json {
        println!("Loading {}...", path2.display());
    }
    let file2 = crate::helpers::load_file(path2, args)?;
    let format2 = crate::helpers::load_format(&file2, args.format_version)?;

    if !json {
        println!("Comparing functions...");
    }

    let report = compare_bundles(
        &file1,
        &format1,
        &file2,
        &format2,
        diff_code,
        path1.display().to_string(),
        path2.display().to_string(),
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&report.sorted())?);
        return Ok(());
    }
    print_report(&report, diff_code);
    Ok(())
}

fn compare_bundles(
    file1: &BytecodeFile,
    format1: &BytecodeFormat,
    file2: &BytecodeFile,
    format2: &BytecodeFormat,
    diff_code: bool,
    base: String,
    new: String,
) -> DiffReport {
    // Name -> every FunctionID carrying that name, ascending
    let groups1 = build_function_groups(file1);
    let groups2 = build_function_groups(file2);

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
        let (pairs, only1, only2) = pair_group(file1, format1, ids1, file2, format2, ids2);

        for (id1, id2) in pairs {
            let status = compare_functions(file1, format1, id1, file2, format2, id2, mode);
            compared += 1;
            if status != DiffStatus::Identical {
                let (base_code, new_code) = if diff_code {
                    (
                        Some(decompile_or_error(file1, format1, id1)),
                        Some(decompile_or_error(file2, format2, id2)),
                    )
                } else {
                    (None, None)
                };
                modified.push(ModifiedFunction {
                    name: display_name(name, id1),
                    base_id: id1,
                    new_id: id2,
                    base_code,
                    new_code,
                });
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
    modified.sort_by_key(|m| m.base_id);
    removed.sort_by_key(|&(_, id)| id);
    added.sort_by_key(|&(_, id)| id);

    DiffReport {
        base,
        new,
        base_functions: file1.function_headers.len(),
        new_functions: file2.function_headers.len(),
        compared,
        identical,
        modified,
        removed: removed.into_iter().map(|(n, _)| n).collect(),
        added: added.into_iter().map(|(n, _)| n).collect(),
    }
}

fn decompile_or_error(file: &BytecodeFile, format: &BytecodeFormat, id: u32) -> String {
    decompile_function_v2(file, format, id, &DecompileOptionsV2::default())
        .unwrap_or_else(|e| format!("Error: {e}"))
}

fn print_report(report: &DiffReport, diff_code: bool) {
    println!("\n--- BinDiff Result ---");
    println!(
        "Compared:  {} pairs ({} functions in base, {} in new)",
        report.compared, report.base_functions, report.new_functions
    );
    println!("Identical: {}", report.identical);
    println!("Modified:  {}", report.modified.len());
    println!("Removed:   {}", report.removed.len());
    println!("Added:     {}", report.added.len());

    if !report.modified.is_empty() {
        println!("\nModified Functions:");
        for m in &report.modified {
            println!("  - {} (ID: {} -> {})", m.name, m.base_id, m.new_id);

            if diff_code {
                println!("\n    --- LEFT (v1) ---");
                for line in m.base_code.as_deref().unwrap_or_default().lines() {
                    println!("    {line}");
                }

                println!("\n    --- RIGHT (v2) ---");
                for line in m.new_code.as_deref().unwrap_or_default().lines() {
                    println!("    {line}");
                }
                println!("\n    ------------------");
            }
        }
    }
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
    let mut left2: Vec<u32> = ids2
        .iter()
        .copied()
        .filter(|i| !claimed.contains(i))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn diff_report_json_is_sorted_and_omits_absent_code() {
        let report = DiffReport {
            base: "a.hbc".into(),
            new: "b.hbc".into(),
            base_functions: 9,
            new_functions: 9,
            compared: 5,
            identical: 3,
            modified: vec![
                ModifiedFunction {
                    name: "zeta".into(),
                    base_id: 5,
                    new_id: 6,
                    base_code: None,
                    new_code: None,
                },
                ModifiedFunction {
                    name: "alpha".into(),
                    base_id: 1,
                    new_id: 2,
                    base_code: Some("function alpha() {}".into()),
                    new_code: Some("function alpha(x) {}".into()),
                },
            ],
            removed: vec!["old2".into(), "old1".into()],
            added: vec!["new2".into(), "new1".into()],
        }
        .sorted();
        let back: Value = serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(back["identical"], 3);
        assert_eq!(back["modified"][0]["name"], "alpha");
        assert_eq!(back["modified"][0]["new_code"], "function alpha(x) {}");
        assert!(back["modified"][1].get("base_code").is_none());
        assert_eq!(back["removed"], serde_json::json!(["old1", "old2"]));
        assert_eq!(back["added"], serde_json::json!(["new1", "new2"]));
    }
}
