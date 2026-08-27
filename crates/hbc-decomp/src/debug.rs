// Parses the debug info section to extract:
// - Source locations (line/column mappings)
// - Scope descriptors (variable names and scope chain)
// - Textified callees (function call target names)
//
// **Everything here is version-keyed**, because upstream reshaped this section
// twice inside the range we support and bumped `BYTECODE_VERSION` for neither of
// the reshapes in a way a reader could detect. `DebugInfoHeader` is 28 bytes at
// v96, 20 at v97 and 16 at v98+; the location-stream encoding is different at v96
// and v98+; and the whole scope-descriptor / textified-callee / debug-string-table
// apparatus is a v96-era feature upstream deleted. Reading a v98 file with the v96
// shapes does not fail — it silently yields nothing, which is what it did here
// until this was keyed (R25). `tests/upstream_pin.rs` pins both sizes and the
// stream's bit layout against the checkouts so the next reshape is a test failure.
//
// Derived from upstream's own serializer and deserializer, not from a spec:
// `lib/BCGen/HBC/DebugInfo.cpp` (`DebugInfoGenerator::appendSourceLocations`) and
// `FunctionDebugInfoDeserializer`. See `docs/UNMODELED_REGIONS_PLAN.md`.

use crate::file::DebugInfoStatus;
use crate::error::Result;
use crate::io::ByteReader;
use std::collections::BTreeMap;

// `data[start..start+len]` if fully in bounds, else `None`. Uses u64 so huge
// header values can't overflow the index arithmetic.
fn slice_in_bounds(data: &[u8], start: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(start).ok()?;
    let len = usize::try_from(len).ok()?;
    let end = start.checked_add(len)?;
    data.get(start..end)
}

// `data[start..end]` if `start <= end <= data.len()`, else `None`.
fn slice_range(data: &[u8], start: u32, end: u32) -> Option<&[u8]> {
    if start > end {
        return None;
    }
    data.get(start as usize..end as usize)
}

#[derive(Debug, Clone, Default)]
pub struct DebugInfo {
    pub source_locations: BTreeMap<u32, Vec<SourceLocation>>,
    /// function id → the offset of its scope descriptor, from
    /// `DebugOffsets.scopeDescData`.
    ///
    /// **This is the function → scope link; the location stream's `scopeAddress` is
    /// a different thing that resembles it.** That field is the innermost scope
    /// live at one instruction, and upstream defaults it to the shared empty
    /// descriptor at offset 0 (`kMostCommonEntryOffset`) — measured on a fixture
    /// with a closure, four of five functions report 0 there while their real
    /// scopes are at 3, 6, 9 and 13. So a variable map built by scanning a stream
    /// is empty for most functions and right for the occasional one, which is the
    /// worst way to be wrong. v96 only: v97 moved the link to `lexicalData` and v98
    /// removed the scope table altogether.
    pub function_scopes: BTreeMap<u32, u32>,
    pub scope_descriptors: Vec<ScopeDescriptor>,
    pub textified_callees: BTreeMap<u32, String>,
    pub string_table: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SourceLocation {
    pub bytecode_offset: u32,
    pub line: u32,
    pub column: u32,
    pub scope_offset: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ScopeDescriptor {
    pub offset: u32,
    pub parent_offset: Option<u32>,
    pub flags: u32,
    pub names: Vec<String>,
}

impl ScopeDescriptor {
    pub fn is_inner_scope(&self) -> bool {
        self.flags & 1 != 0
    }

    pub fn is_dynamic(&self) -> bool {
        self.flags & 2 != 0
    }
}

/// How a version lays out the debug section, or `None` for a version this crate
/// does not model.
///
/// An allow-list rather than a best guess, the same habit as
/// `ModernLayout::for_version`: a version whose shapes have not been derived from a
/// checkout yields no debug info rather than debug info decoded with the wrong
/// ruler. v97 is deliberately absent — it never shipped (see the guide's
/// modern-layout note), and its stream encoding is documented in the plan only so
/// the *shape of the drift* is on record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugLayout {
    /// `sizeof(DebugInfoHeader)` — the number of `u32` fields, times four.
    pub header_size: u32,
    /// Whether the header delimits the v96-era sub-regions (scope descriptors,
    /// textified callees, debug string table). False from v97 on: upstream removed
    /// them, so the debug data is source-location streams and nothing else.
    pub has_lexical_regions: bool,
    /// Which location-stream encoding the version uses.
    pub stream: StreamEncoding,
}

/// The two location-stream encodings this crate decodes.
///
/// Both are "SLEB128 deltas until an address delta of -1", and they agree on
/// nothing else: the prologue length, the meaning of the low bits of the line
/// delta, and which fields are always present all differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEncoding {
    /// v96 and earlier. Prologue is `functionIndex, line, column`. Each entry is
    /// `adelta, ldelta, cdelta, scopeAddress, envReg, [sdelta]`, with bit 0 of
    /// `ldelta` marking the statement delta and the line delta in bits 1..
    Legacy,
    /// v98 and v99. Prologue is `functionIndex, line, column, envIdx`. Each entry
    /// is `adelta` — always applied — then `ldelta`, whose bit 0 says whether a
    /// location follows at all; bits 1 and 2 mark the statement and envIdx deltas,
    /// and the line delta is in bits 3..
    Modern,
}

