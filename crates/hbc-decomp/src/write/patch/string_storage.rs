// How string storage is laid out when a write rebuilds it, and the check that
// every entry still reads back as the text it should -- plan_guides/06_write/
// string_packing/PLAN.md P0 and P1.
//
// Layout follows upstream's `ConsecutiveStringStorage`: all ASCII strings, then
// a pad to 2 bytes, then all UTF-16 strings, so a UTF-16 string never starts at
// an odd offset. Within each half, `Packed` shares storage between a string and
// any string it is a suffix of (`done` inside `undone`); `Unpacked` gives every
// string its own bytes, which is upstream's `fastPackStrings`.
//
// Pinned ids always get private bytes that no other entry reads, and no other
// string is absorbed into them. A same-length patch overwrites an entry in
// place only when its bytes are its own; the entry a rebuild was just made for
// is pinned so the next same-length patch of it does not bounce to another
// rebuild.

use std::collections::BTreeSet;

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::write::serialize::section_offset;

/// One string as it is stored: its bytes, the table's length field (characters
/// for ASCII, code units for UTF-16) and its encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoredString {
    pub bytes: Vec<u8>,
    pub len_field: u32,
    pub is_utf16: bool,
}

impl StoredString {
    /// Hermes stores a string with one byte per character only when it is pure
    /// ASCII, and as UTF-16 for anything with a non-ASCII character.
    pub fn encode(value: &str) -> Self {
        if value.bytes().any(|b| b > 0x7f) {
            let units: Vec<u16> = value.encode_utf16().collect();
            Self {
                bytes: units.iter().flat_map(|u| u.to_le_bytes()).collect(),
                len_field: units.len() as u32,
                is_utf16: true,
            }
        } else {
            Self {
                bytes: value.as_bytes().to_vec(),
                len_field: value.len() as u32,
                is_utf16: false,
            }
        }
    }
}

// Per-string storage location read straight from the small/overflow tables.
pub(super) struct StrLoc {
    pub storage_off: u32,
    pub len_field: u32,
    pub is_utf16: bool,
}

const UTF16: u32 = 0x1;
const OFF_SHIFT: u32 = 1;
const OFF_MASK: u32 = 0x7f_ffff;
const LEN_SHIFT: u32 = 24;
const LEN_MASK: u32 = 0xff;
const LEN_OVERFLOW: u32 = 0xff;
const OFF_OVERFLOW: u32 = 0x800000;

// Read the storage offset + length field of every string from the raw tables.
pub(super) fn read_all_string_locs(file: &BytecodeFile) -> Result<Vec<StrLoc>> {
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let small_off = section_offset(file, "small_string_table")
        .ok_or_else(|| Error::Write("small_string_table section missing".into()))?
        as usize;
    let overflow_off = section_offset(file, "overflow_string_table").map(|o| o as usize);

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
            let ov_base =
                overflow_off.ok_or_else(|| Error::Write("overflow string table missing".into()))?;
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

fn byte_len(loc: &StrLoc) -> usize {
    loc.len_field as usize * if loc.is_utf16 { 2 } else { 1 }
}

/// Every string of `file`, as stored, read through its own tables.
pub(super) fn current_strings(file: &BytecodeFile) -> Result<Vec<StoredString>> {
    let raw = file
        .raw_bytes
        .as_ref()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let storage_off = section_offset(file, "string_storage")
        .ok_or_else(|| Error::Write("string_storage section missing".into()))?
        as usize;
    read_all_string_locs(file)?
        .iter()
        .map(|loc| {
            let start = storage_off + loc.storage_off as usize;
            let bytes = raw
                .get(start..start + byte_len(loc))
                .ok_or_else(|| Error::Write("string storage OOB".into()))?;
            Ok(StoredString {
                bytes: bytes.to_vec(),
                len_field: loc.len_field,
                is_utf16: loc.is_utf16,
            })
        })
        .collect()
}

/// How a rebuild lays out string storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Packing {
    Packed,
    Unpacked,
}

/// A rebuilt string table: the small and overflow tables and the storage.
pub(super) struct StringLayout {
    pub small: Vec<u32>,
    pub overflow: Vec<(u32, u32)>,
    pub storage: Vec<u8>,
}

/// Lay out `strings` (indexed by id) into fresh storage and tables.
pub(super) fn lay_out(
    strings: &[StoredString],
    pinned: &BTreeSet<usize>,
    packing: Packing,
) -> Result<StringLayout> {
    let mut storage = Vec::new();
    let mut offsets = vec![0u32; strings.len()];
    for utf16 in [false, true] {
        let ids: Vec<usize> = (0..strings.len())
            .filter(|&i| strings[i].is_utf16 == utf16)
            .collect();
        if ids.is_empty() {
            continue;
        }
        if utf16 && !storage.len().is_multiple_of(2) {
            storage.push(0);
        }
        pack_group(strings, &ids, pinned, packing, &mut storage, &mut offsets);
    }

    let mut small = Vec::with_capacity(strings.len());
    let mut overflow = Vec::new();
    for (s, &off) in strings.iter().zip(&offsets) {
        let utf16 = s.is_utf16 as u32;
        // An offset or length the small entry cannot hold goes in the overflow
        // table; the small entry then stores the overflow index and length 0xff.
        if off >= OFF_OVERFLOW || s.len_field >= LEN_OVERFLOW {
            let ov_index = overflow.len() as u32;
            overflow.push((off, s.len_field));
            small.push((LEN_OVERFLOW << LEN_SHIFT) | ((ov_index & OFF_MASK) << OFF_SHIFT) | utf16);
        } else {
            small.push(
                ((s.len_field & LEN_MASK) << LEN_SHIFT) | ((off & OFF_MASK) << OFF_SHIFT) | utf16,
            );
        }
    }
    u32::try_from(storage.len())
        .map_err(|_| Error::Write("string storage larger than 4 GiB".into()))?;
    Ok(StringLayout {
        small,
        overflow,
        storage,
    })
}

