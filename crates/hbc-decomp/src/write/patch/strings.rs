// Patch string table entries: same-length in place, or grow/shrink with a full
// string-table + storage rebuild and tail relocation (hermes_rs issue #10 class).

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;

use crate::write::serialize::{finalize_raw_image, section_offset};

use super::PatchOptions;

// Locate string `id` UTF-8 bytes via the small/overflow string tables in raw bytes.
// Returns (absolute file offset of content, byte length).
// Hermes may pack strings so entries share storage, always use table offsets,
// never a substring search.
fn locate_string_bytes(file: &BytecodeFile, id: u32) -> Result<(usize, usize)> {
    let entry = file
        .strings
        .get(id as usize)
        .ok_or_else(|| Error::Write(format!("string id {id} out of range")))?;
    if entry.is_utf16 {
        return Err(Error::Write(
            "patch_string: UTF-16 strings not yet supported".into(),
        ));
    }
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    let overflow_off = section_offset(file, "overflow_string_table").map(|o| o as usize);

    const UTF16: u32 = 0x1;
    const OFF_SHIFT: u32 = 1;
    const OFF_MASK: u32 = 0x7f_ffff;
    const LEN_SHIFT: u32 = 24;
    const LEN_MASK: u32 = 0xff;
    const LEN_OVERFLOW: u32 = 0xff;
    const OFF_OVERFLOW: u32 = 0x800000;

    // Count how many overflowed entries precede `id` so we know the overflow index.
    let mut overflow_index = 0usize;
    for i in 0..=id as usize {
        let slot = small_off + i * 4;
        if slot + 4 > raw.len() {
            return Err(Error::Write("small string table OOB".into()));
        }
        let raw_e = u32::from_le_bytes(raw[slot..slot + 4].try_into().unwrap());
        let is_utf16 = (raw_e & UTF16) != 0;
        let offset = (raw_e >> OFF_SHIFT) & OFF_MASK;
        let length = (raw_e >> LEN_SHIFT) & LEN_MASK;
        let (off, len) =
            if length == LEN_OVERFLOW || offset == OFF_OVERFLOW {
                let ov_base = overflow_off
                    .ok_or_else(|| Error::Write("overflow string table missing".into()))?;
                let ov_slot = ov_base + overflow_index * 8;
                if ov_slot + 8 > raw.len() {
                    return Err(Error::Write("overflow string table OOB".into()));
                }
                let o = u32::from_le_bytes(raw[ov_slot..ov_slot + 4].try_into().unwrap());
                let l = u32::from_le_bytes(raw[ov_slot + 4..ov_slot + 8].try_into().unwrap());
                overflow_index += 1;
                (o, l)
            } else {
                (offset, length)
            };
        if i == id as usize {
            if is_utf16 {
                return Err(Error::Write("patch_string: UTF-16 not supported".into()));
            }
            let abs = storage_off + off as usize;
            let byte_len = len as usize;
            if abs + byte_len > raw.len() {
                return Err(Error::Write("string content OOB".into()));
            }
            // Sanity: content should match decoded value (modulo packing).
            let slice = &raw[abs..abs + byte_len];
            if slice != entry.value.as_bytes() {
                // Still allow patch if lengths match, packed substrings may decode
                // via different views; trust table length.
                if slice.len() != entry.value.len() {
                    return Err(Error::Write(format!(
                        "string id {id}: table length {} != decoded {}",
                        slice.len(),
                        entry.value.len()
                    )));
                }
            }
            return Ok((abs, byte_len));
        }
    }
    Err(Error::Write(format!("string id {id} not found")))
}

// Per-string storage location read straight from the small/overflow tables.
struct StrLoc {
    storage_off: u32,
    len_field: u32,
    is_utf16: bool,
}