impl DebugLayout {
    pub fn for_version(version: u32) -> Option<Self> {
        match version {
            // 7 u32 fields: the three counts plus three sub-region offsets plus the
            // data size.
            v if v <= 96 => Some(Self {
                header_size: 28,
                has_lexical_regions: true,
                stream: StreamEncoding::Legacy,
            }),
            // 4 u32 fields: the three counts plus the data size. v97's 5-field,
            // 20-byte header is real but unmodelled on purpose.
            98 | 99 => Some(Self {
                header_size: 16,
                has_lexical_regions: false,
                stream: StreamEncoding::Modern,
            }),
            _ => None,
        }
    }
}

/// One function's `DebugOffsets`, as far as this crate reads it.
///
/// Upstream's struct is 12 / 8 / 4 bytes at v96 / v97 / v98+, but the fields we
/// want do not move: `sourceLocations` is always first, and `scopeDescData` is
/// second where it exists at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct FunctionDebugOffsets {
    /// Offset of this function's location stream in the debug data region, or
    /// `u32::MAX` (`NO_OFFSET`) when it has none.
    pub source_locations: u32,
    /// Offset of this function's scope descriptor, v96 only.
    pub scope_desc_data: Option<u32>,
}

// Parsed Hermes `DebugInfoHeader` (up to 7 little-endian u32 fields; how many is
// version-keyed — see `DebugLayout`).
//
// The section layout is:
//   [DebugInfoHeader (28 bytes)]
//   [filename table: filename_count * 8 bytes ({offset,length}) + filename_storage_size bytes]
//   [file regions:   file_region_count * 12 bytes]
//   [debug data (debug_data_size bytes):
//       [0 .. scope_desc_offset)             source-location / line data
//       [scope_desc_offset .. callee_offset) scope descriptors
//       [callee_offset .. string_offset)     textified callees
//       [string_offset .. debug_data_size)   debug string table ]
//
// The three offsets are relative to the START OF THE DEBUG DATA, not to the
// section start, a distinction the old 3-field reader got wrong, which is the
// root cause of issue #4 (it read the filename/region counts as offsets).
#[derive(Debug, Clone)]
struct DebugInfoHeader {
    filename_count: u32,
    filename_storage_size: u32,
    file_region_count: u32,
    scope_desc_offset: u32,
    textified_callee_offset: u32,
    string_table_offset: u32,
    debug_data_size: u32,
}

impl DebugInfo {
    /// Parse the debug section.
    ///
    /// `offsets` maps a function id to its parsed `DebugOffsets` — the only index
    /// into the location streams and the scope table. Without it the streams are
    /// unaddressable, which is why `source_locations` was permanently empty before
    /// P1 (DI1). Functions with no debug info are simply absent from the map.
    pub fn parse(
        bytes: &[u8],
        debug_info_offset: u32,
        version: u32,
        offsets: &BTreeMap<u32, FunctionDebugOffsets>,
    ) -> Result<Self> {
        Self::parse_with_status(bytes, debug_info_offset, version, offsets).0
    }

