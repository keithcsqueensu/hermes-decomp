use crate::error::{Error, Result};
use crate::file::structure::{BytecodeFile, LiteralValue};
use crate::io::ByteReader;
use std::sync::atomic::Ordering;

// Resolve a literal-buffer string id, counting the misses.
//
// A miss means the id addresses nothing in the string table, and the value the
// caller gets back is the placeholder `<string:N>` -- which then travels through
// decompiled output, xrefs and secret scanning looking exactly like a real
// string. That is the strongest available signal that the buffer sections were
// read at the wrong offsets (parsing BigInt before the array buffer once
// produced ~93,000 of these on a Discord HBC96 bundle), so it is counted rather
// than discarded. The counter is on the file because these buffers are read
// lazily, on demand from the IR builder, long after the parse has returned.
fn resolve_string(file: &BytecodeFile, id: u32) -> String {
    match file.string_at(id) {
        Some(entry) => entry.value.clone(),
        None => {
            file.unresolved_string_ids.fetch_add(1, Ordering::Relaxed);
            format!("<string:{id}>")
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum DataBufferTag {
    Null,
    True,
    False,
    Number,
    LongString,
    ShortString,
    ByteString,
    Integer,
    Undefined,
}

pub fn read_buffer_series(
    file: &BytecodeFile,
    buffer: &[u8],
    offset: u32,
    count: u32,
) -> Result<Vec<LiteralValue>> {
    if count == 0 {
        return Ok(Vec::new());
    }
    if offset as usize >= buffer.len() {
        return Err(Error::Parse(format!(
            "buffer offset out of range: {} >= {}",
            offset,
            buffer.len()
        )));
    }

    let mut reader = ByteReader::new(&buffer[offset as usize..]);
    let mut values = Vec::with_capacity(reader.capacity_hint(count as usize));

    while values.len() < count as usize {
        let (tag, length) = read_buffer_tag(&mut reader)?;
        // Hermes never emits length-0 sequences, but a corrupt/misaligned stream
        // can produce them. Skip empty tags rather than aborting the whole series
        // so a single bad tag doesn't wipe the rest of a large literal.
        if length == 0 {
            continue;
        }
        for _ in 0..length {
            let value = read_buffer_value(file, tag, &mut reader)?;
            values.push(value);
            if values.len() == count as usize {
                break;
            }
        }
    }

    Ok(values)
}

fn read_buffer_tag(reader: &mut ByteReader<'_>) -> Result<(DataBufferTag, u32)> {
    let key_tag = reader.read_u8()?;
    let tag_bits = key_tag & 0x70;
    let length = if (key_tag & 0x80) != 0 {
        let next = reader.read_u8()? as u32;
        ((key_tag & 0x0f) as u32) << 8 | next
    } else {
        (key_tag & 0x0f) as u32
    };

    let tag = match tag_bits {
        0x00 => DataBufferTag::Null,
        0x10 => DataBufferTag::True,
        0x20 => DataBufferTag::False,
        0x30 => DataBufferTag::Number,
        0x40 => DataBufferTag::LongString,
        0x50 => DataBufferTag::ShortString,
        0x60 => DataBufferTag::ByteString,
        0x70 => DataBufferTag::Integer,
        _ => DataBufferTag::Undefined,
    };

    Ok((tag, length))
}

fn read_buffer_value(
    file: &BytecodeFile,
    tag: DataBufferTag,
    reader: &mut ByteReader<'_>,
) -> Result<LiteralValue> {
    Ok(match tag {
        DataBufferTag::Null => LiteralValue::Null,
        DataBufferTag::True => LiteralValue::Bool(true),
        DataBufferTag::False => LiteralValue::Bool(false),
        DataBufferTag::Number => LiteralValue::Number(reader.read_f64()?),
        DataBufferTag::Integer => LiteralValue::Integer(reader.read_i32()?),
        DataBufferTag::ShortString => {
            let id = reader.read_u16()? as u32;
            LiteralValue::String(resolve_string(file, id))
        }
        DataBufferTag::LongString => {
            let id = reader.read_u32()?;
            LiteralValue::String(resolve_string(file, id))
        }
        DataBufferTag::ByteString => {
            let id = reader.read_u8()? as u32;
            LiteralValue::String(resolve_string(file, id))
        }
        DataBufferTag::Undefined => LiteralValue::Undefined,
    })
}