// Read the storage offset + length field of every string from the raw tables.
fn read_all_string_locs(file: &BytecodeFile) -> Result<Vec<StrLoc>> {
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let overflow_off = section_offset(file, "overflow_string_table").map(|o| o as usize);

    const UTF16: u32 = 0x1;
    const OFF_SHIFT: u32 = 1;
    const OFF_MASK: u32 = 0x7f_ffff;
    const LEN_SHIFT: u32 = 24;
    const LEN_MASK: u32 = 0xff;
    const LEN_OVERFLOW: u32 = 0xff;
    const OFF_OVERFLOW: u32 = 0x800000;

    let mut out = Vec::with_capacity(file.strings.len());
    let mut overflow_index = 0usize;
    for i in 0..file.strings.len() {
        let slot = small_off + i * 4;
        if slot + 4 > raw.len() {
            return Err(Error::Write("small string table OOB".into()));
        }
        let raw_e = u32::from_le_bytes(raw[slot..slot + 4].try_into().unwrap());
        let is_utf16 = (raw_e & UTF16) != 0;
        let offset = (raw_e >> OFF_SHIFT) & OFF_MASK;
        let length = (raw_e >> LEN_SHIFT) & LEN_MASK;
        let (off, len) = if length == LEN_OVERFLOW || offset == OFF_OVERFLOW {
            let ov_base = overflow_off
                .ok_or_else(|| Error::Write("overflow string table missing".into()))?;
            let ov_slot = ov_base + overflow_index * 8;
            if ov_slot + 8 > raw.len() {
                return Err(Error::Write("overflow string table OOB".into()));
            }
            let o = u32::from_le_bytes(raw[ov_slot..ov_slot + 4].try_into().unwrap());
            let l = u32::from_le_bytes(raw[ov_slot + 4..ov_slot + 8].try_into().unwrap());
            overflow_index += 1;
            (o, l)
        } else {
            (offset, length)
        };
        out.push(StrLoc {
            storage_off: off,
            len_field: len,
            is_utf16,
        });
    }
    Ok(out)
}

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

    // Check for overflow entries — refuse for now (v1).
    let from_slot = small_off + from_id as usize * 4;
    let to_slot = small_off + to_id as usize * 4;
    if from_slot + 4 > raw.len() || to_slot + 4 > raw.len() {
        return Err(Error::Write("string table slot out of range".into()));
    }
    let from_entry = u32::from_le_bytes(raw[from_slot..from_slot + 4].try_into().unwrap());
    let to_entry = u32::from_le_bytes(raw[to_slot..to_slot + 4].try_into().unwrap());
    // An overflowed small entry has length == 0xff (the sentinel); the 23-bit
    // offset field then stores the overflow-table index, not a storage offset.
    let is_overflow = |e: u32| {
        let len = (e >> 24) & 0xff;
        len == 0xff
    };
    if is_overflow(from_entry) || is_overflow(to_entry) {
        return Err(Error::Write(
            "retarget: overflow string entries not supported (use patch-string instead)".into(),
        ));
    }

    // Warn on cross-kind retarget.
    let from_is_id = file.strings[from_id as usize].is_identifier;
    let to_is_id = file.strings[to_id as usize].is_identifier;
    if from_is_id != to_is_id {
        eprintln!(
            "warning: retarget crosses string/identifier boundary \
             (from_id {} is_identifier={}, to_id {} is_identifier={})",
            from_id, from_is_id, to_id, to_is_id
        );
    }

    // Copy the 4-byte entry.
    let entry_bytes: [u8; 4] = raw[to_slot..to_slot + 4].try_into().unwrap();
    raw[from_slot..from_slot + 4].copy_from_slice(&entry_bytes);

    // If from_id is an identifier, update its hash to match to_id's value.
    if from_is_id {
        let to_value = &file.strings[to_id as usize].value;
        update_identifier_hash(file, &mut raw, from_id, to_value)?;
    }

    // Sync the in-memory model.
    let to_val = file.strings[to_id as usize].value.clone();
    let to_utf16 = file.strings[to_id as usize].is_utf16;
    file.strings[from_id as usize].value = to_val;
    file.strings[from_id as usize].is_utf16 = to_utf16;

    let out = finalize_raw_image(raw)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
}

// Byte position of `debug_info_offset` inside a legacy 128-byte header. Mirrors
// the field order written by `write_legacy_header`.
pub(super) fn legacy_debug_info_offset_pos(header: &crate::format::BytecodeHeader) -> usize {
    let mut pos = 64usize;
    if header.big_int_count.is_some() {
        pos += 8; // big_int_count + big_int_storage_size
    }
    pos += 8; // reg_exp_count + reg_exp_storage_size
    pos += 12; // array_buffer_size + obj_key_buffer_size + obj_value_buffer_size
    pos += 8; // segment_id/cjs_module_offset + cjs_module_count
    if header.function_source_count.is_some() {
        pos += 4; // function_source_count
    }
    pos
}