    /// As `parse`, but says *why* when the answer is empty.
    ///
    /// Five separate situations used to collapse onto one `Ok(default())` --
    /// "the file has none", "the offset is past EOF", "we do not model this
    /// version", "the header's own offsets leave the file", and "the streams
    /// failed to parse" -- leaving every caller unable to distinguish a stripped
    /// release build from a version this crate cannot read. They are now
    /// distinguishable; the returned `DebugInfo` is unchanged in every case.
    pub fn parse_with_status(
        bytes: &[u8],
        debug_info_offset: u32,
        version: u32,
        offsets: &BTreeMap<u32, FunctionDebugOffsets>,
    ) -> (Result<Self>, DebugInfoStatus) {
        if debug_info_offset == 0 || debug_info_offset == u32::MAX {
            return (Ok(Self::default()), DebugInfoStatus::Absent);
        }

        let offset = debug_info_offset as usize;
        if offset >= bytes.len() {
            return (Ok(Self::default()), DebugInfoStatus::OffsetOutOfRange);
        }

        // An unmodelled version yields nothing rather than nonsense: reading a v98
        // section with v96's 28-byte header is exactly R25, and it produced an
        // empty result that looked like "this file has no debug info".
        let Some(layout) = DebugLayout::for_version(version) else {
            return (
                Ok(Self::default()),
                DebugInfoStatus::UnsupportedVersion(version),
            );
        };

        let mut reader = ByteReader::new(&bytes[offset..]);
        let header = match Self::parse_header(&mut reader, layout) {
            Ok(h) => h,
            Err(e) => return (Err(e), DebugInfoStatus::ParseFailed),
        };

        // Where the debug-data blob begins, relative to the section start.
        // Every term is bounded by header values; use u64 + saturating math so
        // a corrupt header can never overflow or index out of range.
        let data_start = layout.header_size as u64
            + (header.filename_count as u64).saturating_mul(8)
            + header.filename_storage_size as u64
            + (header.file_region_count as u64).saturating_mul(12);
        let section = &bytes[offset..];
        let Some(data) = slice_in_bounds(section, data_start, header.debug_data_size as u64) else {
            // Header points past the file: treat as "no debug info" rather than
            // failing the whole bytecode parse.
            return (Ok(Self::default()), DebugInfoStatus::HeaderOutOfRange);
        };

        let mut debug_info = DebugInfo::default();

        // The location streams, one per function that has one. This is the half
        // that was missing: the streams were always there, and nothing knew where
        // any of them started.
        for (&function_id, entry) in offsets {
            if entry.source_locations == u32::MAX {
                continue;
            }
            if let Some(locs) =
                Self::parse_location_stream(data, entry.source_locations, layout.stream)
            {
                if !locs.is_empty() {
                    debug_info.source_locations.insert(function_id, locs);
                }
            }
        }

        // The scope link, where the version has one.
        for (&function_id, entry) in offsets {
            if let Some(scope) = entry.scope_desc_data {
                if scope != u32::MAX {
                    debug_info.function_scopes.insert(function_id, scope);
                }
            }
        }

        // Everything below this point is v96-era and was removed upstream by v97,
        // so the header does not delimit it and the offsets to read it do not
        // exist. Reading it anyway is how the old parser produced garbage regions
        // on a modern file.
        if !layout.has_lexical_regions {
            return (Ok(debug_info), DebugInfoStatus::Present);
        }

        // Parse the string table first: scope descriptors and callees refer to
        // their names by index into it.
        // Two views of the same region: the decoded list (for display) and the raw
        // bytes, which is what name references actually address.
        let string_data = slice_range(data, header.string_table_offset, header.debug_data_size)
            .unwrap_or(&[]);
        debug_info.string_table = Self::parse_string_table(string_data);

        if let Some(scope_data) = slice_range(
            data,
            header.scope_desc_offset,
            header.textified_callee_offset,
        ) {
            debug_info.scope_descriptors = Self::parse_scope_descriptors(scope_data, string_data);
        }

        if let Some(callee_data) = slice_range(
            data,
            header.textified_callee_offset,
            header.string_table_offset,
        ) {
            debug_info.textified_callees = Self::parse_textified_callees(callee_data, string_data);
        }

        (Ok(debug_info), DebugInfoStatus::Present)
    }

