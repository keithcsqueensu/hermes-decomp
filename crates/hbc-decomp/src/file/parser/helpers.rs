use super::buffer::read_buffer_series;
use crate::error::Result;
use crate::file::{BytecodeFile, LiteralValue};

// Get BigInt value at the given index.
// Returns the string representation of the BigInt.
pub fn bigint_at(file: &BytecodeFile, bigint_id: u32) -> Option<String> {
    let entry = file.big_int_table.get(bigint_id as usize)?;
    let start = entry.offset as usize;
    let end = start.checked_add(entry.length as usize)?;

    if end > file.big_int_storage.len() {
        return None;
    }

    let bytes = &file.big_int_storage[start..end];
    // BigInt is stored as a sequence of bytes representing the value

    if bytes.is_empty() {
        return Some("0".to_string());
    }

    // Check if it's a small value that fits in i64
    if bytes.len() <= 8 {
        let mut value: i64 = 0;
        let is_negative = bytes.last().map(|b| b & 0x80 != 0).unwrap_or(false);

        for (i, &byte) in bytes.iter().enumerate() {
            value |= (byte as i64) << (i * 8);
        }

        // Sign extend if negative
        if is_negative && bytes.len() < 8 {
            let shift = bytes.len() * 8;
            value |= !0i64 << shift;
        }

        return Some(value.to_string());
    }

    // Above 64 bits, decode the little-endian two's-complement value properly
    // rather than printing the raw bytes.
    //
    // The old form emitted reversed hex with no sign handling, so a large
    // *negative* BigInt came out as an unsigned hex blob -- and `dump --kind
    // big-int` presents this string as the value.
    Some(twos_complement_le_to_decimal(bytes))
}

// Decimal string for a little-endian two's-complement integer of any width.
//
// BigInts are rare and short here, so schoolbook base-1e9 long division is
// plenty and avoids taking a bignum dependency for one call site.
fn twos_complement_le_to_decimal(bytes: &[u8]) -> String {
    let negative = bytes.last().is_some_and(|b| b & 0x80 != 0);
    let mut magnitude = bytes.to_vec();
    if negative {
        // Two's complement -> magnitude: invert, then add one.
        for b in magnitude.iter_mut() {
            *b = !*b;
        }
        let mut carry = 1u16;
        for b in magnitude.iter_mut() {
            let v = *b as u16 + carry;
            *b = v as u8;
            carry = v >> 8;
            if carry == 0 {
                break;
            }
        }
    }

    let digits = to_decimal_le(&magnitude);
    // Guard the sign so an all-zero magnitude can never render as "-0".
    if negative && digits != "0" {
        format!("-{digits}")
    } else {
        digits
    }
}

// Decimal string for a little-endian unsigned byte magnitude.
//
// Repeated division by 1e9: each pass walks the bytes most-significant first,
// carrying the remainder, and yields nine decimal digits at a time.
fn to_decimal_le(bytes: &[u8]) -> String {
    let mut limbs: Vec<u8> = bytes.to_vec();
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    if limbs.is_empty() {
        return "0".to_string();
    }

    const CHUNK: u64 = 1_000_000_000;
    let mut chunks: Vec<u32> = Vec::new();
    while !limbs.is_empty() {
        let mut rem: u64 = 0;
        for byte in limbs.iter_mut().rev() {
            let cur = (rem << 8) | *byte as u64;
            *byte = (cur / CHUNK) as u8;
            rem = cur % CHUNK;
        }
        chunks.push(rem as u32);
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
    }

    let mut out = chunks.pop().unwrap_or(0).to_string();
    for c in chunks.iter().rev() {
        out.push_str(&format!("{c:09}"));
    }
    out
}

pub fn read_array_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if !file.array_buffer.is_empty() {
        read_buffer_series(file, &file.array_buffer, offset, count)
    } else {
        read_buffer_series(file, &file.literal_value_buffer, offset, count)
    }
}

pub fn read_key_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    read_buffer_series(file, &file.obj_key_buffer, offset, count)
}

pub fn read_value_buffer_series(
    file: &BytecodeFile,
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if !file.obj_value_buffer.is_empty() {
        read_buffer_series(file, &file.obj_value_buffer, offset, count)
    } else {
        read_buffer_series(file, &file.literal_value_buffer, offset, count)
    }
}

#[cfg(test)]
mod tests {
    use super::twos_complement_le_to_decimal as dec;

    #[test]
    fn decodes_values_wider_than_64_bits() {
        // 2^64, as nine little-endian bytes.
        assert_eq!(dec(&[0, 0, 0, 0, 0, 0, 0, 0, 1]), "18446744073709551616");
        // 2^71 - 1: eight 0xff bytes then 0x7f (sign bit clear, so positive).
        let mut p = vec![0xffu8; 8];
        p.push(0x7f);
        assert_eq!(dec(&p), "2361183241434822606847");
        // All bits set is -1 at any width.
        assert_eq!(dec(&[0xff; 9]), "-1");
        // -(2^64): the old code printed this as an unsigned hex blob.
        assert_eq!(dec(&[0, 0, 0, 0, 0, 0, 0, 0, 0xff]), "-18446744073709551616");
        // Zero, and a zero magnitude that must not acquire a sign.
        assert_eq!(dec(&[0; 9]), "0");
        assert_eq!(dec(&[0; 16]), "0");
    }

    #[test]
    fn agrees_with_the_i64_path_where_they_overlap() {
        // Same values, once through the wide decoder and once through i64, so the
        // two branches of `bigint_at` cannot drift apart.
        for v in [0i64, 1, -1, 255, -255, i32::MAX as i64, i64::MIN, i64::MAX] {
            let le = v.to_le_bytes();
            assert_eq!(dec(&le), v.to_string(), "value {v}");
        }
    }
}
