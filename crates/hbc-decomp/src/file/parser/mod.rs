use crate::error::Result;
use crate::file::{BytecodeFile, Instruction, LiteralValue, ShapeTableEntry, StringTableEntry};
use crate::format::{FunctionHeaderLayout, HeaderLayout};
use crate::opcode::BytecodeFormat;

pub mod buffer;
pub mod function;
pub mod header;
mod helpers;
mod instructions;
mod parsing;
pub mod table;

impl BytecodeFile {
    pub fn parse_auto(bytes: &[u8]) -> Result<Self> {
        parsing::parse_auto(bytes)
    }

    pub fn parse_with_layout(
        bytes: &[u8],
        layout: HeaderLayout,
        function_layout: FunctionHeaderLayout,
    ) -> Result<Self> {
        parsing::parse_with_layout(bytes, layout, function_layout)
    }

    pub fn decode_function_instructions(
        &self,
        format: &BytecodeFormat,
        function_id: u32,
    ) -> Result<Vec<Instruction>> {
        instructions::decode_function_instructions(self, format, function_id)
    }

    /// Resolve the opcode table for this file's declared version, recording a
    /// diagnostic if a *different* version's table had to be substituted.
    ///
    /// `BytecodeFormat::for_version_or_latest` returns the substituted version so
    /// the caller can report it, and two of its three callers threw that away
    /// with `let (format, _)` -- the library entry point and the MCP one, i.e.
    /// exactly the two consumed by code rather than by a human reading stderr.
    /// A wrong opcode table does not fail: it yields syntactically perfect
    /// JavaScript with the wrong instructions in it (at v99, eight phantom
    /// opcodes shifted twelve later ones and `===` decoded as `>=`). Route
    /// format resolution through here so that cannot happen silently again.
    pub fn resolve_format(&mut self) -> Result<BytecodeFormat> {
        let declared = self.header.version;
        let (format, used) = BytecodeFormat::for_version_or_latest(declared)?;
        if used != declared {
            let d = crate::file::structure::Diagnostic::OpcodeTableSubstituted { declared, used };
            if !self.diagnostics.contains(&d) {
                self.diagnostics.push(d);
            }
        }
        Ok(format)
    }

    pub fn string_at(&self, string_id: u32) -> Option<&StringTableEntry> {
        self.strings.get(string_id as usize)
    }

    pub fn shape_at(&self, shape_id: u32) -> Option<ShapeTableEntry> {
        self.obj_shape_table.get(shape_id as usize).copied()
    }

    // Get BigInt value at the given index.
    pub fn bigint_at(&self, bigint_id: u32) -> Option<String> {
        helpers::bigint_at(self, bigint_id)
    }

    pub fn read_array_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_array_buffer_series(self, offset, count)
    }

    pub fn read_key_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_key_buffer_series(self, offset, count)
    }

    pub fn read_value_buffer_series(&self, offset: u32, count: u32) -> Result<Vec<LiteralValue>> {
        helpers::read_value_buffer_series(self, offset, count)
    }
}
