// Replace the bytecode of one RegExp table entry with a payload compiled
// elsewhere -- `plan_guides/01_read/unmodeled_regions/PLAN.md` P4a.
//
// A regex bytecode stream is position-independent and its table offset is
// relative to regexp storage, so a stream `hermesc` compiled into another file
// runs verbatim here: there is nothing to assemble and nothing inside it to fix
// up. Two ways to land it, cheapest first:
//
//   * in place (archetype B) -- the payload fits the entry's slot and no other
//     entry shares those bytes: overwrite the storage and set the entry's
//     length. Same file size; the slack tail of the old stream is never read.
//   * appended (archetype C) -- otherwise: add the payload to the end of regexp
//     storage and point the entry at it. Every index stays put, so no
//     `CreateRegExp` operand changes, and the tail after the storage moves
//     through `write::reloc::relocate`. The old bytes stay behind, unreferenced.
//
// The stream starts with `RegexBytecodeHeader` -- u16 markedCount, u16
// loopCount, u8 syntaxFlags, u8 constraints. markedCount is the capture-group
// count and is a contract with the code that calls the regex (`m[1]`), so a
// payload with fewer groups than the entry it replaces is refused.
// `.source` and `.flags` do NOT follow: they are `CreateRegExp`'s string
// operands. The report says so whenever the payload's flags byte differs.
//
// The third way (archetype A) touches no table at all: `repoint_regexp` changes
// one `CreateRegExp` to use another entry the bundle already has. When some
// other `CreateRegExp` already uses that entry, its two string operands are the
// right `.source` and `.flags` for it, so they are copied too and nothing is
// left describing the old pattern.

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::opcode::{BytecodeFormat, OperandType};
use crate::write::reloc::{relocate, Reloc};
use crate::write::serialize::{commit_image, section_offset};

use super::operands::{decode_one_at, operand_byte_width, resolve_target, OperandTarget};
use super::PatchOptions;

/// Size of `RegexBytecodeHeader`, the prefix every stream starts with.
pub const REGEX_HEADER_LEN: usize = 6;

/// How `set_regexp` landed the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegexpSetMode {
    /// Overwrote the entry's own storage; the file size is unchanged.
    InPlace,
    /// Appended to regexp storage and repointed the entry; the tail shifted.
    Appended,
}

/// What `set_regexp` did, and what the caller should know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexpSetReport {
    pub index: u32,
    pub mode: RegexpSetMode,
    pub old_len: u32,
    pub new_len: u32,
    pub old_marked_count: u16,
    pub new_marked_count: u16,
    pub old_syntax_flags: u8,
    pub new_syntax_flags: u8,
}

impl RegexpSetReport {
    /// The flag letters of a `syntaxFlags` byte, in Hermes' bit order from bit 0
    /// up: `i g m u s y d` (`SyntaxFlags::toByte`).
    pub fn flags_text(flags: u8) -> String {
        "igmusyd"
            .chars()
            .enumerate()
            .filter(|(bit, _)| flags & (1 << bit) != 0)
            .map(|(_, c)| c)
            .collect()
    }
}

/// The bytecode of RegExp entry `index`, e.g. to take a donor out of a file
/// `hermesc` compiled.
pub fn regexp_payload(file: &BytecodeFile, index: u32) -> Result<&[u8]> {
    let e = file.reg_exp_table.get(index as usize).ok_or_else(|| {
        Error::Write(format!(
            "regexp index {index} out of range ({} entries)",
            file.reg_exp_table.len()
        ))
    })?;
    let (off, len) = (e.offset as usize, e.length as usize);
    file.reg_exp_storage
        .get(off..off + len)
        .ok_or_else(|| Error::Write(format!("regexp {index}: storage range out of bounds")))
}

// markedCount and syntaxFlags of a stream, refusing one too short to have a header.
fn header_of(stream: &[u8], what: &str) -> Result<(u16, u8)> {
    if stream.len() < REGEX_HEADER_LEN {
        return Err(Error::Write(format!(
            "{what} is {} bytes, shorter than the {REGEX_HEADER_LEN}-byte regex header",
            stream.len()
        )));
    }
    Ok((u16::from_le_bytes([stream[0], stream[1]]), stream[4]))
}

// Byte position of `regExpCount` in the file header; `regExpStorageSize` follows
// it. Both layouts put it after the string counts, behind the bigint pair when
// the version has one (always, in the modern layout).
fn reg_exp_count_pos(file: &BytecodeFile) -> usize {
    let modern = matches!(
        file.header.function_header_layout,
        crate::format::FunctionHeaderLayout::Modern12
    );
    if modern || file.header.big_int_count.is_some() {
        72
    } else {
        64
    }
}

