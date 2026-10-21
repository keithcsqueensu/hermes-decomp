// Create a minimal valid `.hbc` from scratch.

use crate::error::{Error, Result};
use crate::file::Instruction;
use crate::opcode::{BytecodeFormat, Operand, OperandType, OperandValue};

use super::encode::encode_function_body;
use super::serialize::{build_minimal_legacy, build_minimal_modern};

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub version: u32,
    /// Optional pre-encoded global body. If empty, a trivial
    /// `LoadConstUndefined; Ret` is built for the version.
    pub global_body: Vec<u8>,
    pub strings: Vec<String>,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            version: 96,
            global_body: Vec::new(),
            strings: vec!["global".into()],
        }
    }
}

// Build a minimal loadable bytecode image.
pub fn create_minimal(options: &CreateOptions) -> Result<Vec<u8>> {
    let body = if options.global_body.is_empty() {
        let format = BytecodeFormat::for_version_or_latest(options.version)?.0;
        let load = format
            .definitions
            .iter()
            .find(|d| d.name == "LoadConstUndefined")
            .map(|d| d.opcode)
            .ok_or_else(|| Error::Write("LoadConstUndefined missing".into()))?;
        let ret = format
            .definitions
            .iter()
            .find(|d| d.name == "Ret")
            .map(|d| d.opcode)
            .ok_or_else(|| Error::Write("Ret missing".into()))?;
        let insns = vec![
            Instruction {
                offset: 0,
                opcode: load,
                operands: vec![Operand {
                    ty: OperandType::Reg8,
                    value: OperandValue::U8(0),
                }],
                length: 2,
            },
            Instruction {
                offset: 2,
                opcode: ret,
                operands: vec![Operand {
                    ty: OperandType::Reg8,
                    value: OperandValue::U8(0),
                }],
                length: 2,
            },
        ];
        encode_function_body(&format, &insns)?
    } else {
        options.global_body.clone()
    };

    let mut strings = options.strings.clone();
    if strings.is_empty() {
        strings.push("global".into());
    }
    let image = if options.version >= 97 {
        build_minimal_modern(options.version, &strings, &body)?
    } else {
        build_minimal_legacy(options.version, &strings, &body)?
    };
    check_created(&image, options.version, &strings, &body)?;
    Ok(image)
}