    fn parse_header(reader: &mut ByteReader<'_>, layout: DebugLayout) -> Result<DebugInfoHeader> {
        // The three counts lead at every version; what follows them is what changed.
        let filename_count = reader.read_u32()?;
        let filename_storage_size = reader.read_u32()?;
        let file_region_count = reader.read_u32()?;
        let (scope_desc_offset, textified_callee_offset, string_table_offset) =
            if layout.has_lexical_regions {
                (reader.read_u32()?, reader.read_u32()?, reader.read_u32()?)
            } else {
                (0, 0, 0)
            };
        Ok(DebugInfoHeader {
            filename_count,
            filename_storage_size,
            file_region_count,
            scope_desc_offset,
            textified_callee_offset,
            string_table_offset,
            debug_data_size: reader.read_u32()?,
        })
    }

    /// Decode one function's location stream, starting at `offset` into the debug
    /// **data** region (not the section).
    ///
    /// Returns `None` only when the offset is out of range. A stream that decodes
    /// to no entries returns an empty vector — that is a real state, not a failure:
    /// the prologue alone describes the function's opening line.
    fn parse_location_stream(
        data: &[u8],
        offset: u32,
        encoding: StreamEncoding,
    ) -> Option<Vec<SourceLocation>> {
        let mut reader = ByteReader::new(data.get(offset as usize..)?);

        // Prologue. The function index is read and dropped: the caller already
        // knows which function this stream belongs to, having followed that
        // function's DebugOffsets to get here. Upstream cross-checks the two; a
        // mismatch would mean the DebugOffsets are wrong, which is worth surfacing
        // one day but is not this function's business.
        let _function_index = reader.read_sleb128().ok()?;
        let mut line = reader.read_sleb128().ok()?;
        let mut column = reader.read_sleb128().ok()?;
        if encoding == StreamEncoding::Modern {
            // envIdx — present in the prologue from v98 on, and easy to miss:
            // skipping it shifts every subsequent read by one SLEB128 and decodes
            // the whole stream into plausible nonsense.
            let _env_idx = reader.read_sleb128().ok()?;
        }

        let mut address: i64 = 0;
        // The prologue itself is a location: upstream seeds its iteration with it
        // (`lastLocation = fdid.getCurrent()`), so the function's opening line is
        // reachable at address 0.
        let mut out = vec![SourceLocation {
            bytecode_offset: 0,
            line: line.max(0) as u32,
            column: column.max(0) as u32,
            scope_offset: None,
        }];

        // A malformed stream must not spin: every iteration consumes at least one
        // byte, and the reader runs out, but bound it anyway.
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            let Ok(address_delta) = reader.read_sleb128() else {
                break;
            };
            if address_delta == -1 {
                break;
            }
            match encoding {
                StreamEncoding::Legacy => {
                    let (Ok(mut line_delta), Ok(column_delta), Ok(scope_address), Ok(_env_reg)) = (
                        reader.read_sleb128(),
                        reader.read_sleb128(),
                        reader.read_sleb128(),
                        reader.read_sleb128(),
                    ) else {
                        break;
                    };
                    // Read the conditional statement delta *before* shifting: bit 0
                    // is the marker and the writer emits the delta last.
                    if line_delta & 1 != 0 && reader.read_sleb128().is_err() {
                        break;
                    }
                    line_delta >>= 1;

                    address += address_delta;
                    line += line_delta;
                    column += column_delta;
                    out.push(SourceLocation {
                        bytecode_offset: address.max(0) as u32,
                        line: line.max(0) as u32,
                        column: column.max(0) as u32,
                        // Absolute, not a delta, and only at v96: this is the link
                        // into the scope-descriptor table that makes variable names
                        // reachable (DI1).
                        scope_offset: u32::try_from(scope_address).ok(),
                    });
                }
                StreamEncoding::Modern => {
                    // The address advances on *every* entry, including one that
                    // carries no location. Two cursors, not one: `address` moves
                    // here, while line/column only move inside the branch below.
                    // Collapsing them silently corrupts every line from the first
                    // location-less entry onward.
                    address += address_delta;

                    let Ok(mut line_delta) = reader.read_sleb128() else {
                        break;
                    };
                    if line_delta & 1 == 0 {
                        continue;
                    }
                    let Ok(column_delta) = reader.read_sleb128() else {
                        break;
                    };
                    if line_delta & 2 != 0 && reader.read_sleb128().is_err() {
                        break;
                    }
                    if line_delta & 4 != 0 && reader.read_sleb128().is_err() {
                        break;
                    }
                    line_delta >>= 3;

                    line += line_delta;
                    column += column_delta;
                    out.push(SourceLocation {
                        bytecode_offset: address.max(0) as u32,
                        line: line.max(0) as u32,
                        column: column.max(0) as u32,
                        // v97 moved the per-location scope link to `lexicalData`
                        // and v98 dropped it entirely, so there is nothing to fill
                        // this with. Do not invent one.
                        scope_offset: None,
                    });
                }
            }
        }

