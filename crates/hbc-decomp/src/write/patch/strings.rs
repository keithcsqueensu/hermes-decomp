// Patch string table entries: same-length in place, or grow/shrink with a full
// string-table + storage rebuild and tail relocation (hermes_rs issue #10 class).

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use std::collections::BTreeSet;

use crate::write::reloc::{relocate, Reloc};
use crate::write::serialize::{commit_image_checked, section_offset, HeaderEdit};

use super::string_storage::{
    current_strings, lay_out, read_all_string_locs, validate, Packing, StoredString, StringLayout,
};
use super::PatchOptions;

// Hermes identifier hash: Jenkins one at a time over UTF-16 code units, seeded
// with 0. This matches hermes::hashString feeding hermes::updateJenkinsHash.
pub(super) fn hermes_identifier_hash(s: &str) -> u32 {
    let mut h: u32 = 0;
    for cu in s.encode_utf16() {
        h = h.wrapping_add(cu as u32);
        h = h.wrapping_add(h << 10);
        h ^= h >> 6;
    }
    h
}

// Index of string `id` within the identifier hash table (identifiers appear in
// string id order). Returns None when the string is not an identifier.
fn identifier_index(file: &BytecodeFile, id: u32) -> Option<usize> {
    if !file.strings.get(id as usize)?.is_identifier {
        return None;
    }
    Some(
        (0..id as usize)
            .filter(|&i| file.strings[i].is_identifier)
            .count(),
    )
}

// If string `id` is an identifier, rewrite its precomputed hash for `new_value`
// in the identifier_hashes table of `buf`. The table sits before the string
// region, so its position is the same in an in place patch or a rebuilt image.
fn update_identifier_hash(
    file: &BytecodeFile,
    buf: &mut [u8],
    id: u32,
    new_value: &str,
) -> Result<()> {
    let Some(idx) = identifier_index(file, id) else {
        return Ok(());
    };
    let ih_off = section_offset(file, "identifier_hashes")
        .ok_or_else(|| Error::Write("identifier_hashes section missing".into()))?
        as usize;
    let pos = ih_off + idx * 4;
    if pos + 4 > buf.len() {
        return Err(Error::Write("identifier hash slot out of range".into()));
    }
    buf[pos..pos + 4].copy_from_slice(&hermes_identifier_hash(new_value).to_le_bytes());
    Ok(())
}

// Retarget string `from_id` to resolve to the same value as `to_id` by copying
// the 4-byte SmallStringTableEntry. Metadata-only: no table rebuild, no storage
// growth, no code change — just 4 bytes + optional identifier hash + SHA-1 footer.
//
// This is the "string-table retarget" technique (e.g. `H:mm` → `HH:mm`): every
// instruction that references `from_id` now gets the value of `to_id`, globally,
// without touching any function body.
pub fn retarget_string(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    from_id: u32,
    to_id: u32,
    _opts: &PatchOptions,
) -> Result<Vec<u8>> {
    let n = file.header.string_count;
    if from_id >= n {
        return Err(Error::Write(format!(
            "retarget: from_id {from_id} out of range (string_count={n})"
        )));
    }
    if to_id >= n {
        return Err(Error::Write(format!(
            "retarget: to_id {to_id} out of range (string_count={n})"
        )));
    }
    if from_id == to_id {
        return Err(Error::Write("retarget: from_id == to_id".into()));
    }

    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;

    // Overflowed entries need nothing special. An overflowed small entry holds
    // the index of its overflow record, which is how the engine finds it, so a
    // copied entry shares to_id's record; an overflowed from_id leaves its own
    // record behind, unreferenced and harmless.
    let from_slot = small_off + from_id as usize * 4;
    let to_slot = small_off + to_id as usize * 4;
    if from_slot + 4 > raw.len() || to_slot + 4 > raw.len() {
        return Err(Error::Write("string table slot out of range".into()));
    }

    // A cross-kind retarget (from_id and to_id differ in is_identifier) is
    // allowed but noteworthy; the CLI warns. The library stays silent so
    // programmatic callers get no unsolicited stderr output.
    let from_is_id = file.strings[from_id as usize].is_identifier;

    // What every entry must read back as afterwards: unchanged, except that
    // from_id now reads as to_id.
    let mut expected = current_strings(file)?;
    expected[from_id as usize] = expected[to_id as usize].clone();

    // Copy the 4-byte entry.
    let entry_bytes: [u8; 4] = raw[to_slot..to_slot + 4].try_into().unwrap();
    raw[from_slot..from_slot + 4].copy_from_slice(&entry_bytes);

    // If from_id is an identifier, update its hash to match to_id's value.
    if from_is_id {
        let to_value = &file.strings[to_id as usize].value;
        update_identifier_hash(file, &mut raw, from_id, to_value)?;
    }

    commit_image_checked(file, raw, HeaderEdit::default(), |after| {
        validate(after, &expected, &BTreeSet::new())
    })
}

