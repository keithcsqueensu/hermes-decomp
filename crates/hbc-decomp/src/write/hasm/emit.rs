// Emit HASM text from bytecode, and assemble HASM back into a patched image.

use std::collections::BTreeSet;

use crate::error::Result;
use crate::file::BytecodeFile;
use crate::opcode::BytecodeFormat;
use crate::write::patch::handlers::HandlerUpdate;
use crate::write::patch::PatchOptions;

use super::parse::parse_hasm_body_with_context;
use super::HasmModule;

// Emit HASM text for a function via the existing disassembler.
//
// A function with exception handlers gets them too, as `.handler start, end,
// target` lines naming labels (Q3): every handler boundary gets an `L<offset>`
// label, the scheme jump targets already use, so the handlers follow the code
// through an edit and `asm` can put them back where the edited code says.
pub fn emit_hasm_function(
    file: &BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
) -> Result<String> {
    let text = crate::disasm::disassemble_function(
        file,
        format,
        function_id,
        &crate::disasm::DisasmOptions {
            show_offsets: true,
            show_labels: true,
            resolve_strings: true,
            enable_color: false,
        },
    )?;
    let Some(handlers) = file.exception_handlers.get(&function_id) else {
        return Ok(text);
    };

    // Label each boundary that is not labelled already, before the instruction at
    // that offset -- or after the last one, for an `end` at the end of the body.
    let wanted: BTreeSet<u32> = handlers
        .iter()
        .flat_map(|h| [h.start, h.end, h.target])
        .collect();
    let label = |off: u32| format!("L{off}:");
    let mut out = String::new();
    let mut placed = BTreeSet::new();
    for line in text.lines() {
        let at = line
            .split_whitespace()
            .next()
            .filter(|t| t.len() >= 4 && t.chars().all(|c| c.is_ascii_hexdigit()))
            .and_then(|t| u32::from_str_radix(t, 16).ok());
        if let Some(off) = at {
            if wanted.contains(&off)
                && placed.insert(off)
                && !out.ends_with(&format!("{}\n", label(off)))
            {
                out.push_str(&label(off));
                out.push('\n');
            }
        } else if let Some(off) = line.strip_suffix(':').and_then(|l| l.strip_prefix('L')) {
            if let Ok(off) = off.parse::<u32>() {
                placed.insert(off);
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    for off in wanted.difference(&placed) {
        out.push_str(&label(*off));
        out.push('\n');
    }
    for h in handlers {
        out.push_str(&format!(
            ".handler L{}, L{}, L{}\n",
            h.start, h.end, h.target
        ));
    }
    Ok(out)
}

// Assemble: replace function bodies in `base` from a map of id → hasm text.
pub fn assemble_function_hasm(
    base: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    hasm_text: &str,
) -> Result<Vec<u8>> {
    assemble_function_hasm_with(base, format, function_id, hasm_text, &Default::default())
}

// `assemble_function_hasm` with patch options. When the HASM states the
// function's exception handlers (`.handler` lines), they are written into its
// handler table at the offsets the new body gives them, so a function with
// handlers can change size (Q3 phase 2). Without them its handlers are kept as
// they are, which only a same-size body allows.
pub fn assemble_function_hasm_with(
    base: &mut BytecodeFile,
    format: &BytecodeFormat,
    function_id: u32,
    hasm_text: &str,
    options: &PatchOptions,
) -> Result<Vec<u8>> {
    let (insns, handlers) = parse_hasm_body_with_context(hasm_text, format, base)?;
    let update = handlers.map_or(HandlerUpdate::Keep, HandlerUpdate::Replace);
    crate::write::patch::functions::patch_function_body_with(
        base,
        format,
        function_id,
        &insns,
        options,
        update,
    )
}

// Assemble a whole module against a base file (each function body replaced when present).
pub fn assemble_module(
    base: &BytecodeFile,
    format: &BytecodeFormat,
    module: &HasmModule,
) -> Result<BytecodeFile> {
    let mut file = base.clone();
    for func in &module.functions {
        if func.instructions.is_empty() {
            continue;
        }
        let _ = crate::write::patch::patch_function_body(
            &mut file,
            format,
            func.id,
            &func.instructions,
            &Default::default(),
        )?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opcode::BytecodeFormat;

    // emit -> parse -> assemble round-trip reparses and keeps the function count.
    #[test]
    fn hasm_roundtrip_v96() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let mut file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let text = emit_hasm_function(&file, &format, 0).unwrap();
        let out = assemble_function_hasm(&mut file, &format, 0, &text).unwrap();
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.function_count, file.header.function_count);
    }
}