        Some(out)
    }

    /// Resolve a name reference inside a scope descriptor or callee table.
    ///
    /// The value is a **byte offset into the debug string table region**, not an
    /// index into a list of strings: upstream's `appendString` writes
    /// `stringTable_.size()` at the moment the string was first appended, and
    /// `decodeString` seeks there and reads a LEB128 length followed by the bytes.
    ///
    /// Treating it as an index — which this did until P1 — resolves the *first*
    /// string correctly, because offset 0 and index 0 coincide, and yields nothing
    /// for every other one. On a scope with three captured variables that produced
    /// `["alpha", "", ""]`, which reads as "Hermes only named one of them" rather
    /// than as a decode bug.
    fn name_at_offset(table_data: &[u8], offset: i64) -> Option<String> {
        let at = usize::try_from(offset).ok()?;
        let mut reader = ByteReader::new(table_data.get(at..)?);
        let len = reader.read_sleb128().ok()?;
        let len = usize::try_from(len).ok()?;
        let bytes = reader.read_bytes(len).ok()?;
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn parse_scope_descriptors(data: &[u8], string_data: &[u8]) -> Vec<ScopeDescriptor> {
        let mut descriptors = Vec::new();
        let mut reader = ByteReader::new(data);
        let mut current_offset = 0u32;

        while reader.remaining() > 0 {
            let start_pos = reader.position();

            // A malformed/mis-aligned section can desync here; bail on the first
            // read error or implausible count rather than panic or loop wildly.
            let Ok(parent_raw) = reader.read_sleb128() else {
                break;
            };
            let Ok(flags) = reader.read_sleb128() else {
                break;
            };
            let Ok(name_count) = reader.read_sleb128() else {
                break;
            };
            if !(0..=reader.remaining() as i64).contains(&name_count) {
                break;
            }

            let mut names = Vec::new();
            for _ in 0..name_count {
                let Ok(name_idx) = reader.read_sleb128() else {
                    break;
                };
                // An unresolvable offset yields an empty name rather than a wrong
                // one; it means the region or the descriptor is malformed.
                names.push(Self::name_at_offset(string_data, name_idx).unwrap_or_default());
            }

            // Hermes encodes "no parent" as the u32 sentinel (all ones).
            let parent_offset = if parent_raw < 0 || parent_raw >= u32::MAX as i64 {
                None
            } else {
                Some(parent_raw as u32)
            };

            descriptors.push(ScopeDescriptor {
                offset: current_offset,
                parent_offset,
                flags: flags as u32,
                names,
            });

            current_offset += (reader.position() - start_pos) as u32;
        }

        descriptors
    }

    fn parse_textified_callees(data: &[u8], string_data: &[u8]) -> BTreeMap<u32, String> {
        let mut callees = BTreeMap::new();
        let mut reader = ByteReader::new(data);

        let Ok(count) = reader.read_sleb128() else {
            return callees;
        };
        if !(0..=reader.remaining() as i64).contains(&count) {
            return callees;
        }

        for _ in 0..count {
            let (Ok(address), Ok(name_idx)) = (reader.read_sleb128(), reader.read_sleb128()) else {
                break;
            };
            if let Some(name) = Self::name_at_offset(string_data, name_idx) {
                callees.insert(address as u32, name);
            }
        }

        callees
    }

    // The debug string table is a run of length-prefixed strings filling the
    // region, there is no leading count.
    fn parse_string_table(data: &[u8]) -> Vec<String> {
        let mut strings = Vec::new();
        let mut reader = ByteReader::new(data);
        while reader.remaining() > 0 {
            match reader.read_length_prefixed_string() {
                Ok(s) => strings.push(s),
                Err(_) => break,
            }
        }
        strings
    }

    pub fn build_variable_map(&self, function_scope_offset: Option<u32>) -> BTreeMap<u32, String> {
        let mut var_map = BTreeMap::new();

        if let Some(scope_offset) = function_scope_offset {
            if let Some(scope) = self
                .scope_descriptors
                .iter()
                .find(|s| s.offset == scope_offset)
            {
                for (i, name) in scope.names.iter().enumerate() {
                    if !name.is_empty() {
                        var_map.insert(i as u32, name.clone());
                    }
                }
            }
        }

        var_map
    }

    /// The variable names in scope for `function_id`, keyed by register index.
    ///
    /// This is what DI1's consumers want, and the reason it is a method rather than
    /// something they assemble: getting from a function to its scope is one lookup
    /// they were doing wrong, via the location stream, for as long as the feature
    /// existed. Empty when the version has no scope table, when the function has no
    /// entry, or when the scope genuinely names nothing — which is the common case,
    /// because Hermes only records a name for a *captured* variable. Plain locals
    /// live in registers and never appear here at any optimization level.
    pub fn variable_map_for_function(&self, function_id: u32) -> BTreeMap<u32, String> {
        self.build_variable_map(self.function_scopes.get(&function_id).copied())
    }

    pub fn all_variable_names(&self) -> Vec<&str> {
        self.scope_descriptors
            .iter()
            .flat_map(|s| s.names.iter().map(|n| n.as_str()))
            .filter(|n| !n.is_empty())
            .collect()
    }
}