// Grow, shrink or re-encode one string entry. Rebuilds the small and overflow
// tables and the storage (`string_storage::lay_out`, packed unless `packing`
// says otherwise, with `id` pinned to bytes of its own), then moves everything
// after the string region by the size delta (`write::reloc::relocate`). Works on
// legacy and modern layouts, overflowed entries and UTF-16 alike; every entry is
// checked against its intended text before the result is committed (P0).
fn patch_string_resize(
    file: &mut BytecodeFile,
    id: u32,
    new_value: &str,
    packing: Packing,
) -> Result<Vec<u8>> {
    if file.strings.get(id as usize).is_none() {
        return Err(Error::Write(format!("string id {id} out of range")));
    }
    let raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    // Section right after the string region: array_buffer on legacy,
    // literal_value_buffer on modern. Everything from here shifts.
    let array_off = section_offset(file, "array_buffer")
        .or_else(|| section_offset(file, "literal_value_buffer"))
        .ok_or_else(|| Error::Write("post-string section missing".into()))?
        as usize;
    if array_off < small_off || small_off > storage_off {
        return Err(Error::Write("unexpected string section order".into()));
    }

    let mut strings = current_strings(file)?;
    strings[id as usize] = StoredString::encode(new_value);
    let pinned = BTreeSet::from([id as usize]);
    let layout = lay_out(&strings, &pinned, packing)?;

    // The new string region: small table, overflow table, storage padded so the
    // following section keeps its 4-byte alignment.
    let mut region: Vec<u8> = Vec::new();
    push_tables_and_storage(&mut region, &layout);
    let delta = region.len() as i64 - (array_off - small_off) as i64;

    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..small_off]);
    rebuilt.extend_from_slice(&region);
    rebuilt.extend_from_slice(&raw[array_off..]);

    // overflow_string_count (56) and string_storage_size (60) share offsets
    // across layouts.
    rebuilt[56..60].copy_from_slice(&(layout.overflow.len() as u32).to_le_bytes());
    rebuilt[60..64].copy_from_slice(&(layout.storage.len() as u32).to_le_bytes());
    // Everything after the string region moves by `delta`. Exception-handler
    // tables are body-relative and are NOT rewritten: a whole-region shift moves
    // the table and the code it points into together.
    relocate(
        file,
        &mut rebuilt,
        &Reloc {
            moved_from: array_off,
            delta,
            resized: None,
        },
    )?;

    // An identifier's precomputed hash depends on its text, so refresh it.
    update_identifier_hash(file, &mut rebuilt, id, new_value)?;

    let edit = HeaderEdit {
        string_storage: Some((layout.overflow.len() as u32, layout.storage.len() as u32)),
        ..HeaderEdit::shifted(delta)
    };
    commit_image_checked(file, rebuilt, edit, |after| {
        validate(after, &strings, &pinned)
    })
}

// Small table, overflow table and storage, in file order, padded to 4.
fn push_tables_and_storage(region: &mut Vec<u8>, layout: &StringLayout) {
    for e in &layout.small {
        region.extend_from_slice(&e.to_le_bytes());
    }
    for (off, len) in &layout.overflow {
        region.extend_from_slice(&off.to_le_bytes());
        region.extend_from_slice(&len.to_le_bytes());
    }
    region.extend_from_slice(&layout.storage);
    while !region.len().is_multiple_of(4) {
        region.push(0);
    }
}

