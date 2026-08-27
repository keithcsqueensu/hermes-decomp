use crate::debug::DebugInfo;
use crate::format::{BytecodeHeader, FunctionHeader, HeaderLayout};
use crate::opcode::Operand;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringKindType {
    String,
    Identifier,
}

#[derive(Debug, Clone)]
pub struct StringKindEntry {
    pub kind: StringKindType,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct StringTableEntry {
    pub value: String,
    pub is_utf16: bool,
    pub is_identifier: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct TableEntry {
    pub offset: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ShapeTableEntry {
    pub key_buffer_offset: u32,
    pub num_props: u32,
}

#[derive(Debug, Clone)]
pub enum LiteralValue {
    Null,
    Bool(bool),
    Number(f64),
    Integer(i32),
    String(String),
    Undefined,
}

#[derive(Debug, Clone)]
pub struct SectionInfo {
    pub name: &'static str,
    pub offset: u32,
    pub size: u32,
    pub entries: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptionHandler {
    pub start: u32,
    pub end: u32,
    pub target: u32,
}

/// Why this file has no parsed debug info -- or that it does.
///
/// `debug_info: Option<DebugInfo>` collapses five different situations onto one
/// `None`, and "the file was built without `-g`" is indistinguishable from "we
/// cannot read this version's header" or "the section points past EOF". Callers
/// that want to say *why* read this instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugInfoStatus {
    /// Parsed. The payload is in `BytecodeFile::debug_info`.
    Present,
    /// `debug_info_offset` is 0 or `NO_OFFSET`: the file genuinely carries none.
    Absent,
    /// `debug_info_offset` points past the end of the file.
    OffsetOutOfRange,
    /// This crate has no `DebugLayout` for the version, so reading it would be a
    /// guess. v97, and everything below v96 or above v99.
    UnsupportedVersion(u32),
    /// The header parsed but its interior offsets/sizes leave the file.
    HeaderOutOfRange,
    /// The section parsed as far as the header and then failed.
    ParseFailed,
}

impl DebugInfoStatus {
    pub fn describe(self) -> String {
        match self {
            Self::Present => "present".to_string(),
            Self::Absent => "absent (the file carries no debug info)".to_string(),
            Self::OffsetOutOfRange => "unreadable: debug_info_offset points past EOF".to_string(),
            Self::UnsupportedVersion(v) => {
                format!("unreadable: no debug-info layout is modelled for bytecode version {v}")
            }
            Self::HeaderOutOfRange => {
                "unreadable: the debug-info header's own offsets leave the file".to_string()
            }
            Self::ParseFailed => "unreadable: the debug-info section failed to parse".to_string(),
        }
    }

    /// True when the absence is a limitation or a corruption rather than a fact
    /// about the file. These are worth telling the user about; `Absent` is not.
    pub fn is_failure(self) -> bool {
        !matches!(self, Self::Present | Self::Absent)
    }
}

/// A recoverable "this read is not what it looks like" condition, recorded during
/// the parse instead of being thrown away.
///
/// The read path deliberately degrades rather than failing -- reading a
/// deliberately broken image is a legitimate use of this crate. That is only safe
/// if the degradation is *reported*, which is what this is for. Nothing here fails
/// a parse; every variant is a fact the caller should be able to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    /// The trailing 20-byte SHA-1 footer does not match the rest of the image.
    /// Usually means a hand-patched bundle whose footer was never refreshed --
    /// which the Hermes VM rejects at load time.
    FooterMismatch,
    /// `header.file_length` disagrees with the actual byte count (truncated,
    /// appended to, or mis-parsed).
    LengthMismatch { declared: u32, actual: usize },
    /// The layout that parsed is not the one the declared version implies. The
    /// file was decoded at a stride the version says is wrong; treat every offset
    /// in the result as suspect.
    LayoutFallback {
        version: u32,
        used: HeaderLayout,
        implied: HeaderLayout,
    },
    /// An opcode table for a *different* version was substituted. Recorded by the
    /// caller that resolves the format, not by the parser.
    OpcodeTableSubstituted { declared: u32, used: u32 },
    /// N string-table entries had storage offsets outside the string storage and
    /// decoded to an `<invalid utf8>` / `<invalid utf16>` placeholder.
    InvalidStringStorage(usize),
    /// Debug info could not be read, and why.
    DebugInfoUnreadable(DebugInfoStatus),
}

impl Diagnostic {
    /// One line, suitable for stderr or an MCP response.
    pub fn describe(&self) -> String {
        match self {
            Self::FooterMismatch => "the trailing SHA-1 footer does not match the file contents \
                 (a hand-patched bundle whose footer was not refreshed; the Hermes VM will \
                 reject it)"
                .to_string(),
            Self::LengthMismatch { declared, actual } => format!(
                "header.file_length is {declared} but the file is {actual} bytes \
                 ({} bytes {})",
                (*actual as i64 - *declared as i64).unsigned_abs(),
                if (*actual as u64) < *declared as u64 { "short" } else { "extra" }
            ),
            Self::LayoutFallback {
                version,
                used,
                implied,
            } => format!(
                "parsed with the {used:?} header layout, but bytecode version {version} implies \
                 {implied:?}; the {implied:?} parse failed. Every offset below was decoded at a \
                 stride the version says is wrong"
            ),
            Self::OpcodeTableSubstituted { declared, used } => format!(
                "no opcode table for bytecode version {declared}; decoded with the version {used} \
                 table. Operand shapes and opcode numbering may differ, which silently changes \
                 which instruction each byte means"
            ),
            Self::InvalidStringStorage(n) => format!(
                "{n} string-table entries point outside the string storage and decoded to a \
                 placeholder"
            ),
            Self::DebugInfoUnreadable(s) => format!("debug info {}", s.describe()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BytecodeFile {
    pub header: BytecodeHeader,
    pub function_headers: Vec<FunctionHeader>,
    pub string_kinds: Vec<StringKindEntry>,
    pub identifier_hashes: Vec<u32>,
    pub strings: Vec<StringTableEntry>,
    pub big_int_table: Vec<TableEntry>,
    pub big_int_storage: Vec<u8>,
    pub reg_exp_table: Vec<TableEntry>,
    pub reg_exp_storage: Vec<u8>,
    pub array_buffer: Vec<u8>,
    pub literal_value_buffer: Vec<u8>,
    pub obj_key_buffer: Vec<u8>,
    pub obj_value_buffer: Vec<u8>,
    pub obj_shape_table: Vec<ShapeTableEntry>,
    pub cjs_module_table: Vec<(u32, u32)>,
    pub function_source_table: Vec<(u32, u32)>,
    pub instruction_offset: u32,
    pub instructions: Vec<u8>,
    pub debug_info: Option<DebugInfo>,
    /// Why `debug_info` is what it is. `Absent` means the file carries none;
    /// anything else that is not `Present` means we could not read what is there.
    pub debug_info_status: DebugInfoStatus,
    pub exception_handlers: BTreeMap<u32, Vec<ExceptionHandler>>,
    pub sections: Vec<SectionInfo>,
    /// Recoverable "this read is not what it looks like" conditions found during
    /// the parse. Empty is the normal case. See `Diagnostic`.
    pub diagnostics: Vec<Diagnostic>,
    /// Count of literal-buffer string ids that did not resolve to a string-table
    /// entry, and were rendered as a `<string:N>` placeholder.
    ///
    /// Shared and interior-mutable because the literal buffers are read lazily,
    /// on demand from the IR builder, long after the parse returns -- so this
    /// cannot be a parse-time total. A non-zero value is the single strongest
    /// signal available that the file was decoded at the wrong offsets: reading
    /// the sections in the wrong order once produced ~93,000 of these on a
    /// Discord HBC96 bundle, a signal that was there to be counted and was not.
    pub unresolved_string_ids: Arc<AtomicUsize>,
    /// Original file bytes when parsed from disk. Used by the write path for
    /// identity serialize and surgical patches (keeps overflow headers intact).
    pub raw_bytes: Option<Vec<u8>>,
}

impl BytecodeFile {
    /// Every diagnostic worth showing a user, as one line each: the parse-time
    /// ones plus the lazily-accumulated unresolved-string count.
    ///
    /// Call it *after* the work that reads literal buffers (decompilation) to
    /// pick up the lazy counter; call it right after parsing for the rest.
    pub fn warnings(&self) -> Vec<String> {
        let mut out: Vec<String> = self.diagnostics.iter().map(Diagnostic::describe).collect();
        let unresolved = self.unresolved_string_ids.load(Ordering::Relaxed);
        if unresolved > 0 {
            out.push(format!(
                "{unresolved} literal-buffer string ids did not resolve to a string-table entry \
                 and were rendered as `<string:N>`; this usually means the buffer sections were \
                 read at the wrong offsets"
            ));
        }
        out
    }

    /// True when nothing about this read was degraded.
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty() && self.unresolved_string_ids.load(Ordering::Relaxed) == 0
    }
}

#[derive(Debug, Clone)]
pub struct Instruction {
    pub offset: u32,
    pub opcode: u8,
    pub operands: Vec<Operand>,
    pub length: u32,
}