// Append one encoding's strings to `storage`, recording each id's offset.
//
// Packed: sort by reversed bytes, *descending*, so every string comes straight
// after a string it is a suffix of, if there is one -- strings sharing a reversed
// prefix sort together and the longer one first. Sorting ascending puts each
// suffix before its container, absorbs nothing, and still produces valid output,
// so the saving is asserted in the tests rather than trusted. For UTF-16 both
// lengths are even, so a byte suffix is a code-unit suffix and stays 2-aligned.
fn pack_group(
    strings: &[StoredString],
    ids: &[usize],
    pinned: &BTreeSet<usize>,
    packing: Packing,
    storage: &mut Vec<u8>,
    offsets: &mut [u32],
) {
    // Write `id` out in full at the end of storage.
    let fresh = |storage: &mut Vec<u8>, offsets: &mut [u32], id: usize| {
        offsets[id] = storage.len() as u32;
        storage.extend_from_slice(&strings[id].bytes);
    };
    let (shared, private): (Vec<usize>, Vec<usize>) =
        ids.iter().partition(|id| !pinned.contains(id));
    match packing {
        Packing::Unpacked => shared.iter().for_each(|&id| fresh(storage, offsets, id)),
        Packing::Packed => {
            let mut order = shared;
            order.sort_by(|&a, &b| {
                strings[b]
                    .bytes
                    .iter()
                    .rev()
                    .cmp(strings[a].bytes.iter().rev())
            });
            // The last string written out in full; anything that is its suffix
            // is placed at its tail instead of written again.
            let mut container: Option<usize> = None;
            for id in order {
                let bytes = &strings[id].bytes;
                match container {
                    Some(c) if strings[c].bytes.ends_with(bytes) => {
                        offsets[id] = offsets[c] + (strings[c].bytes.len() - bytes.len()) as u32;
                    }
                    _ => {
                        fresh(storage, offsets, id);
                        container = Some(id);
                    }
                }
            }
        }
    }
    for id in private {
        fresh(storage, offsets, id);
    }
}