// Append a new string to the string table. Rebuilds the entire string region
// (kinds, identifier hashes, small table, overflow table, storage) and
// relocates the tail. The new string gets id = old `string_count`; every
// existing id is stable, and the new entry is pinned to bytes of its own.
// Returns `(raw_image, new_id)`.
//
// `is_identifier`: true for property/symbol names (adds a Jenkins hash slot);
// false for plain string literals.
pub fn add_string(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    value: &str,
    is_identifier: bool,
    opts: &PatchOptions,
) -> Result<(Vec<u8>, u32)> {
    let new_id = file.header.string_count;
    let raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    // Derive section boundaries. The rebuild uses kinds_off as the splice origin
    // and array_off as the splice end; the middle two are checked for presence.
    let kinds_off = section_offset(file, "string_kinds")
        .ok_or_else(|| Error::Write("string_kinds section missing".into()))?
        as usize;
    section_offset(file, "identifier_hashes")
        .ok_or_else(|| Error::Write("identifier_hashes section missing".into()))?;
    section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?;
    let array_off = section_offset(file, "array_buffer")
        .or_else(|| section_offset(file, "literal_value_buffer"))
        .ok_or_else(|| Error::Write("post-string section missing".into()))?
        as usize;
    if array_off < kinds_off {
        return Err(Error::Write("unexpected string section order".into()));
    }

    // A duplicate value is allowed (ids are append-only, so it still gets a new
    // id); the CLI notes it. The library stays silent so programmatic callers
    // get no unsolicited stderr output.
    let mut strings = current_strings(file)?;
    strings.push(StoredString::encode(value));
    let pinned = BTreeSet::from([new_id as usize]);
    let layout = lay_out(&strings, &pinned, opts.string_packing())?;

    // ---- Rebuild string_kinds ----
    use crate::file::{StringKindEntry, StringKindType};

    let new_kind = if is_identifier {
        StringKindType::Identifier
    } else {
        StringKindType::String
    };
    let mut new_kinds = file.string_kinds.clone();
    match new_kinds.last_mut() {
        Some(last) if last.kind == new_kind => last.count += 1,
        _ => new_kinds.push(StringKindEntry {
            kind: new_kind,
            count: 1,
        }),
    }

    // ---- Rebuild identifier_hashes ----
    let mut new_id_hashes = file.identifier_hashes.clone();
    if is_identifier {
        new_id_hashes.push(hermes_identifier_hash(value));
    }

    // ---- Assemble the new region block ----
    // Order: string_kinds ++ identifier_hashes ++ small_string_table ++
    // overflow_string_table ++ string_storage.
    let mut region: Vec<u8> = Vec::new();
    // String kinds (each entry is u32: high bit = kind, low 31 = count).
    for k in &new_kinds {
        let raw_k = match k.kind {
            StringKindType::String => k.count,
            StringKindType::Identifier => k.count | (1u32 << 31),
        };
        region.extend_from_slice(&raw_k.to_le_bytes());
    }
    for h in &new_id_hashes {
        region.extend_from_slice(&h.to_le_bytes());
    }
    push_tables_and_storage(&mut region, &layout);
    let delta = region.len() as i64 - (array_off - kinds_off) as i64;

    // ---- Splice: [..kinds_off] + region + [array_off..] ----
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..kinds_off]);
    rebuilt.extend_from_slice(&region);
    rebuilt.extend_from_slice(&raw[array_off..]);

    // ---- Update header counts ----
    // string_kind_count at bytes 44..48, identifier_count at 48..52, string_count
    // at 52..56, overflow_string_count at 56..60, string_storage_size at 60..64.
    rebuilt[44..48].copy_from_slice(&(new_kinds.len() as u32).to_le_bytes());
    rebuilt[48..52].copy_from_slice(&(new_id_hashes.len() as u32).to_le_bytes());
    rebuilt[52..56].copy_from_slice(&(new_id + 1).to_le_bytes());
    rebuilt[56..60].copy_from_slice(&(layout.overflow.len() as u32).to_le_bytes());
    rebuilt[60..64].copy_from_slice(&(layout.storage.len() as u32).to_le_bytes());

    // ---- Shift downstream offsets by delta ----
    relocate(
        file,
        &mut rebuilt,
        &Reloc {
            moved_from: array_off,
            delta,
            resized: None,
        },
    )?;

    let edit = HeaderEdit {
        string_counts: Some((
            new_kinds.len() as u32,
            new_id_hashes.len() as u32,
            new_id + 1,
        )),
        string_storage: Some((layout.overflow.len() as u32, layout.storage.len() as u32)),
        ..HeaderEdit::shifted(delta)
    };
    let out = commit_image_checked(file, rebuilt, edit, |after| {
        validate(after, &strings, &pinned)
    })?;
    Ok((out, new_id))
}

