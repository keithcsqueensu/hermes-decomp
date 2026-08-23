// Patch a single string-id operand within an instruction, without rebuilding
// the function body. Validates the instruction shape, checks operand width,
// and read-back verifies after the write.

use crate::error::{Error, Result};
use crate::file::BytecodeFile;
use crate::io::ByteReader;
use crate::opcode::{BytecodeFormat, Operand, OperandType};

use crate::write::serialize::finalize_raw_image;

use super::PatchOptions;

/// How the target instruction is addressed.
pub enum OperandTarget {
    /// Absolute byte offset of the opcode in the file.
    AbsoluteOffset(u32),
    /// Function id + relative offset within the function body.
    FunctionRelative { function: u32, insn_offset: u32 },
}

/// Decode one instruction from `raw` at absolute byte offset `abs_off`.
fn decode_one_at(
    raw: &[u8],
    abs_off: usize,
    format: &BytecodeFormat,
) -> Result<(crate::file::Instruction, Vec<usize>)> {
    if abs_off >= raw.len() {
        return Err(Error::Write(format!(
            "offset 0x{abs_off:x} past end of file"
        )));
    }
    let remaining = &raw[abs_off..];
    let mut reader = ByteReader::new(remaining);

    let opcode = reader.read_u8()?;
    let def = format
        .definitions
        .get(opcode as usize)
        .ok_or_else(|| Error::Write(format!("unknown opcode {opcode} at 0x{abs_off:x}")))?;

    let mut operands = Vec::with_capacity(def.operand_types.len());
    // Track the byte position of each operand relative to abs_off.
    let mut operand_positions = Vec::with_capacity(def.operand_types.len());
    for operand_type in &def.operand_types {
        operand_positions.push(abs_off + reader.position());
        let value = operand_type.read(&mut reader)?;
        operands.push(Operand {
            ty: *operand_type,
            value,
        });
    }
    let length = reader.position() as u32;

    Ok((
        crate::file::Instruction {
            offset: abs_off as u32,
            opcode,
            operands,
            length,
        },
        operand_positions,
    ))
}

/// Byte width of an operand type.
fn operand_byte_width(ty: OperandType) -> usize {
    match ty {
        OperandType::Reg8 | OperandType::UInt8 | OperandType::UInt8S | OperandType::Addr8 => 1,
        OperandType::UInt16 | OperandType::UInt16S => 2,
        OperandType::Reg32
        | OperandType::UInt32
        | OperandType::UInt32S
        | OperandType::Addr32
        | OperandType::Imm32 => 4,
        OperandType::Double => 8,
    }
}