/// Replace RegExp entry `index`'s bytecode with `payload`, a stream compiled by
/// `hermesc` (see `regexp_payload`). In place when it fits the entry's own slot,
/// appended otherwise; see the module comment. Refuses a payload with fewer
/// capture groups than the stream it replaces.
pub fn set_regexp(
    file: &mut BytecodeFile,
    index: u32,
    payload: &[u8],
    _opts: &PatchOptions,
) -> Result<(Vec<u8>, RegexpSetReport)> {
    let old = regexp_payload(file, index)?;
    let (old_marked, old_flags) = header_of(old, &format!("regexp {index}"))?;
    let (new_marked, new_flags) = header_of(payload, "the payload")?;
    if new_marked < old_marked {
        return Err(Error::Write(format!(
            "regexp {index} has {old_marked} capture group(s) and the payload has \
             {new_marked}: code reading a group that no longer exists would get \
             `undefined`, far from this edit. Use a payload with at least \
             {old_marked}."
        )));
    }
    let entry = file.reg_exp_table[index as usize];
    let (old_off, old_len) = (entry.offset as usize, entry.length as usize);
    let new_len = u32::try_from(payload.len())
        .map_err(|_| Error::Write("regexp payload larger than 4 GiB".into()))?;

    let table_off = section_offset(file, "regexp_table")
        .ok_or_else(|| Error::Write("regexp_table section missing".into()))?
        as usize;
    let storage_off = section_offset(file, "regexp_storage")
        .ok_or_else(|| Error::Write("regexp_storage section missing".into()))?
        as usize;
    let storage_section = file
        .sections
        .iter()
        .find(|s| s.name == "regexp_storage")
        .map(|s| s.size as usize)
        .unwrap_or(0);
    let storage_size = file.header.reg_exp_storage_size as usize;
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    // Refuse to write through a header position that does not hold the values
    // the parser read: a wrong position here would corrupt a neighbouring field.
    let count_pos = reg_exp_count_pos(file);
    let at = |pos: usize| u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap());
    if at(count_pos) != file.header.reg_exp_count
        || at(count_pos + 4) != file.header.reg_exp_storage_size
    {
        return Err(Error::Write(format!(
            "regexp count/size not found at header byte {count_pos} for version {}",
            file.header.version
        )));
    }

    // In place only when no other entry reads any byte of this slot: hermesc
    // packs storage end to end, but nothing forbids two entries sharing one.
    let shared = file.reg_exp_table.iter().enumerate().any(|(i, e)| {
        let (o, l) = (e.offset as usize, e.length as usize);
        i != index as usize && l > 0 && o < old_off + old_len && old_off < o + l
    });
    let entry_pos = table_off + index as usize * 8;

    let (out_raw, mode) = if payload.len() <= old_len && !shared {
        let at = storage_off + old_off;
        raw[at..at + payload.len()].copy_from_slice(payload);
        raw[entry_pos + 4..entry_pos + 8].copy_from_slice(&new_len.to_le_bytes());
        (raw, RegexpSetMode::InPlace)
    } else {
        // Storage is recorded unpadded in the header and padded to 4 in the file.
        let mut storage = raw[storage_off..storage_off + storage_size].to_vec();
        storage.extend_from_slice(payload);
        let new_size = u32::try_from(storage.len())
            .map_err(|_| Error::Write("regexp storage larger than 4 GiB".into()))?;
        while !storage.len().is_multiple_of(4) {
            storage.push(0);
        }
        let moved_from = storage_off + storage_section;
        let delta = storage.len() as i64 - storage_section as i64;

        let mut rebuilt = Vec::with_capacity((raw.len() as i64 + delta) as usize);
        rebuilt.extend_from_slice(&raw[..storage_off]);
        rebuilt.extend_from_slice(&storage);
        rebuilt.extend_from_slice(&raw[moved_from..]);
        rebuilt[count_pos + 4..count_pos + 8].copy_from_slice(&new_size.to_le_bytes());
        rebuilt[entry_pos..entry_pos + 4].copy_from_slice(&(storage_size as u32).to_le_bytes());
        rebuilt[entry_pos + 4..entry_pos + 8].copy_from_slice(&new_len.to_le_bytes());
        relocate(
            file,
            &mut rebuilt,
            &Reloc {
                moved_from,
                delta,
                resized: None,
            },
        )?;
        (rebuilt, RegexpSetMode::Appended)
    };

    let out = commit_image(file, out_raw)?;
    Ok((
        out,
        RegexpSetReport {
            index,
            mode,
            old_len: old_len as u32,
            new_len,
            old_marked_count: old_marked,
            new_marked_count: new_marked,
            old_syntax_flags: old_flags,
            new_syntax_flags: new_flags,
        },
    ))
}

/// What `repoint_regexp` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegexpRepointReport {
    /// Absolute file offset of the `CreateRegExp` that was changed.
    pub at: u32,
    pub old_index: u32,
    pub new_index: u32,
    /// Absolute offset of the other `CreateRegExp` already using `new_index`
    /// whose pattern and flags strings were copied; `None` when no site uses it,
    /// and the strings were left describing the old pattern.
    pub strings_from: Option<u32>,
    pub old_marked_count: u16,
    pub new_marked_count: u16,
    pub old_syntax_flags: u8,
    pub new_syntax_flags: u8,
}