pub fn try_parse_debug_info(
    bytes: &[u8],
    debug_info_offset: u32,
    version: u32,
    offsets: &BTreeMap<u32, FunctionDebugOffsets>,
) -> (Option<DebugInfo>, DebugInfoStatus) {
    let (result, status) = DebugInfo::parse_with_status(bytes, debug_info_offset, version, offsets);
    match result {
        Ok(info) => (Some(info), status),
        // `parse_with_status` already labelled why; do not relabel an error as
        // absence, which is what `.ok()` used to do.
        Err(_) => (None, status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_debug_info() {
        let info = DebugInfo::parse(&[], 0, 96, &BTreeMap::new()).unwrap();
        assert!(info.scope_descriptors.is_empty());
        assert!(info.textified_callees.is_empty());
    }

    #[test]
    fn test_invalid_offset() {
        let info = DebugInfo::parse(&[0u8; 100], u32::MAX, 96, &BTreeMap::new()).unwrap();
        assert!(info.scope_descriptors.is_empty());
    }

    // Build a complete, well-formed Hermes debug section: the 7-field header, a
    // real filename table (`{offset,length}` entries + concatenated storage)
    // and a file-region table, then the debug-data blob (scope descriptors,
    // textified callees, string table). This lays the section out exactly like
    // a real bundle, so it exercises the data-start arithmetic
    // (`28 + 8*filenameCount + filenameStorageSize + 12*fileRegionCount`),     // not a degenerate empty-table shortcut. The section offsets are derived
    // from the actual blob sizes. Returns (full buffer, offset for `parse`).
    fn build_debug_section(
        filenames: &[&str],
        file_regions: u32,
        scope_data: &[u8],
        callee_data: &[u8],
        string_table: &[u8],
    ) -> (Vec<u8>, u32) {
        // Filename table: an {offset, length} pair per name, then the storage.
        let mut storage = Vec::new();
        let mut entries = Vec::new();
        for name in filenames {
            entries.push((storage.len() as u32, name.len() as u32));
            storage.extend_from_slice(name.as_bytes());
        }

        // Offsets are relative to the start of the debug-data blob.
        let scope_off = 0u32;
        let callee_off = scope_data.len() as u32;
        let string_off = callee_off + callee_data.len() as u32;
        let data_size = string_off + string_table.len() as u32;

        let mut bytes = vec![0u8; 4]; // prefix so the offset is non-zero
        for v in [
            filenames.len() as u32,
            storage.len() as u32,
            file_regions,
            scope_off,
            callee_off,
            string_off,
            data_size,
        ] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        // Filename table: entries, then storage.
        for (off, len) in &entries {
            bytes.extend_from_slice(&off.to_le_bytes());
            bytes.extend_from_slice(&len.to_le_bytes());
        }
        bytes.extend_from_slice(&storage);
        // File regions: file_regions * 12 bytes (contents irrelevant here).
        bytes.extend(vec![0u8; file_regions as usize * 12]);
        // Debug data: scope descriptors, callees, string table.
        bytes.extend_from_slice(scope_data);
        bytes.extend_from_slice(callee_data);
        bytes.extend_from_slice(string_table);
        (bytes, 4)
    }

    #[test]
    fn test_parses_real_section_with_filenames_and_regions() {
        // One scope: parent=-1 (0x7f), flags=0, name_count=1, name_idx=0 -> "hi".
        let scope = [0x7f, 0x00, 0x01, 0x00];
        let strings = [0x02, b'h', b'i']; // string table: one entry "hi"
        // Two filenames + one file region so data_start =
        // 28 + 8*2 + len("app.js"=6)+len("b.js"=4) + 12*1 = 28+16+10+12 = 66.
        let (bytes, off) = build_debug_section(&["app.js", "b.js"], 1, &scope, &[], &strings);
        let info = DebugInfo::parse(&bytes, off, 96, &BTreeMap::new()).unwrap();
        assert_eq!(info.string_table, vec!["hi".to_string()]);
        assert_eq!(info.scope_descriptors.len(), 1);
        assert_eq!(info.scope_descriptors[0].names, vec!["hi".to_string()]);
        assert_eq!(info.scope_descriptors[0].parent_offset, None);
    }

    // Regression for issue #4: a mis-located/short debug section (as happens on
    // v96 bundles) used to decode an absurd name count and panic in
    // Vec::with_capacity ("capacity overflow"). Parsing must now degrade to a
    // clean result instead, never panicking, for any input.
    #[test]
    fn test_malformed_debug_info_never_panics() {
        // Poison name_count (-1) in the scope region of an otherwise valid section.
        let scope = [0x7f, 0x00, 0x7f]; // parent=-1, flags=0, name_count=-1
        let (bytes, off) = build_debug_section(&["a.js"], 1, &scope, &[], &[]);
        let info = DebugInfo::parse(&bytes, off, 96, &BTreeMap::new()).expect("must not panic");
        assert!(info.scope_descriptors.is_empty());

        // Arbitrary garbage offsets / truncated buffers must not panic either.
        for len in [0usize, 1, 8, 28, 40] {
            let junk = vec![0xffu8; len];
            let _ = DebugInfo::parse(&junk, 1, 96, &BTreeMap::new());
            let _ = DebugInfo::parse(&junk, len as u32, 96, &BTreeMap::new());
        }
    }
}