/// Rewrite a single string-id operand in one instruction.
///
/// Decodes the instruction at `target`, finds its string operand (UInt8S /
/// UInt16S / UInt32S), validates that `new_string_id` fits in the operand
/// width, overwrites the bytes, and rehashes the footer. When the instruction
/// has multiple string operands (e.g. `CreateRegExp`), `operand_index`
/// selects which one (0-based among string operands only).
///
/// Returns the patched raw image.
pub fn patch_string_operand(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    target: OperandTarget,
    new_string_id: u32,
    operand_index: Option<usize>,
    _opts: &PatchOptions,
) -> Result<Vec<u8>> {
    let raw = file
        .raw_bytes
        .clone()
        .ok_or_else(|| Error::Write("no raw_bytes".into()))?;

    // Resolve target to absolute offset.
    let abs_off = match target {
        OperandTarget::AbsoluteOffset(off) => off as usize,
        OperandTarget::FunctionRelative {
            function,
            insn_offset,
        } => {
            let hdr = file
                .function_headers
                .get(function as usize)
                .ok_or_else(|| Error::Write(format!("function {function} not found")))?;
            hdr.offset() as usize + insn_offset as usize
        }
    };

    // Decode the instruction.
    let (insn, operand_positions) = decode_one_at(&raw, abs_off, format)?;
    let def = format
        .definitions
        .get(insn.opcode as usize)
        .ok_or_else(|| Error::Write(format!("unknown opcode {}", insn.opcode)))?;

    // Find string operand(s).
    let string_ops: Vec<(usize, &Operand, usize)> = insn
        .operands
        .iter()
        .enumerate()
        .filter(|(_, op)| matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S))
        .map(|(i, op)| (i, op, operand_positions[i]))
        .collect();

    if string_ops.is_empty() {
        return Err(Error::Write(format!(
            "instruction {} at 0x{:x} has no string operand",
            def.name, abs_off
        )));
    }

    let sel = operand_index.unwrap_or(0);
    if sel >= string_ops.len() {
        return Err(Error::Write(format!(
            "instruction {} has {} string operand(s), --operand-index {} out of range",
            def.name,
            string_ops.len(),
            sel
        )));
    }
    let (op_idx, old_op, byte_pos) = string_ops[sel];
    let op_ty = old_op.ty;
    let old_id = old_op.value.as_u32().unwrap_or(0);

    // Width check.
    let max_val: u32 = match op_ty {
        OperandType::UInt8S => 0xff,
        OperandType::UInt16S => 0xffff,
        OperandType::UInt32S => 0xffff_ffff,
        _ => unreachable!(),
    };
    if new_string_id > max_val {
        return Err(Error::Write(format!(
            "string id {} exceeds operand width ({:?} max {}); \
             use the Long variant of the opcode or ensure add-string returned a lower id",
            new_string_id, op_ty, max_val
        )));
    }

    // Write the new value.
    let width = operand_byte_width(op_ty);
    let mut out = raw;
    if byte_pos + width > out.len() {
        return Err(Error::Write("operand position out of range".into()));
    }
    match width {
        1 => out[byte_pos] = new_string_id as u8,
        2 => out[byte_pos..byte_pos + 2].copy_from_slice(&(new_string_id as u16).to_le_bytes()),
        4 => out[byte_pos..byte_pos + 4].copy_from_slice(&new_string_id.to_le_bytes()),
        _ => unreachable!(),
    }

    // Read-back verification.
    let (verify_insn, _) = decode_one_at(&out, abs_off, format)?;
    let verify_val = verify_insn.operands[op_idx].value.as_u32().unwrap_or(u32::MAX);
    if verify_val != new_string_id {
        return Err(Error::Write(format!(
            "read-back failed: expected {new_string_id}, got {verify_val}"
        )));
    }

    // Resolve old/new string values for the status message.
    let old_str = file
        .strings
        .get(old_id as usize)
        .map(|s| s.value.as_str())
        .unwrap_or("<unknown>");
    let new_str = file
        .strings
        .get(new_string_id as usize)
        .map(|s| s.value.as_str())
        .unwrap_or("<unknown>");
    eprintln!(
        "{} at 0x{:x}: operand {} ({:?}) {} ({:?}) → {} ({:?})",
        def.name, abs_off, op_idx, op_ty, old_id, old_str, new_string_id, new_str
    );

    let out = finalize_raw_image(out)?;
    file.raw_bytes = Some(out.clone());
    Ok(out)
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

    // Find a function that has at least one instruction with a string operand,
    // patch it, and verify the round-trip.
    #[test]
    fn patch_operand_roundtrip() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let raw = file.raw_bytes.as_ref().unwrap();
        // Find the first instruction with a string operand in function 0.
        let hdr = &file.function_headers[0];
        let body_off = hdr.offset() as usize;
        let body_len = hdr.bytecode_size_in_bytes() as usize;
        let mut found = None;
        let mut pos = body_off;
        while pos < body_off + body_len {
            if let Ok((insn, _positions)) = decode_one_at(raw, pos, &format) {
                let has_string = insn.operands.iter().any(|op| {
                    matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S)
                });
                if has_string {
                    let old_id = insn
                        .operands
                        .iter()
                        .find(|op| matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S))
                        .and_then(|op| op.value.as_u32())
                        .unwrap();
                    found = Some((pos as u32, old_id));
                    break;
                }
                pos += insn.length as usize;
            } else {
                break;
            }
        }
        let Some((target_off, old_id)) = found else {
            return; // No string operand in function 0
        };
        // Pick a different valid string id.
        let new_id = if old_id == 0 { 1u32 } else { 0u32 };
        if new_id as usize >= file.strings.len() {
            return;
        }
        let opts = PatchOptions::default();
        let out = patch_string_operand(
            &mut file,
            &format,
            OperandTarget::AbsoluteOffset(target_off),
            new_id,
            None,
            &opts,
        )
        .expect("patch_string_operand");
        assert!(verify_footer(&out));
        // Verify the operand was changed.
        let (verify, _) = decode_one_at(&out, target_off as usize, &format).unwrap();
        let patched_id = verify
            .operands
            .iter()
            .find(|op| matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S))
            .and_then(|op| op.value.as_u32())
            .unwrap();
        assert_eq!(patched_id, new_id);
    }

    // FunctionRelative addressing mode.
    #[test]
    fn patch_operand_function_relative() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let raw = file.raw_bytes.as_ref().unwrap();
        let hdr = &file.function_headers[0];
        let body_off = hdr.offset() as usize;
        let body_len = hdr.bytecode_size_in_bytes() as usize;
        let mut found = None;
        let mut pos = body_off;
        while pos < body_off + body_len {
            if let Ok((insn, _)) = decode_one_at(raw, pos, &format) {
                let has_string = insn.operands.iter().any(|op| {
                    matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S)
                });
                if has_string {
                    found = Some((pos - body_off) as u32);
                    break;
                }
                pos += insn.length as usize;
            } else {
                break;
            }
        }
        let Some(rel_off) = found else { return };
        let opts = PatchOptions::default();
        let out = patch_string_operand(
            &mut file,
            &format,
            OperandTarget::FunctionRelative {
                function: 0,
                insn_offset: rel_off,
            },
            0,
            None,
            &opts,
        )
        .expect("patch via function-relative");
        assert!(verify_footer(&out));
    }

    // Patching a non-string instruction should error.
    #[test]
    fn patch_operand_no_string_operand_rejected() {
        if !std::path::Path::new(FIXTURE).exists() {
            return;
        }
        let (mut file, format) = load(FIXTURE);
        let raw = file.raw_bytes.as_ref().unwrap();
        let hdr = &file.function_headers[0];
        let body_off = hdr.offset() as usize;
        let body_len = hdr.bytecode_size_in_bytes() as usize;
        // Find an instruction WITHOUT a string operand.
        let mut found = None;
        let mut pos = body_off;
        while pos < body_off + body_len {
            if let Ok((insn, _)) = decode_one_at(raw, pos, &format) {
                let has_string = insn.operands.iter().any(|op| {
                    matches!(op.ty, OperandType::UInt8S | OperandType::UInt16S | OperandType::UInt32S)
                });
                if !has_string && !insn.operands.is_empty() {
                    found = Some(pos as u32);
                    break;
                }
                pos += insn.length as usize;
            } else {
                break;
            }
        }
        let Some(target_off) = found else { return };
        let opts = PatchOptions::default();
        let result = patch_string_operand(
            &mut file,
            &format,
            OperandTarget::AbsoluteOffset(target_off),
            0,
            None,
            &opts,
        );
        assert!(result.is_err(), "should reject instruction with no string operand");
    }
}