// The operand positions of a decoded CreateRegExp: (pattern, flags, index), each
// as (operand type, absolute byte position, value). The opcode is
// `(Reg8 dst, UInt32S pattern, UInt32S flags, UInt32 index)` at v96, v98 and
// v99; the shape is checked rather than assumed, so a version that moved it is
// refused instead of mis-edited.
type Slot = (OperandType, usize, u32);

fn create_regexp_slots(
    raw: &[u8],
    abs_off: usize,
    format: &BytecodeFormat,
) -> Result<(Slot, Slot, Slot)> {
    let (insn, pos) = decode_one_at(raw, abs_off, format)?;
    let name = format
        .definitions
        .get(insn.opcode as usize)
        .map(|d| d.name.as_str())
        .unwrap_or("?");
    let is_string = |t: OperandType| {
        matches!(
            t,
            OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S
        )
    };
    let slot = |i: usize| -> Slot {
        let op = &insn.operands[i];
        (op.ty, pos[i], op.value.as_u32().unwrap_or(u32::MAX))
    };
    match insn.operands.iter().map(|o| o.ty).collect::<Vec<_>>()[..] {
        [_, p, f, OperandType::UInt32]
            if name == "CreateRegExp" && is_string(p) && is_string(f) =>
        {
            Ok((slot(1), slot(2), slot(3)))
        }
        _ => Err(Error::Write(format!(
            "instruction at 0x{abs_off:x} is {name}, not a CreateRegExp of the \
             (dst, pattern, flags, index) shape"
        ))),
    }
}

fn write_operand(buf: &mut [u8], (ty, pos, _): Slot, value: u32) {
    match operand_byte_width(ty) {
        1 => buf[pos] = value as u8,
        2 => buf[pos..pos + 2].copy_from_slice(&(value as u16).to_le_bytes()),
        _ => buf[pos..pos + 4].copy_from_slice(&value.to_le_bytes()),
    }
}

/// Point one `CreateRegExp` at RegExp entry `new_index`, an entry the bundle
/// already has (archetype A). No table or storage changes. When another
/// `CreateRegExp` already uses `new_index`, its pattern and flags string ids are
/// copied too, so `.source` and `.flags` match what the regex now does. Refuses
/// an entry with fewer capture groups than the one it replaces.
pub fn repoint_regexp(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    target: OperandTarget,
    new_index: u32,
    _opts: &PatchOptions,
) -> Result<(Vec<u8>, RegexpRepointReport)> {
    let abs_off = resolve_target(file, &target)?;
    let mut raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;
    let (pattern, flags, index) = create_regexp_slots(&raw, abs_off, format)?;
    let old_index = index.2;
    if old_index == new_index {
        return Err(Error::Write(format!(
            "CreateRegExp at 0x{abs_off:x} already uses regexp {new_index}"
        )));
    }
    let (old_marked, old_flags) = header_of(
        regexp_payload(file, old_index)?,
        &format!("regexp {old_index}"),
    )?;
    let (new_marked, new_flags) = header_of(
        regexp_payload(file, new_index)?,
        &format!("regexp {new_index}"),
    )?;
    if new_marked < old_marked {
        return Err(Error::Write(format!(
            "regexp {old_index} has {old_marked} capture group(s) and regexp \
             {new_index} has {new_marked}: code reading a group that no longer \
             exists would get `undefined`, far from this edit"
        )));
    }

    // Another site that already uses the new entry carries the strings that
    // describe it. Same opcode, so the same operand types.
    let mut strings_from = None;
    'search: for id in 0..file.function_headers.len() as u32 {
        let Ok(insns) = file.decode_function_instructions(format, id) else {
            continue;
        };
        let base = file.function_headers[id as usize].offset() as usize;
        for insn in insns {
            let at = base + insn.offset as usize;
            if at == abs_off {
                continue;
            }
            if let Ok((p, f, i)) = create_regexp_slots(&raw, at, format) {
                if i.2 == new_index && p.0 == pattern.0 && f.0 == flags.0 {
                    strings_from = Some((at, p.2, f.2));
                    break 'search;
                }
            }
        }
    }

    write_operand(&mut raw, index, new_index);
    if let Some((_, p, f)) = strings_from {
        write_operand(&mut raw, pattern, p);
        write_operand(&mut raw, flags, f);
    }
    let (_, _, check) = create_regexp_slots(&raw, abs_off, format)?;
    if check.2 != new_index {
        return Err(Error::Write(format!(
            "read-back failed: expected regexp {new_index}, got {}",
            check.2
        )));
    }

    let out = commit_image(file, raw)?;
    Ok((
        out,
        RegexpRepointReport {
            at: abs_off as u32,
            old_index,
            new_index,
            strings_from: strings_from.map(|(at, _, _)| at as u32),
            old_marked_count: old_marked,
            new_marked_count: new_marked,
            old_syntax_flags: old_flags,
            new_syntax_flags: new_flags,
        },
    ))
}
