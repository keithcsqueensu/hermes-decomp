// Keep a function's debug line table correct across an *insertion* into its body.
//
// R24 in one sentence: a location stream stores bytecode addresses within a
// function as SLEB128 deltas, and resizing the body rewrites none of them, so every
// location past the edit maps to the wrong instruction — silently, because the
// stream still decodes and still terminates. P0 guarded that by refusing. This is
// P2: for the edits where "where did the bytes go in" is a well-defined question,
// fix the addresses instead of refusing.
//
// **Only insertions can be relocated, and that is not a limitation of this code.**
// `inject-stub` adds instructions at a known point and leaves everything else in
// place, so old address A maps to A or A+delta and the line table can follow. A
// wholesale body replacement (`asm`, `patch-function`) has no such mapping: the new
// body is different code, and there is no answer to "which new address does old
// address A correspond to". Those keep P0's refusal, because relocating them would
// mean inventing a correspondence.
//
// The trick that keeps this small: addresses are *deltas*. Shifting every address
// at or past the insertion point means adding `delta` to exactly **one** delta —
// the first entry that crosses the point — because every later entry is relative to
// it. Nothing else in the stream is touched, so statement deltas, envReg, envIdx
// and the conditional fields survive without this code having to understand them.

use crate::debug::{DebugLayout, StreamEncoding};
use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::format::{FunctionHeader, FLAG_HAS_DEBUG_INFO, FLAG_HAS_EXCEPTION_HANDLER};
use crate::io::ByteReader;

/// Where a function's `DebugOffsets.sourceLocations` field lives in the image, and
/// what it currently says.
struct OffsetsSlot {
    /// Byte position of the `sourceLocations` u32 in the function info area.
    at: usize,
    /// Its value: the stream's offset within the debug *data* region.
    stream_offset: u32,
}

/// One entry's address delta, located in the stream.
struct EntryDelta {
    /// Byte position of the SLEB128 address delta.
    at: usize,
    /// Its encoded length, which re-encoding may change.
    len: usize,
    /// The address this entry lands on, after applying the delta.
    address: i64,
}

fn encode_sleb128(mut value: i64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value as u8) & 0x7f;
        value >>= 7;
        // The sign bit of the byte must agree with the remaining value, or the
        // decoder will sign-extend the wrong way.
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            return out;
        }
    }
}

/// Read every function's `sourceLocations` slot position, walking the info area the
/// way upstream's `serializeFunctionInfo` writes it: an optional exception-handler
/// table, then `DebugOffsets`, each 4-byte aligned.
fn source_location_slots(bytes: &[u8], headers: &[FunctionHeader]) -> Vec<(u32, OffsetsSlot)> {
    let mut out = Vec::new();
    for fh in headers {
        let (info_offset, flags, id) = match fh {
            FunctionHeader::Legacy(h) => (h.info_offset as usize, h.flags, h.function_id),
            FunctionHeader::Modern(h) => (h.info_offset as usize, h.flags, h.function_id),
        };
        if flags & FLAG_HAS_DEBUG_INFO == 0 || info_offset == 0 {
            continue;
        }
        let mut pos = info_offset.saturating_add(3) & !3;
        if flags & FLAG_HAS_EXCEPTION_HANDLER != 0 {
            let Some(raw) = bytes.get(pos..pos + 4) else {
                continue;
            };
            let count = u32::from_le_bytes(raw.try_into().unwrap()) as usize;
            if count > 1000 {
                continue;
            }
            pos = pos.saturating_add(4).saturating_add(count.saturating_mul(12));
            pos = pos.saturating_add(3) & !3;
        }
        let Some(raw) = bytes.get(pos..pos + 4) else {
            continue;
        };
        let stream_offset = u32::from_le_bytes(raw.try_into().unwrap());
        if stream_offset != u32::MAX {
            out.push((id, OffsetsSlot { at: pos, stream_offset }));
        }
    }
    out
}

/// Walk one stream, recording where each entry's address delta sits and the address
/// it produces. Mirrors `DebugInfo::parse_location_stream`, which is the reader; the
/// two must agree about the field layout, and `upstream_pin` pins that layout.
fn scan_stream(data: &[u8], start: usize, enc: StreamEncoding) -> Result<Vec<EntryDelta>> {
    let slice = data
        .get(start..)
        .ok_or_else(|| Error::Write("debug stream offset out of range".into()))?;
    let mut reader = ByteReader::new(slice);

    // Prologue: functionIndex, line, column, and envIdx from v98 on.
    let prologue = match enc {
        StreamEncoding::Legacy => 3,
        StreamEncoding::Modern => 4,
    };
    for _ in 0..prologue {
        reader.read_sleb128()?;
    }

    let mut out = Vec::new();
    let mut address: i64 = 0;
    loop {
        let at = start + reader.position();
        let Ok(address_delta) = reader.read_sleb128() else {
            break;
        };
        let len = start + reader.position() - at;
        if address_delta == -1 {
            break;
        }
        address += address_delta;
        out.push(EntryDelta { at, len, address });

        match enc {
            StreamEncoding::Legacy => {
                let ldelta = reader.read_sleb128()?;
                reader.read_sleb128()?; // column
                reader.read_sleb128()?; // scopeAddress
                reader.read_sleb128()?; // envReg
                if ldelta & 1 != 0 {
                    reader.read_sleb128()?; // statement
                }
            }
            StreamEncoding::Modern => {
                let ldelta = reader.read_sleb128()?;
                if ldelta & 1 == 0 {
                    // No location, but the address above still moved.
                    continue;
                }
                reader.read_sleb128()?; // column
                if ldelta & 2 != 0 {
                    reader.read_sleb128()?; // statement
                }
                if ldelta & 4 != 0 {
                    reader.read_sleb128()?; // envIdx
                }
            }
        }
    }
    Ok(out)
}