// Replace the value of string table entry `id`. Same-length ASCII edits patch
// storage in place; anything else rebuilds the string tables and relocates the
// tail. Hermes packs strings so ranges can overlap (`done` inside `undone`), so
// an entry sharing any byte with another is never overwritten in place: it is
// rebuilt instead, which pins it to bytes of its own.
pub fn patch_string_by_id(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    id: u32,
    new_value: &str,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let packing = options.string_packing();
    let Some(entry) = file.strings.get(id as usize) else {
        return Err(Error::Write(format!("string id {id} out of range")));
    };
    // A UTF-16 entry, or a value that needs UTF-16, is re-encoded: rebuild.
    if entry.is_utf16 || !new_value.is_ascii() {
        return patch_string_resize(file, id, new_value, packing);
    }
    // Where every entry's bytes are, read from the tables in one pass; Hermes may
    // share storage between entries, so this is never a substring search.
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    let locs = read_all_string_locs(file)?;
    let (abs_off, old_len) = (
        storage_off + locs[id as usize].storage_off as usize,
        locs[id as usize].len_field as usize,
    );
    if new_value.len() != old_len {
        return patch_string_resize(file, id, new_value, packing);
    }
    // Overlap guard: any other entry whose bytes intersect ours.
    let shared = locs.iter().enumerate().any(|(other, loc)| {
        let start = storage_off + loc.storage_off as usize;
        let end = start + loc.len_field as usize * if loc.is_utf16 { 2 } else { 1 };
        other != id as usize && start < end && abs_off < end && start < abs_off + old_len
    });
    if shared {
        return patch_string_resize(file, id, new_value, packing);
    }

    let mut expected = current_strings(file)?;
    expected[id as usize] = StoredString::encode(new_value);
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    raw[abs_off..abs_off + old_len].copy_from_slice(new_value.as_bytes());
    // An identifier's precomputed hash tracks its text, so refresh it here too.
    update_identifier_hash(file, &mut raw, id, new_value)?;
    commit_image_checked(file, raw, HeaderEdit::default(), |after| {
        validate(after, &expected, &BTreeSet::new())
    })
}