/// P0: every entry of the image `after` must read back, through its own tables,
/// as exactly `expected` -- same encoding, same length field, same bytes -- and
/// every pinned id must own its bytes outright. One pass over the table; run
/// after every string write, because a packing or table bug otherwise yields a
/// file that parses, runs, and shows one wrong word somewhere.
pub(super) fn validate(
    after: &BytecodeFile,
    expected: &[StoredString],
    pinned: &BTreeSet<usize>,
) -> Result<()> {
    let got = current_strings(after)?;
    if got.len() != expected.len() {
        return Err(Error::Write(format!(
            "string validation: {} entries written, {} expected",
            got.len(),
            expected.len()
        )));
    }
    if let Some(id) = (0..got.len()).find(|&i| got[i] != expected[i]) {
        return Err(Error::Write(format!(
            "string validation: entry {id} reads back as {:?} (utf16={}, len={}) \
             but should be {:?} (utf16={}, len={})",
            String::from_utf8_lossy(&got[id].bytes),
            got[id].is_utf16,
            got[id].len_field,
            String::from_utf8_lossy(&expected[id].bytes),
            expected[id].is_utf16,
            expected[id].len_field,
        )));
    }
    if pinned.is_empty() {
        return Ok(());
    }
    let locs = read_all_string_locs(after)?;
    let range = |i: usize| {
        let s = locs[i].storage_off as usize;
        (s, s + byte_len(&locs[i]))
    };
    for &p in pinned {
        let (ps, pe) = range(p);
        if ps == pe {
            continue;
        }
        // An empty entry reads no bytes, so it cannot share any.
        if let Some(other) = (0..locs.len()).find(|&i| {
            let (s, e) = range(i);
            i != p && s < e && s < pe && ps < e
        }) {
            return Err(Error::Write(format!(
                "string validation: pinned entry {p} shares storage with entry {other}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ascii(s: &str) -> StoredString {
        StoredString::encode(s)
    }

    // Read entry `i` back out of a layout the way the engine does.
    fn read_back(l: &StringLayout, strings: &[StoredString], i: usize) -> Vec<u8> {
        let e = l.small[i];
        let (off, len) = if (e >> LEN_SHIFT) == LEN_OVERFLOW {
            l.overflow[((e >> OFF_SHIFT) & OFF_MASK) as usize]
        } else {
            ((e >> OFF_SHIFT) & OFF_MASK, e >> LEN_SHIFT)
        };
        let n = len as usize * if strings[i].is_utf16 { 2 } else { 1 };
        l.storage[off as usize..off as usize + n].to_vec()
    }

    fn assert_reads_back(l: &StringLayout, strings: &[StoredString]) {
        for (i, s) in strings.iter().enumerate() {
            assert_eq!(read_back(l, strings, i), s.bytes, "entry {i}");
            assert_eq!(l.small[i] & UTF16 != 0, s.is_utf16, "entry {i} encoding");
        }
    }

    // The sort direction is the whole algorithm: backwards, nothing is absorbed
    // and the output is still valid, so the saving itself is what is asserted.
    #[test]
    fn suffixes_share_their_container_storage() {
        let strings = vec![
            ascii("done"),
            ascii("undone"),
            ascii("one"),
            ascii("x"),
            ascii(""),
        ];
        let packed = lay_out(&strings, &BTreeSet::new(), Packing::Packed).unwrap();
        let unpacked = lay_out(&strings, &BTreeSet::new(), Packing::Unpacked).unwrap();
        assert_reads_back(&packed, &strings);
        assert_reads_back(&unpacked, &strings);
        assert_eq!(unpacked.storage.len(), 4 + 6 + 3 + 1);
        assert_eq!(
            packed.storage.len(),
            6 + 1,
            "done and one live inside undone"
        );
    }

    #[test]
    fn a_pinned_entry_gets_bytes_no_other_entry_reads() {
        let strings = vec![ascii("done"), ascii("undone"), ascii("one")];
        let pinned = BTreeSet::from([0]);
        let l = lay_out(&strings, &pinned, Packing::Packed).unwrap();
        assert_reads_back(&l, &strings);
        // undone + one inside it, then done on its own.
        assert_eq!(l.storage.len(), 6 + 4);
        let off = |i: usize| ((l.small[i] >> OFF_SHIFT) & OFF_MASK) as usize;
        assert_eq!(
            off(0),
            6,
            "the pinned entry is written after the shared ones"
        );
        assert!(off(2) < 6, "one is still absorbed into undone");
    }

    // Upstream lays out ASCII, pads to 2, then UTF-16, so no UTF-16 string
    // starts at an odd offset; the rebuild used to interleave them.
    #[test]
    fn utf16_strings_follow_the_ascii_ones_two_byte_aligned() {
        let strings = vec![ascii("élan"), ascii("abc"), ascii("lan"), ascii("ün")];
        for packing in [Packing::Packed, Packing::Unpacked] {
            let l = lay_out(&strings, &BTreeSet::new(), packing).unwrap();
            assert_reads_back(&l, &strings);
            for (i, s) in strings.iter().enumerate() {
                let off = (l.small[i] >> OFF_SHIFT) & OFF_MASK;
                if s.is_utf16 {
                    assert_eq!(off % 2, 0, "{packing:?}: entry {i} at odd offset {off}");
                    assert!(
                        off >= 6,
                        "{packing:?}: UTF-16 entry {i} before the ASCII ones"
                    );
                }
            }
        }
    }

    #[test]
    fn a_long_string_takes_an_overflow_entry() {
        let strings = vec![ascii(&"a".repeat(300)), ascii("b")];
        let l = lay_out(&strings, &BTreeSet::new(), Packing::Packed).unwrap();
        assert_eq!(l.overflow.len(), 1);
        assert_reads_back(&l, &strings);
    }

    fn fixture() -> Option<BytecodeFile> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plain.v96.hbc");
        let bytes = std::fs::read(path).ok()?;
        Some(BytecodeFile::parse_auto(&bytes).unwrap())
    }

    // P0's acceptance: corrupt one entry and watch the check fire.
    #[test]
    fn validation_catches_an_entry_that_reads_back_wrong() {
        let file = fixture().expect("committed fixture");
        let expected = current_strings(&file).unwrap();
        validate(&file, &expected, &BTreeSet::new()).expect("an untouched file validates");

        // Point entry 1 at entry 0's bytes, in the image itself.
        let mut raw = file.raw_bytes.clone().unwrap();
        let small = section_offset(&file, "small_string_table").unwrap() as usize;
        let e0: [u8; 4] = raw[small..small + 4].try_into().unwrap();
        raw[small + 4..small + 8].copy_from_slice(&e0);
        let corrupt = BytecodeFile::parse_auto(&raw).unwrap();
        let err = validate(&corrupt, &expected, &BTreeSet::new())
            .expect_err("a wrong entry must be caught")
            .to_string();
        assert!(err.contains("entry 1"), "{err}");

        // And a pinned entry that shares bytes is caught too.
        let err = validate(
            &corrupt,
            &current_strings(&corrupt).unwrap(),
            &BTreeSet::from([1]),
        )
        .expect_err("a shared pinned entry must be caught")
        .to_string();
        assert!(err.contains("pinned entry 1"), "{err}");
    }
}