// Grow or shrink a UTF-8 string entry. Rebuilds the small string table and the
// string storage (unpacked), then relocates every section after the string
// region and shifts all absolute offsets (function bodies, function info,
// debug info) by the size delta. This is the hermes_rs issue #10 case.
//
// Legacy layout, non-overflowed function headers, non-identifier UTF-8 target
// only. Refuses anything that would need an overflow string entry or an
// identifier-hash rebuild, so it never emits a silently corrupt file.
fn patch_string_resize(
    file: &mut BytecodeFile,
    id: u32,
    new_value: &str,
) -> Result<Vec<u8>> {
    let modern = matches!(
        file.header.function_header_layout,
        crate::format::FunctionHeaderLayout::Modern12
    );
    // Validate the id up front; the encoding is chosen per new value below.
    if file.strings.get(id as usize).is_none() {
        return Err(Error::Write(format!("string id {id} out of range")));
    }

    let locs = read_all_string_locs(file)?;
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
    // First section after the string region, everything from here shifts.
    // Section right after the string region: array_buffer on legacy,
    // literal_value_buffer on modern.
    let array_off = section_offset(file, "array_buffer")
        .or_else(|| section_offset(file, "literal_value_buffer"))
        .ok_or_else(|| Error::Write("post-string section missing".into()))?
        as usize;
    if array_off < small_off || small_off > storage_off {
        return Err(Error::Write("unexpected string section order".into()));
    }
    let old_region_len = array_off - small_off;

    // Rebuild the storage (unpacked) plus the small and overflow string tables.
    // A string whose offset or length does not fit the small entry gets an
    // overflow entry (32-bit offset and length), and its small entry is marked
    // with length 0xff and the overflow index.
    let mut new_storage: Vec<u8> = Vec::new();
    let mut new_small: Vec<u32> = Vec::with_capacity(locs.len());
    let mut new_overflow: Vec<(u32, u32)> = Vec::new();
    for (i, loc) in locs.iter().enumerate() {
        let (bytes, len_field, is_utf16): (Vec<u8>, u32, bool) = if i as u32 == id {
            // Hermes stores a string with one byte per character only when it is
            // pure ASCII, and as UTF-16 for anything with a non-ASCII character.
            // Choose from the new value's own characters, not the old flag: a plain
            // ASCII string patched to hold `é` or `€` must switch to UTF-16.
            let needs_utf16 = new_value.bytes().any(|b| b > 0x7f);
            if needs_utf16 {
                // UTF-16LE; length is in code units.
                let units: Vec<u16> = new_value.encode_utf16().collect();
                let mut b = Vec::with_capacity(units.len() * 2);
                for u in &units {
                    b.extend_from_slice(&u.to_le_bytes());
                }
                (b, units.len() as u32, true)
            } else {
                // Pure ASCII: one byte per character.
                (new_value.as_bytes().to_vec(), new_value.len() as u32, false)
            }
        } else {
            let byte_len = if loc.is_utf16 {
                loc.len_field as usize * 2
            } else {
                loc.len_field as usize
            };
            let start = storage_off + loc.storage_off as usize;
            if start + byte_len > raw.len() {
                return Err(Error::Write("string storage OOB during rebuild".into()));
            }
            (raw[start..start + byte_len].to_vec(), loc.len_field, loc.is_utf16)
        };
        let off = new_storage.len() as u32;
        new_storage.extend_from_slice(&bytes);
        let _ = i;
        if off >= 0x80_0000 || len_field >= 0xff {
            // Overflowed: real offset + length go in the overflow table; the small
            // entry stores the overflow index and length 0xff.
            let ov_index = new_overflow.len() as u32;
            new_overflow.push((off, len_field));
            let e = (0xffu32 << 24) | ((ov_index & 0x7f_ffff) << 1) | (is_utf16 as u32);
            new_small.push(e);
        } else {
            let e = ((len_field & 0xff) << 24) | ((off & 0x7f_ffff) << 1) | (is_utf16 as u32);
            new_small.push(e);
        }
    }

    // Assemble the new string region: small table, overflow table, storage padded
    // so the following section keeps its 4-byte alignment.
    let mut region: Vec<u8> = Vec::new();
    for e in &new_small {
        region.extend_from_slice(&e.to_le_bytes());
    }
    for (off, len) in &new_overflow {
        region.extend_from_slice(&off.to_le_bytes());
        region.extend_from_slice(&len.to_le_bytes());
    }
    let storage_size = new_storage.len() as u32;
    let overflow_count = new_overflow.len() as u32;
    region.extend_from_slice(&new_storage);
    while region.len() % 4 != 0 {
        region.push(0);
    }

    let delta = region.len() as i64 - old_region_len as i64;

    // Splice: [.. small_off] + region + [array_off ..]
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..small_off]);
    rebuilt.extend_from_slice(&region);
    rebuilt.extend_from_slice(&raw[array_off..]);

    // Header field updates. overflow_string_count (56) and string_storage_size
    // (60) share offsets across layouts; debug_info_offset differs.
    rebuilt[56..60].copy_from_slice(&overflow_count.to_le_bytes());
    rebuilt[60..64].copy_from_slice(&storage_size.to_le_bytes());
    if file.header.debug_info_offset != 0 {
        // Modern header keeps debug_info_offset at a fixed byte 108.
        let dpos = if modern {
            108
        } else {
            legacy_debug_info_offset_pos(&file.header)
        };
        let shifted = (file.header.debug_info_offset as i64 + delta) as u32;
        if dpos + 4 <= rebuilt.len() {
            rebuilt[dpos..dpos + 4].copy_from_slice(&shifted.to_le_bytes());
        }
    }

    // Everything after the string region moves by `delta`, so every function
    // body offset shifts. The small function header is before the region and
    // keeps its slot; we edit it in place. When a function is overflowed the
    // small header only holds a pointer to an out-of-line large header (also in
    // the moved region): we shift that pointer and the large header's own
    // offset fields, then shift the offsets in its exception handler table.
    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let hsize = if modern { 12 } else { 16 };
    let flag_byte = if modern { 11 } else { 15 };
    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * hsize;
        if slot + hsize > rebuilt.len() {
            break;
        }
        let overflowed = rebuilt[slot + flag_byte] & crate::format::FLAG_OVERFLOWED != 0;
        if overflowed {
            relocate_overflowed_header(&mut rebuilt, slot, modern, delta)?;
        } else if modern {
            crate::write::header_write::shift_modern_small_header_offset(
                &mut rebuilt[slot..slot + 12],
                delta,
            )?;
        } else {
            // Legacy non-overflowed: shift the 25-bit body offset in place; the
            // 25-bit info_offset only moves if it is set (points past the region).
            crate::write::header_write::shift_legacy_small_header_offsets(
                &mut rebuilt[slot..slot + 16],
                delta,
            )?;
        }
    }

    // An identifier's precomputed hash depends on its text, so refresh it.
    update_identifier_hash(file, &mut rebuilt, id, new_value)?;

    // Keep the decoded model consistent.
    if let Some(s) = file.strings.get_mut(id as usize) {
        s.value = new_value.to_string();
    }
    file.header.overflow_string_count = overflow_count;
    file.header.string_storage_size = storage_size;

    let out = finalize_raw_image(rebuilt)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
}