// R14: read the image back and require exactly what `create` meant to write --
// one function holding `body`, `strings` in order and all of the String kind, and
// every other count, size and offset in the header zero. The header's field set
// is version-gated in both directions (the builders write it, the parser reads
// it), so a gate that disagreed would surface here as a garbled field, at every
// version `create` accepts rather than only the three with an engine to run on.
fn check_created(image: &[u8], version: u32, strings: &[String], body: &[u8]) -> Result<()> {
    let file = crate::file::BytecodeFile::parse_auto(image)
        .map_err(|e| Error::Write(format!("create v{version}: output does not reparse: {e}")))?;
    let h = &file.header;
    let mut wrong: Vec<String> = Vec::new();
    let mut expect = |name: &str, got: u32, want: u32| {
        if got != want {
            wrong.push(format!("{name} = {got}, expected {want}"));
        }
    };
    expect("version", h.version, version);
    expect("file_length", h.file_length, image.len() as u32);
    expect("function_count", h.function_count, 1);
    expect("global_code_index", h.global_code_index, 0);
    expect("string_count", h.string_count, strings.len() as u32);
    expect("string_kind_count", h.string_kind_count, 1);
    expect("identifier_count", h.identifier_count, 0);
    expect("debug_info_offset", h.debug_info_offset, 0);
    expect("options", h.options_raw as u32, 0);
    for (name, value) in [
        ("big_int_count", h.big_int_count),
        ("big_int_storage_size", h.big_int_storage_size),
        ("reg_exp_count", Some(h.reg_exp_count)),
        ("reg_exp_storage_size", Some(h.reg_exp_storage_size)),
        ("literal_value_buffer_size", h.literal_value_buffer_size),
        ("array_buffer_size", h.array_buffer_size),
        ("obj_key_buffer_size", Some(h.obj_key_buffer_size)),
        ("obj_value_buffer_size", h.obj_value_buffer_size),
        ("obj_shape_table_count", h.obj_shape_table_count),
        ("num_string_switch_imms", h.num_string_switch_imms),
        ("segment_id", h.segment_id),
        ("cjs_module_offset", h.cjs_module_offset),
        ("cjs_module_count", Some(h.cjs_module_count)),
        ("function_source_count", h.function_source_count),
    ] {
        expect(name, value.unwrap_or(0), 0);
    }
    let got: Vec<&str> = file.strings.iter().map(|s| s.value.as_str()).collect();
    if got != strings.iter().map(String::as_str).collect::<Vec<_>>() {
        wrong.push(format!("strings = {got:?}, expected {strings:?}"));
    }
    let f = &file.function_headers[0];
    let at = f.offset() as usize;
    if image.get(at..at + f.bytecode_size_in_bytes() as usize) != Some(body) {
        wrong.push("function 0 does not hold the body".into());
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(Error::Write(format!(
            "create v{version}: the image does not read back as built: {}",
            wrong.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use crate::write::footer::verify_footer;

    // R14 at every version `create` accepts: its own read-back check passes with
    // a string that needs an overflow record and one stored as UTF-16. Only v96,
    // v98 and v99 have an engine to run on, so for the other 56 this is the check
    // there is. v97's modern layout is a different vintage nothing here models,
    // and `create` refuses it rather than guessing.
    #[test]
    fn create_reads_back_as_built_at_every_version() {
        let strings = vec!["global".to_string(), "x".repeat(300), "héllo".to_string()];
        for version in 40..=99 {
            let made = create_minimal(&CreateOptions {
                version,
                strings: strings.clone(),
                ..Default::default()
            });
            match version {
                97 => assert!(made.is_err(), "v97 must be refused"),
                _ => {
                    made.unwrap_or_else(|e| panic!("v{version}: {e}"));
                }
            }
        }
    }

    #[test]
    fn the_read_back_check_names_what_is_wrong() {
        let strings = vec!["global".to_string()];
        let mut image = create_minimal(&CreateOptions::default()).unwrap();
        // reg_exp_count, as a misplaced header write would leave it.
        image[72..76].copy_from_slice(&3u32.to_le_bytes());
        crate::write::footer::rehash_footer(&mut image).unwrap();
        let body = {
            let file = BytecodeFile::parse_auto(&image).unwrap();
            let f = &file.function_headers[0];
            image[f.offset() as usize..(f.offset() + f.bytecode_size_in_bytes()) as usize].to_vec()
        };
        let err = check_created(&image, 96, &strings, &body)
            .unwrap_err()
            .to_string();
        assert!(err.contains("reg_exp_count = 3"), "{err}");
    }

    #[test]
    fn create_minimal_v96_parses() {
        let bytes = create_minimal(&CreateOptions {
            version: 96,
            ..Default::default()
        })
        .expect("create");
        assert!(verify_footer(&bytes));
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created file");
        assert_eq!(file.header.version, 96);
        assert_eq!(file.header.function_count, 1);
        assert!(!file.strings.is_empty());
    }

    // Modern create builds an overflowed global with an out-of-line large header.
    // The whole image must parse back with a modern layout and a running v98 VM
    // accepts it (checked separately against the external toolchain).
    #[test]
    fn create_minimal_v98_parses() {
        let bytes = create_minimal(&CreateOptions {
            version: 98,
            global_body: Vec::new(),
            strings: vec!["global".into(), "hello".into()],
        })
        .expect("create v98");
        assert!(verify_footer(&bytes));
        let file = BytecodeFile::parse_auto(&bytes).expect("parse created v98 file");
        assert_eq!(file.header.version, 98);
        assert_eq!(file.header.function_count, 1);
        assert!(matches!(
            file.header.function_header_layout,
            crate::format::FunctionHeaderLayout::Modern12
        ));
        // The global is overflowed, so parsing had to follow the out-of-line large
        // header pointer to recover the body. A non-zero body offset and size prove
        // that indirection resolved.
        assert!(file.function_headers[0].offset() > 0);
        assert_eq!(file.strings.len(), 2);
    }
}