// Replace the first string table entry whose value equals `old` with `new`.
pub fn patch_string_replace(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    old: &str,
    new: &str,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let id = file
        .strings
        .iter()
        .position(|s| s.value == old)
        .ok_or_else(|| Error::Write(format!("string not found: {old:?}")))? as u32;
    patch_string_by_id(file, format, id, new, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::footer::verify_footer;

    fn load(path: &str) -> (BytecodeFile, BytecodeFormat) {
        let bytes = std::fs::read(path).unwrap();
        let file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        (file, format)
    }

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
    );

    #[test]
    fn patch_string_same_length_v96() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let candidates: Vec<u32> = file
            .strings
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_utf16 && s.value.len() >= 3)
            .map(|(i, _)| i as u32)
            .collect();
        let mut patched = false;
        for id in candidates {
            let old = file.strings[id as usize].value.clone();
            let new = "Z".repeat(old.len());
            match patch_string_by_id(&mut file, &format, id, &new, &PatchOptions::default()) {
                Ok(out) => {
                    assert!(verify_footer(&out));
                    let re = BytecodeFile::parse_auto(&out).unwrap();
                    assert_eq!(re.strings[id as usize].value, new);
                    patched = true;
                    break;
                }
                Err(e) if e.to_string().contains("overlaps") => continue,
                Err(e) => panic!("unexpected: {e}"),
            }
        }
        assert!(patched, "expected at least one non-overlapping string");
    }

    #[test]
    fn patch_string_resize_grow_reparses() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "gen" is a plain (non-identifier) string in this fixture.
        let id = file.strings.iter().position(|s| s.value == "gen");
        let Some(id) = id else { return };
        let out = patch_string_by_id(
            &mut file,
            &format,
            id as u32,
            "genXXXXX",
            &PatchOptions::default(),
        );
        if let Ok(out) = out {
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(re.strings[id].value, "genXXXXX");
        }
    }

    #[test]
    fn patch_string_packed_falls_back_to_resize() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "done" shares storage with "next" here, so an in place patch would
        // overlap. The patch must still succeed by rebuilding the table unpacked.
        if file.strings.get(5).map(|s| s.value.as_str()) == Some("done") {
            let out = patch_string_by_id(&mut file, &format, 5, "GONE", &PatchOptions::default())
                .expect("packed same length patch should resize, not fail");
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(re.strings[5].value, "GONE");
        }
    }

    #[test]
    fn identifier_hash_matches_hermes() {
        // Values checked against a real hermesc-compiled table.
        assert_eq!(hermes_identifier_hash("foo"), 0x9290_584e);
        assert_eq!(hermes_identifier_hash("print"), 0xa689_f65b);
    }

    // Patching an ASCII string to a value with non-ASCII characters must switch it
    // to UTF-16 so the runtime reads the real characters, not the UTF-8 bytes. This
    // guards the encoding-by-content rule (a real v98 VM confirmed the round trip).
    #[test]
    fn patch_ascii_to_non_ascii_becomes_utf16() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        // Latin1-range only characters still require UTF-16 (they are not ASCII).
        let out = patch_string_by_id(
            &mut file,
            &format,
            id as u32,
            "éàü",
            &PatchOptions::default(),
        )
        .expect("patch to non-ascii");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(
            re.strings[id].is_utf16,
            "non-ascii value must be stored UTF-16"
        );
        assert_eq!(re.strings[id].value, "éàü");

        // A character above the basic plane also round trips.
        let (mut file2, format2) = load(FIXTURE);
        let out2 = patch_string_by_id(
            &mut file2,
            &format2,
            id as u32,
            "a€☕",
            &PatchOptions::default(),
        )
        .expect("patch to astral");
        let re2 = BytecodeFile::parse_auto(&out2).unwrap();
        assert!(re2.strings[id].is_utf16);
        assert_eq!(re2.strings[id].value, "a€☕");
    }

    // ---- retarget_string tests ----

    #[test]
    fn retarget_string_basic() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // Find two distinct non-empty strings to retarget.
        let a = file
            .strings
            .iter()
            .position(|s| !s.value.is_empty() && !s.is_utf16);
        let b = file
            .strings
            .iter()
            .rposition(|s| !s.value.is_empty() && !s.is_utf16);
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) if a != b => (a as u32, b as u32),
            _ => return,
        };
        let target_val = file.strings[b as usize].value.clone();
        let opts = PatchOptions::default();
        let out = retarget_string(&mut file, &format, a, b, &opts).expect("retarget_string basic");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.strings[a as usize].value, target_val);
    }

    #[test]
    fn retarget_string_file_size_unchanged() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let original_len = file.raw_bytes.as_ref().unwrap().len();
        // Pick any two distinct strings.
        let (a, b) = (0u32, 1u32);
        if file.header.string_count < 2 {
            return;
        }
        let opts = PatchOptions::default();
        let out =
            retarget_string(&mut file, &format, a, b, &opts).expect("retarget_string size check");
        // File size must not change — metadata-only edit.
        assert_eq!(out.len(), original_len, "file size changed after retarget");
    }

    #[test]
    fn retarget_string_other_strings_unchanged() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        if file.header.string_count < 3 {
            return;
        }
        let originals: Vec<(String, bool)> = file
            .strings
            .iter()
            .map(|s| (s.value.clone(), s.is_utf16))
            .collect();
        let opts = PatchOptions::default();
        let _ =
            retarget_string(&mut file, &format, 0, 1, &opts).expect("retarget for unchanged check");
        let re = BytecodeFile::parse_auto(file.raw_bytes.as_ref().unwrap()).unwrap();
        for (i, (value, _)) in originals.iter().enumerate().skip(2) {
            assert_eq!(&re.strings[i].value, value, "string {i} changed");
        }
    }

    #[test]
    fn retarget_string_same_id_rejected() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        let result = retarget_string(&mut file, &format, 0, 0, &opts);
        assert!(result.is_err(), "retarget same id should fail");
    }

    #[test]
    fn retarget_string_out_of_range_rejected() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        let bad_id = file.header.string_count + 100;
        let result = retarget_string(&mut file, &format, 0, bad_id, &opts);
        assert!(result.is_err(), "retarget out of range should fail");
    }

    #[test]
    fn retarget_string_identifier_hash_updated() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // Find two identifiers.
        let ids: Vec<u32> = file
            .strings
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_identifier)
            .map(|(i, _)| i as u32)
            .collect();
        if ids.len() < 2 {
            return;
        }
        let (a, b) = (ids[0], ids[ids.len() - 1]);
        let to_val = file.strings[b as usize].value.clone();
        let opts = PatchOptions::default();
        let out = retarget_string(&mut file, &format, a, b, &opts).expect("retarget identifier");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.strings[a as usize].value, to_val);
        // The identifier hash for `a` should now match the hash of `to_val`.
        let idx_a = (0..a as usize)
            .filter(|&i| re.strings[i].is_identifier)
            .count();
        assert_eq!(
            re.identifier_hashes[idx_a],
            hermes_identifier_hash(&to_val),
            "identifier hash not updated"
        );
    }

    // R2: retarget copies the 4-byte small entry, and an overflowed entry holds
    // the index of its overflow record, so retargeting to or from one is just as
    // valid -- it used to be refused. Retargeting entry 0 at the overflowed entry
    // also makes entry 0 point at the *last* record while sitting first in the
    // table, which a reader that counts overflowed entries in table order (as
    // this crate's readers did) resolves to the wrong record; the engine indexes
    // by the entry, and so does every reader now. P0 checks each step.
    #[test]
    fn retarget_works_to_and_from_an_overflow_entry() {
        let mut file = plain();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let opts = PatchOptions::default();
        let long_val = "X".repeat(256);
        let (_, long_id) = add_string(&mut file, &format, &long_val, false, &opts).unwrap();
        assert_eq!(
            file.header.overflow_string_count, 1,
            "the long string overflows"
        );

        retarget_string(&mut file, &format, 0, long_id, &opts).expect("retarget to overflow");
        assert_eq!(file.strings[0].value, long_val);
        assert_eq!(file.strings[long_id as usize].value, long_val);

        let one = file.strings[1].value.clone();
        retarget_string(&mut file, &format, long_id, 1, &opts).expect("retarget from overflow");
        assert_eq!(file.strings[long_id as usize].value, one);
        assert_eq!(
            file.strings[0].value, long_val,
            "entry 0 still shares the record"
        );
    }

    // ---- add_string tests ----

    #[test]
    fn add_string_ascii_reparses() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let old_count = file.header.string_count;
        let opts = PatchOptions::default();
        let (out, new_id) = add_string(&mut file, &format, "hello_world", false, &opts)
            .expect("add_string plain ASCII");
        assert_eq!(new_id, old_count);
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.string_count, old_count + 1);
        assert_eq!(re.strings[new_id as usize].value, "hello_world");
        assert!(!re.strings[new_id as usize].is_utf16);
        assert!(!re.strings[new_id as usize].is_identifier);
    }

    #[test]
    fn add_string_utf16() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        let (out, new_id) =
            add_string(&mut file, &format, "café☕", false, &opts).expect("add_string UTF-16");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings[new_id as usize].is_utf16);
        assert_eq!(re.strings[new_id as usize].value, "café☕");
    }

    #[test]
    fn add_string_identifier() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let old_id_count = file.header.identifier_count;
        let opts = PatchOptions::default();
        let (out, new_id) = add_string(&mut file, &format, "myNewProp", true, &opts)
            .expect("add_string identifier");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.identifier_count, old_id_count + 1);
        assert!(re.strings[new_id as usize].is_identifier);
        assert_eq!(re.strings[new_id as usize].value, "myNewProp");
        // The hash in the reparsed file should match our Jenkins hash.
        let expected_hash = hermes_identifier_hash("myNewProp");
        assert_eq!(
            *re.identifier_hashes.last().unwrap(),
            expected_hash,
            "identifier hash mismatch"
        );
    }

    // Appending a string with same kind as the last run should bump the count
    // but not add a new run.
    #[test]
    fn add_string_kind_run_extends() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let old_kind_count = file.header.string_kind_count;
        let last_kind_is_string = file
            .string_kinds
            .last()
            .map(|k| k.kind == crate::file::StringKindType::String)
            .unwrap_or(false);
        let opts = PatchOptions::default();
        // Append a plain string (kind = String).
        let (out, _) = add_string(&mut file, &format, "extend_run", false, &opts)
            .expect("add_string extend run");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        if last_kind_is_string {
            // Same kind as last run: no new run entry.
            assert_eq!(re.header.string_kind_count, old_kind_count);
        } else {
            // Different kind: new run.
            assert_eq!(re.header.string_kind_count, old_kind_count + 1);
        }
    }

    // Appending a string with different kind than the last run should add a new
    // string_kind entry.
    #[test]
    fn add_string_kind_run_new() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let old_kind_count = file.header.string_kind_count;
        let last_kind_is_identifier = file
            .string_kinds
            .last()
            .map(|k| k.kind == crate::file::StringKindType::Identifier)
            .unwrap_or(false);
        let opts = PatchOptions::default();
        // Append an identifier (kind = Identifier) -- opposite of the last run's
        // kind if it was String.
        if !last_kind_is_identifier {
            let (out, _) = add_string(&mut file, &format, "newIdent", true, &opts)
                .expect("add_string new kind run");
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(
                re.header.string_kind_count,
                old_kind_count + 1,
                "expected new string_kind run"
            );
        }
    }

    // Existing strings remain intact after an append.
    #[test]
    fn add_string_existing_strings_intact() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let originals: Vec<(String, bool, bool)> = file
            .strings
            .iter()
            .map(|s| (s.value.clone(), s.is_utf16, s.is_identifier))
            .collect();
        let opts = PatchOptions::default();
        let (out, _) = add_string(&mut file, &format, "roundtrip_check", false, &opts)
            .expect("add_string roundtrip");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        for (i, (val, utf16, ident)) in originals.iter().enumerate() {
            assert_eq!(&re.strings[i].value, val, "string {i} value changed");
            assert_eq!(re.strings[i].is_utf16, *utf16, "string {i} utf16 changed");
            assert_eq!(
                re.strings[i].is_identifier, *ident,
                "string {i} identifier changed"
            );
        }
    }

    // Modern v98 layout: append + reparse (mirrors inject_stub_modern_v98).
    #[test]
    fn add_string_modern_v98_reparses() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v98/expressions/class_basic/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
            return;
        }
        let (mut file, format) = load(path);
        assert!(matches!(
            file.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));
        let old_count = file.header.string_count;
        let opts = PatchOptions::default();
        let (out, new_id) = add_string(&mut file, &format, "modernTestProp", true, &opts)
            .expect("add_string on modern v98");
        assert_eq!(new_id, old_count);
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).expect("reparse after modern add_string");
        assert_eq!(re.header.string_count, old_count + 1);
        assert_eq!(re.strings[new_id as usize].value, "modernTestProp");
        assert!(re.strings[new_id as usize].is_identifier);
        // All function headers must still parse (overflowed large headers
        // relocated correctly).
        assert_eq!(
            re.function_headers.len(),
            file.function_headers.len(),
            "function count changed after modern add_string"
        );
    }

    // After an append, a function that references existing strings must still
    // disassemble correctly (downstream offsets intact).
    #[test]
    fn add_string_downstream_offsets_intact() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        // Disassemble function 0 before the append.
        let disasm_opts = crate::DisasmOptions {
            show_offsets: false,
            show_labels: true,
            resolve_strings: true,
            enable_color: false,
        };
        let before = crate::disassemble_function(&file, &format, 0, &disasm_opts)
            .expect("disasm before add_string");
        let (out, _) = add_string(&mut file, &format, "offset_check", false, &opts)
            .expect("add_string for offset check");
        let re = BytecodeFile::parse_auto(&out).unwrap();
        let fmt2 = BytecodeFormat::for_version(re.header.version).unwrap();
        let after = crate::disassemble_function(&re, &fmt2, 0, &disasm_opts)
            .expect("disasm after add_string");
        assert_eq!(
            before, after,
            "function 0 disassembly changed after add_string"
        );
    }

    // Overflow threshold: exercise the overflow path by appending a string whose
    // length exceeds the small-entry 8-bit limit (0xff = 255).
    #[test]
    fn add_string_overflow_entry() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        // A string of 256 characters exceeds the small-entry length field (max 254).
        let long_value = "A".repeat(256);
        let (out, new_id) = add_string(&mut file, &format, &long_value, false, &opts)
            .expect("add_string with overflow-length string");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.strings[new_id as usize].value, long_value);
        // The overflow table must have grown (the new entry must have been routed
        // through the overflow table since its length >= 0xff).
        assert!(
            re.header.overflow_string_count > 0,
            "expected at least one overflow entry for the long string"
        );
    }

    // A patch that stays pure ASCII keeps the one-byte encoding.
    #[test]
    fn patch_ascii_stays_one_byte() {
        if !crate::write::corpus_fixture_present(FIXTURE) {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        let out = patch_string_by_id(
            &mut file,
            &format,
            id as u32,
            "PLAINASCII",
            &PatchOptions::default(),
        )
        .expect("patch ascii");
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(!re.strings[id].is_utf16, "ascii value must stay one byte");
        assert_eq!(re.strings[id].value, "PLAINASCII");
    }

    // ---- patch_string_replace (--old) tests ----
    //
    // plan_guides/06_write/RISKS.md flags patch_string_replace (the by-value entry point behind the
    // CLI's --old) as having no test. These build a real image with create_minimal
    // so they run in CI.
    fn make_v96(strings: Vec<String>) -> (BytecodeFile, BytecodeFormat) {
        let bytes = crate::write::create::create_minimal(&crate::write::create::CreateOptions {
            version: 96,
            strings,
            ..Default::default()
        })
        .expect("create_minimal v96");
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created file");
        let format = BytecodeFormat::for_version_or_latest(96).expect("format").0;
        (file, format)
    }

    // Same-length replace located by value: the old value is found and rewritten.
    #[test]
    fn patch_string_replace_by_value_same_length() {
        let (mut file, format) = make_v96(vec!["global".into(), "hello".into()]);
        let out = patch_string_replace(
            &mut file,
            &format,
            "hello",
            "world",
            &PatchOptions::default(),
        )
        .expect("replace hello→world");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings.iter().any(|s| s.value == "world"));
        assert!(!re.strings.iter().any(|s| s.value == "hello"));
    }

    // Length-changing replace located by value: the lookup path also drives the
    // resize/rebuild.
    #[test]
    fn patch_string_replace_by_value_grow() {
        let (mut file, format) = make_v96(vec!["global".into(), "hello".into()]);
        let out = patch_string_replace(
            &mut file,
            &format,
            "hello",
            "hello_world_longer",
            &PatchOptions::default(),
        )
        .expect("replace hello→hello_world_longer");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings.iter().any(|s| s.value == "hello_world_longer"));
        assert!(!re.strings.iter().any(|s| s.value == "hello"));
    }

    // A value not in the table is an error, not a panic or silent no-op.
    #[test]
    fn patch_string_replace_not_found_errors() {
        let (mut file, format) = make_v96(vec!["global".into()]);
        let err = patch_string_replace(
            &mut file,
            &format,
            "nonexistent",
            "x",
            &PatchOptions::default(),
        )
        .expect_err("missing old value must error");
        assert!(
            err.to_string().contains("not found"),
            "error should say the value was not found, got: {err}"
        );
    }

    fn plain() -> BytecodeFile {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plain.v96.hbc");
        BytecodeFile::parse_auto(&std::fs::read(path).unwrap()).unwrap()
    }

    fn id_of(file: &BytecodeFile, value: &str) -> u32 {
        file.strings.iter().position(|s| s.value == value).unwrap() as u32
    }

    // hermesc -O overlapped `alpha` with `global` ("...globalpha..."), so the
    // first same-length patch must rebuild rather than overwrite `global`'s
    // tail. That rebuild pins `alpha` to bytes of its own, so the *next*
    // same-length patch is in place -- which is what the pin set is for: without
    // it, a packed rebuild could re-share the entry and every later patch would
    // bounce back to a full rebuild.
    #[test]
    fn a_rebuilt_entry_is_pinned_so_the_next_patch_is_in_place() {
        let mut file = plain();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let id = id_of(&file, "alpha");
        let opts = PatchOptions::default();

        let first = patch_string_by_id(&mut file, &format, id, "omega", &opts).unwrap();
        assert_eq!(file.strings[id as usize].value, "omega");
        assert!(
            file.strings.iter().any(|s| s.value == "global"),
            "global survived"
        );

        let second = patch_string_by_id(&mut file, &format, id, "sigma", &opts).unwrap();
        assert_eq!(file.strings[id as usize].value, "sigma");
        assert_eq!(first.len(), second.len());
        let changed = first.iter().zip(&second).filter(|(a, b)| a != b).count();
        assert!(
            changed <= 5 + 20,
            "{changed} bytes changed: the second patch rebuilt instead of writing in place"
        );
    }

    // Same byte length is not same storage: `bé` is 3 UTF-8 bytes like `bob`,
    // but Hermes stores it as 2 UTF-16 code units. `bob` owns its bytes, so this
    // used to take the in-place path and write UTF-8 into an ASCII entry.
    #[test]
    fn a_non_ascii_value_of_the_same_byte_length_is_re_encoded() {
        let mut file = plain();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let id = id_of(&file, "bob");
        assert_eq!("bé".len(), "bob".len());
        patch_string_by_id(&mut file, &format, id, "bé", &PatchOptions::default()).unwrap();
        assert_eq!(file.strings[id as usize].value, "bé");
        assert!(file.strings[id as usize].is_utf16);
    }
}