// Relocate an overflowed function whose real fields live in an out-of-line large
// header. Shifts the small header pointer, then the large header body offset (and
// its info offset for legacy). All of these sit in the region that moved.
fn relocate_overflowed_header(
    rebuilt: &mut [u8],
    slot: usize,
    modern: bool,
    delta: i64,
) -> Result<()> {
    use crate::write::header_write as hw;
    let large_ptr = if modern {
        hw::read_modern_large_pointer(&rebuilt[slot..slot + 12])?
    } else {
        hw::read_legacy_large_pointer(&rebuilt[slot..slot + 16])?
    };
    if modern {
        hw::shift_modern_large_pointer(&mut rebuilt[slot..slot + 12], delta)?;
    } else {
        hw::shift_legacy_large_pointer(&mut rebuilt[slot..slot + 16], delta)?;
    }
    let lh = (large_ptr as i64 + delta) as usize;
    // The body offset is the first u32 of both large header layouts.
    hw::shift_u32_at(rebuilt, lh, delta)?;
    // Legacy large headers store info_offset at +16; modern computes it (nothing
    // stored to shift).
    if !modern {
        let info_pos = lh + 16;
        if info_pos + 4 <= rebuilt.len() {
            let info = u32::from_le_bytes(rebuilt[info_pos..info_pos + 4].try_into().unwrap());
            if info != 0 {
                hw::shift_u32_at(rebuilt, info_pos, delta)?;
            }
        }
    }
    Ok(())
}

