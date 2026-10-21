use crate::error::{Error, Result};
use crate::file::structure::{
    ShapeTableEntry, StringKindEntry, StringKindType, StringTableEntry, TableEntry,
};
use crate::format::SmallStringEntry;
use crate::io::ByteReader;

// --- String-kind entry bitfields (one u32 per kind run) ---
// The high bit selects the kind (0 = String, 1 = Identifier); the low 31 bits
// hold the run length (number of consecutive strings of that kind).
// High bit of a string-kind entry: set => Identifier, clear => String.
const STRING_KIND_TYPE_BIT: u32 = 1 << 31;
// Mask for the run-length portion (low 31 bits) of a string-kind entry.
const STRING_KIND_COUNT_MASK: u32 = 0x7fff_ffff;

// Small string-table entries (one u32 per string) are decoded by
// `crate::format::SmallStringEntry`, which every write path uses too.

pub fn parse_string_kinds(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<StringKindEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let raw = reader.read_u32()?;
        let kind = if (raw & STRING_KIND_TYPE_BIT) == 0 {
            StringKindType::String
        } else {
            StringKindType::Identifier
        };
        let count = raw & STRING_KIND_COUNT_MASK;
        entries.push(StringKindEntry { kind, count });
    }
    Ok(entries)
}

pub fn parse_overflow_string_table(
    reader: &mut ByteReader<'_>,
    count: u32,
) -> Result<Vec<(u32, u32)>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let offset = reader.read_u32()?;
        let length = reader.read_u32()?;
        entries.push((offset, length));
    }
    Ok(entries)
}

pub fn parse_u32_vec(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<u32>> {
    let mut values = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        values.push(reader.read_u32()?);
    }
    Ok(values)
}

pub fn parse_table_entries(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<TableEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let offset = reader.read_u32()?;
        let length = reader.read_u32()?;
        entries.push(TableEntry { offset, length });
    }
    Ok(entries)
}

pub fn parse_shape_table(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<ShapeTableEntry>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let key_buffer_offset = reader.read_u32()?;
        let num_props = reader.read_u32()?;
        entries.push(ShapeTableEntry {
            key_buffer_offset,
            num_props,
        });
    }
    Ok(entries)
}

pub fn parse_pair_table(reader: &mut ByteReader<'_>, count: u32) -> Result<Vec<(u32, u32)>> {
    let mut entries = Vec::with_capacity(reader.capacity_hint(count as usize));
    for _ in 0..count {
        let first = reader.read_u32()?;
        let second = reader.read_u32()?;
        entries.push((first, second));
    }
    Ok(entries)
}

// Decode the string table.
//
// Returns the entries plus the count of entries whose storage offset/length fell
// outside the string storage and decoded to an `<invalid utf8>` / `<invalid
// utf16>` placeholder. Those placeholders travel in-band, inside the string
// value itself, so downstream they are indistinguishable from real content --
// the count is the only way a caller can tell that some of what it is reading is
// not what the file says. Zero on every intact bundle.
pub fn decode_string_table(
    string_count: u32,
    kinds: &[StringKindEntry],
    small_entries: &[u32],
    overflow_entries: &[(u32, u32)],
    storage: &[u8],
) -> Result<(Vec<StringTableEntry>, usize)> {
    // A corrupt or mis-read header can carry a string_count far larger than the
    // actual small-entry table; clamp so both the allocations and the loops
    // below stay bounded (for valid files the two are equal anyway).
    let count = (string_count as usize).min(small_entries.len());
    let mut expanded_kinds = Vec::with_capacity(count);
    let mut kind_index = 0usize;
    let mut remaining = kinds.first().map(|k| k.count).unwrap_or(0);

    for _ in 0..count {
        if remaining == 0 {
            kind_index += 1;
            remaining = kinds.get(kind_index).map(|k| k.count).unwrap_or(0);
        }
        let kind = kinds
            .get(kind_index)
            .map(|k| k.kind)
            .unwrap_or(StringKindType::String);
        expanded_kinds.push(kind);
        remaining = remaining.saturating_sub(1);
    }

    let mut strings = Vec::with_capacity(count);

    let mut invalid = 0usize;

    for i in 0..count {
        let raw = small_entries
            .get(i)
            .copied()
            .ok_or_else(|| Error::Parse("string table entry missing".to_string()))?;

        // An overflowed entry names its record by index, as the engine reads it;
        // records need not be in table order, nor used once each.
        let entry = SmallStringEntry::decode(raw);
        let is_utf16 = entry.is_utf16;
        let (offset, length) = entry.resolve(overflow_entries).ok_or_else(|| {
            Error::Parse(format!(
                "string {i}: overflow record {} missing ({} records)",
                entry.offset,
                overflow_entries.len()
            ))
        })?;

        let value = if is_utf16 {
            // saturating math: an overflowing offset/length just fails the
            // bounds check below and yields "<invalid>" instead of panicking.
            let byte_len = (length as usize).saturating_mul(2);
            let start = offset as usize;
            let end = start.saturating_add(byte_len);

            if start >= storage.len() || end > storage.len() {
                invalid += 1;
                "<invalid utf16>".to_string()
            } else {
                let slice = &storage[start..end];
                let mut units = Vec::with_capacity(length as usize);
                for chunk in slice.as_chunks::<2>().0 {
                    units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
                }
                String::from_utf16_lossy(&units)
            }
        } else {
            let start = offset as usize;
            let end = start.saturating_add(length as usize);
            if start >= storage.len() || end > storage.len() {
                invalid += 1;
                "<invalid utf8>".to_string()
            } else {
                String::from_utf8_lossy(&storage[start..end]).to_string()
            }
        };

        let is_identifier = expanded_kinds
            .get(i)
            .copied()
            .unwrap_or(StringKindType::String)
            == StringKindType::Identifier;

        strings.push(StringTableEntry {
            value,
            is_utf16,
            is_identifier,
        });
    }

    Ok((strings, invalid))
}