/// Shift every source location at or after `insert_at` by `delta`, for one function.
///
/// `buf` must be a finalized image whose *body* edit has already been applied — the
/// debug section is read at `file.header.debug_info_offset`, which by then points at
/// its post-edit position. Returns the image with the line table corrected; the
/// caller commits it.
pub fn relocate_locations_for_insertion(
    file: &BytecodeFile,
    mut buf: Vec<u8>,
    function_id: u32,
    insert_at: u32,
    delta: i64,
) -> Result<Vec<u8>> {
    if delta == 0 {
        return Ok(buf);
    }
    let Some(layout) = DebugLayout::for_version(file.header.version) else {
        // A version whose debug section this crate does not model. Refuse rather
        // than leave a silently wrong line table -- the caller asked for an edit
        // that needs relocation, and pretending it happened is the R24 failure.
        return Err(Error::Write(format!(
            "cannot relocate debug info at bytecode version {}: its debug layout is \
             not modelled. Pass --allow-stale-debug-info to proceed and discard this \
             function's line numbers.",
            file.header.version
        )));
    };
    let section = file.header.debug_info_offset as usize;
    if section == 0 || section >= buf.len() {
        return Ok(buf);
    }

    // Where the debug *data* begins, and where the header fields that describe it
    // live. Same arithmetic as the reader, in image coordinates.
    let mut reader = ByteReader::new(&buf[section..]);
    let filename_count = reader.read_u32()?;
    let filename_storage = reader.read_u32()?;
    let file_regions = reader.read_u32()?;
    let interior = if layout.has_lexical_regions { 3 } else { 0 };
    let data_start = section
        + layout.header_size as usize
        + filename_count as usize * 8
        + filename_storage as usize
        + file_regions as usize * 12;

    let slots = source_location_slots(&buf, &file.function_headers);
    let Some((_, target)) = slots.iter().find(|(id, _)| *id == function_id) else {
        // The function has no stream; nothing to relocate.
        return Ok(buf);
    };
    let stream_start = data_start + target.stream_offset as usize;

    let entries = scan_stream(&buf, stream_start, layout.stream)?;
    let Some(entry) = entries.iter().find(|e| e.address >= insert_at as i64) else {
        // Every location precedes the insertion point, so none of them moved.
        return Ok(buf);
    };

    // The one delta that changes. Everything after it is relative to it and stays.
    let old = {
        let mut r = ByteReader::new(&buf[entry.at..]);
        r.read_sleb128()?
    };
    let encoded = encode_sleb128(old + delta);
    let growth = encoded.len() as i64 - entry.len as i64;

    buf.splice(entry.at..entry.at + entry.len, encoded);

    if growth != 0 {
        // The stream got longer or shorter, so the debug data region did too.
        // Three things now describe the wrong sizes: the header's data size, the
        // interior region offsets that follow the location data (v96 only), and
        // every other function's stream offset past this one.
        let size_pos = section + layout.header_size as usize - 4;
        let size = u32::from_le_bytes(buf[size_pos..size_pos + 4].try_into().unwrap());
        let new_size = (size as i64 + growth) as u32;
        buf[size_pos..size_pos + 4].copy_from_slice(&new_size.to_le_bytes());

        for i in 0..interior {
            let at = section + 12 + i * 4;
            let value = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
            let shifted = (value as i64 + growth) as u32;
            buf[at..at + 4].copy_from_slice(&shifted.to_le_bytes());
        }

        for (_, slot) in &slots {
            if slot.stream_offset as usize > target.stream_offset as usize {
                let shifted = (slot.stream_offset as i64 + growth) as u32;
                buf[slot.at..slot.at + 4].copy_from_slice(&shifted.to_le_bytes());
            }
        }
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleb128_round_trips() {
        for value in [0i64, 1, -1, 63, 64, -64, -65, 127, 128, -128, 1000, -1000, i32::MAX as i64] {
            let encoded = encode_sleb128(value);
            let mut reader = ByteReader::new(&encoded);
            assert_eq!(
                reader.read_sleb128().unwrap(),
                value,
                "round trip failed for {value}"
            );
            assert_eq!(
                reader.position(),
                encoded.len(),
                "encoder emitted trailing bytes for {value}"
            );
        }
    }

    // The encoding is canonical: 63 fits in one byte, 64 does not, because bit 6 is
    // the sign bit. Getting this wrong makes every re-encoded delta one byte long
    // and silently negative.
    #[test]
    fn sleb128_uses_the_sign_bit_boundary() {
        assert_eq!(encode_sleb128(63).len(), 1);
        assert_eq!(encode_sleb128(64).len(), 2);
        assert_eq!(encode_sleb128(-64).len(), 1);
        assert_eq!(encode_sleb128(-65).len(), 2);
    }
}