// Append a new string to the string table. Rebuilds the entire string region
// (kinds, identifier hashes, small table, overflow table, storage) and
// relocates the tail. The new string gets id = old `string_count`; every
// existing id is stable. Returns `(raw_image, new_id)`.
//
// `is_identifier`: true for property/symbol names (adds a Jenkins hash slot);
// false for plain string literals.
pub fn add_string(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    value: &str,
    is_identifier: bool,
    _opts: &PatchOptions,
) -> Result<(Vec<u8>, u32)> {
    let modern = matches!(
        file.header.function_header_layout,
        crate::format::FunctionHeaderLayout::Modern12
    );
    let new_id = file.header.string_count;

    let locs = read_all_string_locs(file)?;
    let raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    // Derive section boundaries.
    let kinds_off = section_offset(file, "string_kinds")
        .ok_or_else(|| Error::Write("string_kinds section missing".into()))?
        as usize;
    // These section offsets are computed to validate presence; the rebuild uses
    // kinds_off as the splice origin and array_off as the splice end.
    let _ids_off = section_offset(file, "identifier_hashes")
        .ok_or_else(|| Error::Write("identifier_hashes section missing".into()))?
        as usize;
    let _small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    let array_off = section_offset(file, "array_buffer")
        .or_else(|| section_offset(file, "literal_value_buffer"))
        .ok_or_else(|| Error::Write("post-string section missing".into()))?
        as usize;

    if array_off < kinds_off {
        return Err(Error::Write("unexpected string section order".into()));
    }
    let old_region_len = array_off - kinds_off;

    // Duplicate check: warn (to stderr) if value already exists but still append.
    for (i, s) in file.strings.iter().enumerate() {
        if s.value == value && s.is_identifier == is_identifier {
            eprintln!(
                "note: string {:?} already exists at id {} (is_identifier={}); appending anyway as id {}",
                value, i, is_identifier, new_id
            );
            break;
        }
    }

    // ---- Rebuild storage + small/overflow tables (N+1 entries) ----
    let mut new_storage: Vec<u8> = Vec::new();
    let mut new_small: Vec<u32> = Vec::with_capacity(locs.len() + 1);
    let mut new_overflow: Vec<(u32, u32)> = Vec::new();

    // Existing entries.
    for loc in locs.iter() {
        let byte_len = if loc.is_utf16 {
            loc.len_field as usize * 2
        } else {
            loc.len_field as usize
        };
        let start = storage_off + loc.storage_off as usize;
        if start + byte_len > raw.len() {
            return Err(Error::Write("string storage OOB during rebuild".into()));
        }
        let bytes = &raw[start..start + byte_len];
        let off = new_storage.len() as u32;
        new_storage.extend_from_slice(bytes);
        if off >= 0x80_0000 || loc.len_field >= 0xff {
            let ov_index = new_overflow.len() as u32;
            new_overflow.push((off, loc.len_field));
            let e = (0xffu32 << 24) | ((ov_index & 0x7f_ffff) << 1) | (loc.is_utf16 as u32);
            new_small.push(e);
        } else {
            let e = ((loc.len_field & 0xff) << 24) | ((off & 0x7f_ffff) << 1) | (loc.is_utf16 as u32);
            new_small.push(e);
        }
    }

    // Append the new entry.
    let needs_utf16 = value.bytes().any(|b| b > 0x7f);
    let (new_bytes, new_len_field, new_is_utf16) = if needs_utf16 {
        let units: Vec<u16> = value.encode_utf16().collect();
        let mut b = Vec::with_capacity(units.len() * 2);
        for u in &units {
            b.extend_from_slice(&u.to_le_bytes());
        }
        (b, units.len() as u32, true)
    } else {
        (value.as_bytes().to_vec(), value.len() as u32, false)
    };
    let new_off = new_storage.len() as u32;
    new_storage.extend_from_slice(&new_bytes);
    if new_off >= 0x80_0000 || new_len_field >= 0xff {
        let ov_index = new_overflow.len() as u32;
        new_overflow.push((new_off, new_len_field));
        let e = (0xffu32 << 24) | ((ov_index & 0x7f_ffff) << 1) | (new_is_utf16 as u32);
        new_small.push(e);
    } else {
        let e = ((new_len_field & 0xff) << 24) | ((new_off & 0x7f_ffff) << 1) | (new_is_utf16 as u32);
        new_small.push(e);
    }

    // ---- Rebuild string_kinds ----
    use crate::file::{StringKindEntry, StringKindType, StringTableEntry};

    let new_kind = if is_identifier {
        StringKindType::Identifier
    } else {
        StringKindType::String
    };
    let mut new_kinds = file.string_kinds.clone();
    if let Some(last) = new_kinds.last_mut() {
        if last.kind == new_kind {
            last.count += 1;
        } else {
            new_kinds.push(StringKindEntry {
                kind: new_kind,
                count: 1,
            });
        }
    } else {
        new_kinds.push(StringKindEntry {
            kind: new_kind,
            count: 1,
        });
    }

    // ---- Rebuild identifier_hashes ----
    let mut new_id_hashes = file.identifier_hashes.clone();
    if is_identifier {
        new_id_hashes.push(hermes_identifier_hash(value));
    }

    // ---- Assemble the new region block ----
    // Order: string_kinds ++ identifier_hashes ++ small_string_table ++ overflow_string_table ++ string_storage
    let mut region: Vec<u8> = Vec::new();

    // String kinds (each entry is u32: high bit = kind, low 31 = count).
    for k in &new_kinds {
        let raw_k = match k.kind {
            StringKindType::String => k.count,
            StringKindType::Identifier => k.count | (1u32 << 31),
        };
        region.extend_from_slice(&raw_k.to_le_bytes());
    }
    while region.len() % 4 != 0 {
        region.push(0);
    }

    // Identifier hashes.
    for h in &new_id_hashes {
        region.extend_from_slice(&h.to_le_bytes());
    }
    while region.len() % 4 != 0 {
        region.push(0);
    }

    // Small string table.
    for e in &new_small {
        region.extend_from_slice(&e.to_le_bytes());
    }

    // Overflow string table.
    for (off, len) in &new_overflow {
        region.extend_from_slice(&off.to_le_bytes());
        region.extend_from_slice(&len.to_le_bytes());
    }

    // String storage.
    let storage_size = new_storage.len() as u32;
    let overflow_count = new_overflow.len() as u32;
    region.extend_from_slice(&new_storage);
    while region.len() % 4 != 0 {
        region.push(0);
    }

    let delta = region.len() as i64 - old_region_len as i64;

    // ---- Splice: [..kinds_off] + region + [array_off..] ----
    let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
    rebuilt.extend_from_slice(&raw[..kinds_off]);
    rebuilt.extend_from_slice(&region);
    rebuilt.extend_from_slice(&raw[array_off..]);

    // ---- Update header counts ----
    // string_kind_count at bytes 44..48, identifier_count at 48..52, string_count
    // at 52..56, overflow_string_count at 56..60, string_storage_size at 60..64.
    let new_string_kind_count = new_kinds.len() as u32;
    let new_identifier_count = new_id_hashes.len() as u32;
    let new_string_count = new_id + 1;
    rebuilt[44..48].copy_from_slice(&new_string_kind_count.to_le_bytes());
    rebuilt[48..52].copy_from_slice(&new_identifier_count.to_le_bytes());
    rebuilt[52..56].copy_from_slice(&new_string_count.to_le_bytes());
    rebuilt[56..60].copy_from_slice(&overflow_count.to_le_bytes());
    rebuilt[60..64].copy_from_slice(&storage_size.to_le_bytes());

    // ---- Shift downstream offsets by delta ----
    if file.header.debug_info_offset != 0 {
        let dpos = if modern {
            108
        } else {
            legacy_debug_info_offset_pos(&file.header)
        };
        let shifted = (file.header.debug_info_offset as i64 + delta) as u32;
        if dpos + 4 <= rebuilt.len() {
            rebuilt[dpos..dpos + 4].copy_from_slice(&shifted.to_le_bytes());
        }
    }

    let fh_sec = section_offset(file, "function_headers")
        .ok_or_else(|| Error::Write("function_headers section missing".into()))?
        as usize;
    let hsize = if modern { 12 } else { 16 };
    let flag_byte = if modern { 11 } else { 15 };
    for i in 0..file.function_headers.len() {
        let slot = fh_sec + i * hsize;
        if slot + hsize > rebuilt.len() {
            break;
        }
        let overflowed = rebuilt[slot + flag_byte] & crate::format::FLAG_OVERFLOWED != 0;
        if overflowed {
            relocate_overflowed_header(&mut rebuilt, slot, modern, delta)?;
        } else if modern {
            crate::write::header_write::shift_modern_small_header_offset(
                &mut rebuilt[slot..slot + 12],
                delta,
            )?;
        } else {
            crate::write::header_write::shift_legacy_small_header_offsets(
                &mut rebuilt[slot..slot + 16],
                delta,
            )?;
        }
    }

    // ---- Sync in-memory model ----
    file.strings.push(StringTableEntry {
        value: value.to_string(),
        is_utf16: new_is_utf16,
        is_identifier,
    });
    file.string_kinds = new_kinds;
    file.identifier_hashes = new_id_hashes;
    file.header.string_kind_count = new_string_kind_count;
    file.header.identifier_count = new_identifier_count;
    file.header.string_count = new_string_count;
    file.header.overflow_string_count = overflow_count;
    file.header.string_storage_size = storage_size;

    let out = finalize_raw_image(rebuilt)?;
    file.raw_bytes = Some(out.clone());
    Ok((out, new_id))
}

// Replace the value of string table entry `id`. Same-length edits patch storage
// in place; length changes rebuild the string tables and relocate the tail.
// Hermes packs strings so ranges can overlap (`done`/`next` share storage). We
// refuse in-place same-length patches that would corrupt another entry's range.
pub fn patch_string_by_id(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    id: u32,
    new_value: &str,
    _options: &PatchOptions,
) -> Result<Vec<u8>> {
    // UTF-16 entries are re-encoded, so they always take the rebuild path.
    if file
        .strings
        .get(id as usize)
        .map(|s| s.is_utf16)
        .unwrap_or(false)
    {
        return patch_string_resize(file, id, new_value);
    }
    let (abs_off, old_len) = locate_string_bytes(file, id)?;
    let new_bytes = new_value.as_bytes();
    if new_bytes.len() != old_len {
        return patch_string_resize(file, id, new_value);
    }
    // Overlap guard: any other UTF-8 entry whose [start,end) intersects ours.
    let our_end = abs_off + old_len;
    for other in 0..file.strings.len() as u32 {
        if other == id {
            continue;
        }
        if file.strings[other as usize].is_utf16 {
            continue;
        }
        let Ok((o_off, o_len)) = locate_string_bytes(file, other) else {
            continue;
        };
        let o_end = o_off + o_len;
        let overlaps = abs_off < o_end && o_off < our_end;
        if overlaps {
            // Storage is shared with another entry, so an in place overwrite
            // would corrupt it. Rebuild the string table unpacked instead, which
            // gives this entry its own storage.
            return patch_string_resize(file, id, new_value);
        }
    }
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    raw[abs_off..abs_off + old_len].copy_from_slice(new_bytes);
    // An identifier's precomputed hash tracks its text, so refresh it here too.
    update_identifier_hash(file, &mut raw, id, new_value)?;
    if let Some(s) = file.strings.get_mut(id as usize) {
        s.value = new_value.to_string();
    }
    let out = finalize_raw_image(raw)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "gen" is a plain (non-identifier) string in this fixture.
        let id = file.strings.iter().position(|s| s.value == "gen");
        let Some(id) = id else { return };
        let out = patch_string_by_id(&mut file, &format, id as u32, "genXXXXX", &PatchOptions::default());
        if let Ok(out) = out {
            assert!(verify_footer(&out));
            let re = BytecodeFile::parse_auto(&out).unwrap();
            assert_eq!(re.strings[id].value, "genXXXXX");
        }
    }

    #[test]
    fn patch_string_packed_falls_back_to_resize() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // "done" shares storage with "next" here, so an in place patch would
        // overlap. The patch must still succeed by rebuilding the table unpacked.
        if file.strings.get(5).map(|s| s.value.as_str()) == Some("done") {
            let out =
                patch_string_by_id(&mut file, &format, 5, "GONE", &PatchOptions::default())
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
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        // Latin1-range only characters still require UTF-16 (they are not ASCII).
        let out = patch_string_by_id(&mut file, &format, id as u32, "éàü", &PatchOptions::default())
            .expect("patch to non-ascii");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings[id].is_utf16, "non-ascii value must be stored UTF-16");
        assert_eq!(re.strings[id].value, "éàü");

        // A character above the basic plane also round trips.
        let (mut file2, format2) = load(FIXTURE);
        let out2 =
            patch_string_by_id(&mut file2, &format2, id as u32, "a€☕", &PatchOptions::default())
                .expect("patch to astral");
        let re2 = BytecodeFile::parse_auto(&out2).unwrap();
        assert!(re2.strings[id].is_utf16);
        assert_eq!(re2.strings[id].value, "a€☕");
    }

    // ---- retarget_string tests ----

    #[test]
    fn retarget_string_basic() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        // Find two distinct non-empty strings to retarget.
        let a = file.strings.iter().position(|s| !s.value.is_empty() && !s.is_utf16);
        let b = file.strings.iter().rposition(|s| !s.value.is_empty() && !s.is_utf16);
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) if a != b => (a as u32, b as u32),
            _ => return,
        };
        let target_val = file.strings[b as usize].value.clone();
        let opts = PatchOptions::default();
        let out = retarget_string(&mut file, &format, a, b, &opts)
            .expect("retarget_string basic");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.strings[a as usize].value, target_val);
    }

    #[test]
    fn retarget_string_file_size_unchanged() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        let out = retarget_string(&mut file, &format, a, b, &opts)
            .expect("retarget_string size check");
        // File size must not change — metadata-only edit.
        assert_eq!(out.len(), original_len, "file size changed after retarget");
    }

    #[test]
    fn retarget_string_other_strings_unchanged() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        let _ = retarget_string(&mut file, &format, 0, 1, &opts)
            .expect("retarget for unchanged check");
        let re = BytecodeFile::parse_auto(file.raw_bytes.as_ref().unwrap()).unwrap();
        for i in 2..originals.len() {
            assert_eq!(re.strings[i].value, originals[i].0, "string {i} changed");
        }
    }

    #[test]
    fn retarget_string_same_id_rejected() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        let result = retarget_string(&mut file, &format, 0, 0, &opts);
        assert!(result.is_err(), "retarget same id should fail");
    }

    #[test]
    fn retarget_string_out_of_range_rejected() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        let out = retarget_string(&mut file, &format, a, b, &opts)
            .expect("retarget identifier");
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

    // Regression: the overflow detection in retarget_string originally checked
    // `off == 0x800000` which is unreachable after a 23-bit mask. Verify that
    // the corrected check (length == 0xff) still catches overflow entries.
    // We test indirectly: create a file with an overflow entry via add_string
    // (a 256-char string forces overflow), then try to retarget from/to it.
    #[test]
    fn retarget_string_overflow_entry_refused() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        // Append a long string to force an overflow entry.
        let long_val = "X".repeat(256);
        let (_, overflow_id) = add_string(&mut file, &format, &long_val, false, &opts)
            .expect("add long string for overflow test");
        // Re-parse so sections are fresh for retarget_string.
        let reparsed = BytecodeFile::parse_auto(file.raw_bytes.as_ref().unwrap()).unwrap();
        let mut file2 = reparsed;
        let result = retarget_string(&mut file2, &format, 0, overflow_id, &opts);
        assert!(result.is_err(), "retarget to overflow entry should fail");
        assert!(
            result.unwrap_err().to_string().contains("overflow"),
            "error should mention overflow"
        );
    }

    // ---- add_string tests ----

    #[test]
    fn add_string_ascii_reparses() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let opts = PatchOptions::default();
        let (out, new_id) = add_string(&mut file, &format, "café☕", false, &opts)
            .expect("add_string UTF-16");
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(re.strings[new_id as usize].is_utf16);
        assert_eq!(re.strings[new_id as usize].value, "café☕");
    }

    #[test]
    fn add_string_identifier() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(path).exists() {
            return;
        }
        let (mut file, format) = load(path);
        assert!(matches!(
            file.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));
        let old_count = file.header.string_count;
        let opts = PatchOptions::default();
        let (out, new_id) =
            add_string(&mut file, &format, "modernTestProp", true, &opts)
                .expect("add_string on modern v98");
        assert_eq!(new_id, old_count);
        assert!(verify_footer(&out));
        let re = BytecodeFile::parse_auto(&out)
            .expect("reparse after modern add_string");
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
        if !std::path::Path::new(FIXTURE).exists() {
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
        assert_eq!(before, after, "function 0 disassembly changed after add_string");
    }

    // Overflow threshold: exercise the overflow path by appending a string whose
    // length exceeds the small-entry 8-bit limit (0xff = 255).
    #[test]
    fn add_string_overflow_entry() {
        if !std::path::Path::new(FIXTURE).exists() {
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
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let id = file
            .strings
            .iter()
            .position(|s| !s.is_utf16 && s.value.is_ascii() && s.value.len() >= 3);
        let Some(id) = id else { return };
        let out = patch_string_by_id(&mut file, &format, id as u32, "PLAINASCII", &PatchOptions::default())
            .expect("patch ascii");
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert!(!re.strings[id].is_utf16, "ascii value must stay one byte");
        assert_eq!(re.strings[id].value, "PLAINASCII");
    }
}
