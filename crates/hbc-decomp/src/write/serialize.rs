// Full `.hbc` image serialization.
//
// Primary path: identity re-emit from `BytecodeFile::raw_bytes` (preserves
// overflow headers, debug info, packing). Mutation paths update raw bytes then
// rehash the footer.
//
// Secondary path (`create`): build a minimal legacy-layout image from scratch.

use crate::error::{Error, Result};
use crate::file::header::{
    LEGACY_BIGINT_MIN_VERSION, LEGACY_FUNCTION_SOURCE_MIN_VERSION, LEGACY_SEGMENT_ID_MIN_VERSION,
};
use crate::file::BytecodeFile;
use crate::format::{HeaderLayout, FLAG_OVERFLOWED};
use crate::opcode::BytecodeFormat;

use super::footer::{append_footer, rehash_footer};
use super::header_write::{write_function_header_legacy_small, write_legacy_header};
use super::patch::string_storage::{lay_out, Packing, StoredString, StringLayout};

#[derive(Debug, Clone, Default)]
pub struct SerializeOptions {
    // If true, drop debug info when writing (smaller / simpler). Currently a no-op
    // on the raw-preserving path; used by create.
    pub strip_debug_info: bool,
}

// Serialize a fully populated `BytecodeFile` to raw `.hbc` bytes.
//
// When `raw_bytes` is present, this is an identity (plus footer rehash) so
// overflowed function headers and debug info stay valid.
pub fn serialize_file(
    file: &BytecodeFile,
    _format: &BytecodeFormat,
    _options: &SerializeOptions,
) -> Result<Vec<u8>> {
    if let Some(raw) = &file.raw_bytes {
        let mut out = raw.clone();
        // Ensure file_length field matches actual length after rehash.
        rehash_footer(&mut out)?;
        // Update file_length in header (offset 32 for both layouts after magic+ver+hash).
        if out.len() >= 36 {
            let len = out.len() as u32;
            out[32..36].copy_from_slice(&len.to_le_bytes());
            // file_length is part of the hashed region, rehash again.
            rehash_footer(&mut out)?;
        }
        return Ok(out);
    }
    Err(Error::Write(
        "serialize_file: no raw_bytes on BytecodeFile, parse from disk first, or use create_minimal"
            .into(),
    ))
}

// Convenience: serialize and write to a path.
pub fn write_file(
    path: impl AsRef<std::path::Path>,
    file: &BytecodeFile,
    format: &BytecodeFormat,
    options: &SerializeOptions,
) -> Result<()> {
    let bytes = serialize_file(file, format, options)?;
    std::fs::write(path, bytes).map_err(Error::Io)
}

// Rebuild a complete image from `raw_bytes` after in-place mutations to the
// working buffer (function bodies / string storage). Always rehashes footer and
// updates file_length.
pub fn finalize_raw_image(mut buf: Vec<u8>) -> Result<Vec<u8>> {
    if buf.len() < 36 {
        return Err(Error::Write("image too small".into()));
    }
    // Provisional length without footer adjustment.
    let mut len = buf.len() as u32;
    // If trailing looks like a footer already, length includes it.
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

// Finalize a mutated image, adopt it, and hand the bytes back.
//
// Every patch op ends the same way, so this is the one place the write path
// commits a result -- which makes it the right place to enforce invariant I1
// (the structured model and the bytes agree) rather than hoping each op
// remembers to.
//
// **The model is refreshed from the finalized bytes, not hand-patched.** That is
// the whole point. I1 used to be maintained by hand: each op edited raw bytes and
// then separately updated some subset of `file.strings` / `file.header.*` /
// `file.function_headers`. Every op got that subset wrong. Adding a
// debug assertion here found, on its first run:
//
//   * `patch_function_bytes` rewrote every function's header bytes but left
//     `file.function_headers` entirely untouched, so after any `asm` /
//     `patch-function` / `inject-stub` the model reported the pre-edit body size
//     and offsets.
//   * `patch_string_replace` on a growing string shifted every function offset in
//     the bytes and none in the model.
//
// Both are exactly R5, and neither was visible to tests that only assert the
// bytes reparse. Reparsing here makes the model *derived*, so it cannot drift.
//
// It also retires **R1**, which was the highest-residual risk in the register:
// chaining a second op on a `file` whose `sections` were stale after the first
// resize silently computed offsets against the old layout and corrupted the
// image. `sections` is part of what gets refreshed, so I2's written contract
// ("re-parse before chaining") is now enforced by construction instead of by
// remembering. The tests that carefully re-parsed between ops no longer have to.
//
// Cost is one parse per write op: ~40ms on the 16.8 MB Equinox bundle, and nothing
// measurable on the fixtures. Callers were already told to pay it (I2) -- this
// just stops them having to remember.
//
// **And the header changes only where the op says it does** (R3, R14). Every op
// passes a `HeaderEdit`: how far it moved the debug section, and which counts and
// sizes it rewrote. The reparsed header must equal the old one with exactly those
// edits applied. A header field written at the wrong position -- the R3 hazard,
// since the legacy `debug_info_offset` sits at a version-dependent offset --
// shows up here as a neighbouring field that changed, or an offset that did not.
pub fn commit_image(file: &mut BytecodeFile, buf: Vec<u8>, edit: HeaderEdit) -> Result<Vec<u8>> {
    commit_image_checked(file, buf, edit, |_| Ok(()))
}

// `commit_image`, plus an op-specific check of the reparsed image. `file` is
// replaced only when the image reparses, keeps its header as `edit` allows *and*
// passes `check`, so a refused write leaves the caller's file as it was.
pub fn commit_image_checked(
    file: &mut BytecodeFile,
    buf: Vec<u8>,
    edit: HeaderEdit,
    check: impl FnOnce(&BytecodeFile) -> Result<()>,
) -> Result<Vec<u8>> {
    let out = finalize_raw_image(buf)?;
    // Note this also promotes "the finalized image reparses" from something the
    // individual op tests assert to something every write op checks, always.
    let refreshed = BytecodeFile::parse_auto(&out).map_err(|e| {
        Error::Write(format!(
            "write produced an image that does not reparse: {e}. The edit reached \
             finalization, so this is a malformed layout rather than a rejected input."
        ))
    })?;
    let mut expected = file.header.clone();
    edit.apply(&mut expected);
    expected.file_length = out.len() as u32;
    header_matches(&expected, &refreshed.header, "write")?;
    check(&refreshed)?;
    *file = refreshed;
    Ok(out)
}

/// What a write op may change in the file header. Everything else must come back
/// exactly as it was. `HeaderEdit::default()` changes nothing (`file_length` is
/// always checked against the image itself).
#[derive(Debug, Clone, Copy, Default)]
pub struct HeaderEdit {
    /// How far the op moved everything after its splice. `debug_info_offset`,
    /// when set, must move by exactly this: the debug section follows every
    /// region a write op splices.
    pub shift: i64,
    /// New `(string_kind_count, identifier_count, string_count)`.
    pub string_counts: Option<(u32, u32, u32)>,
    /// New `(overflow_string_count, string_storage_size)`.
    pub string_storage: Option<(u32, u32)>,
    /// New `reg_exp_storage_size`.
    pub reg_exp_storage_size: Option<u32>,
}

impl HeaderEdit {
    /// A splice that moved everything after it by `shift` bytes.
    pub fn shifted(shift: i64) -> Self {
        Self {
            shift,
            ..Self::default()
        }
    }

    fn apply(&self, h: &mut crate::format::BytecodeHeader) {
        if h.debug_info_offset != 0 {
            h.debug_info_offset = (h.debug_info_offset as i64 + self.shift) as u32;
        }
        if let Some((kinds, idents, strings)) = self.string_counts {
            (h.string_kind_count, h.identifier_count, h.string_count) = (kinds, idents, strings);
        }
        if let Some((overflow, storage)) = self.string_storage {
            (h.overflow_string_count, h.string_storage_size) = (overflow, storage);
        }
        if let Some(size) = self.reg_exp_storage_size {
            h.reg_exp_storage_size = size;
        }
    }
}

// Compare two headers and name every field that differs.
fn header_matches(
    expected: &crate::format::BytecodeHeader,
    got: &crate::format::BytecodeHeader,
    what: &str,
) -> Result<()> {
    if expected == got {
        return Ok(());
    }
    let (e, g) = (format!("{expected:#?}"), format!("{got:#?}"));
    let diffs: Vec<String> = e
        .lines()
        .zip(g.lines())
        .filter(|(a, b)| a != b)
        .map(|(a, b)| format!("expected {} got {}", a.trim(), b.trim()))
        .collect();
    Err(Error::Write(format!(
        "{what} changed the file header beyond what it declares: {}",
        diffs.join("; ")
    )))
}

// ---- create-from-scratch helpers ----

// The string tables `create` emits, laid out as a string-table rebuild lays them
// out, unpacked (`create` is upstream's non-optimising path): a string too long
// for a small entry, or too far into storage, gets an overflow record, and a
// non-ASCII one is stored as UTF-16. Both used to be refused or mis-encoded.
fn create_string_tables(strings: &[String]) -> Result<StringLayout> {
    let stored: Vec<StoredString> = strings.iter().map(|s| StoredString::encode(s)).collect();
    lay_out(&stored, &Default::default(), Packing::Unpacked)
}

// Small table, overflow table, storage: the sections between the identifier
// hashes and the buffers, each 4-aligned.
fn push_string_tables(buf: &mut Vec<u8>, tables: &StringLayout) {
    for e in &tables.small {
        buf.extend_from_slice(&e.to_le_bytes());
    }
    align4(buf);
    for (off, len) in &tables.overflow {
        buf.extend_from_slice(&off.to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
    }
    align4(buf);
    buf.extend_from_slice(&tables.storage);
    align4(buf);
}

// Build a minimal v96-style legacy HBC: one global function with the given body
// and string table. Used by `create` and tests.
pub fn build_minimal_legacy(
    version: u32,
    strings: &[String],
    global_body: &[u8],
) -> Result<Vec<u8>> {
    // Legacy layout only. `create` emits modern layout for v97 and newer, which
    // `build_minimal_modern` handles; `create_minimal` dispatches there before
    // reaching this builder, so hitting this guard means `build_minimal_legacy`
    // was called directly with a modern version.
    if version >= 97 {
        return Err(Error::Write(
            "build_minimal_legacy: v97 and newer use modern layout (build_minimal_modern)".into(),
        ));
    }

    // Header is written last-pass with correct sizes; build body sections first.
    let tables = create_string_tables(strings)?;

    // String kinds: one run of String kind covering all strings.
    let string_kind_raw: u32 = strings.len() as u32; // high bit 0 = String

    // Function header (legacy small, 16 bytes), offsets filled after layout.
    // We'll place bytecode right after tables.
    // Placeholder 128-byte header, filled after layout.
    let mut buf = vec![0u8; 128];

    // function headers: 1 function
    let func_header_pos = buf.len();
    buf.extend_from_slice(&[0u8; 16]); // filled later

    // string kinds (1 entry)
    buf.extend_from_slice(&string_kind_raw.to_le_bytes());
    align4(&mut buf);

    // identifier hashes: empty
    align4(&mut buf);

    push_string_tables(&mut buf, &tables);

    // array buffer empty, obj keys empty, obj values empty
    // bigints empty for version with bigint fields, still emit zero-size sections
    // regexp empty, cjs empty

    // For v96: after string storage:
    // array, obj_key, obj_value, bigint table, bigint storage, regexp table, regexp storage,
    // cjs, (function source if v>=84)

    // All empty sections still need correct alignment, sizes in header are 0 so we write nothing.

    if version >= LEGACY_FUNCTION_SOURCE_MIN_VERSION {
        // function_source_table empty
    }

    let instruction_offset = buf.len() as u32;
    buf.extend_from_slice(global_body);

    // Fill function header at func_header_pos
    let fh = write_function_header_legacy_small(
        instruction_offset,
        1, // param_count (this)
        global_body.len() as u32,
        0, // function_name string index "global" if present
        0, // info_offset
        2, // frame_size
        0, // env
        0,
        0,
        // ProhibitInvoke = None and nothing else. This was 0x12, whose 0x10 is
        // hasDebugInfo: on an image with no debug section, the v96 engine went
        // looking for this function's debug info the first time it built a stack
        // trace and died with "LLVM ERROR: overflow past end of bytecode".
        crate::format::PROHIBIT_NONE,
    );
    buf[func_header_pos..func_header_pos + 16].copy_from_slice(&fh);

    // Write header fields
    write_legacy_header(
        &mut buf,
        version,
        [0u8; 20],
        0, // file_length filled later
        0, // global_code_index
        1, // function_count
        1, // string_kind_count
        0, // identifier_count
        strings.len() as u32,
        tables.overflow.len() as u32,
        tables.storage.len() as u32,
        version >= LEGACY_BIGINT_MIN_VERSION,
        0,
        0, // bigint
        0,
        0, // regexp
        0, // array
        0, // obj_key
        0, // obj_value
        version >= LEGACY_SEGMENT_ID_MIN_VERSION,
        0, // segment
        0, // cjs
        version >= LEGACY_FUNCTION_SOURCE_MIN_VERSION,
        0, // function_source_count
        0, // debug_info_offset
        0, // options
    )?;

    append_footer(&mut buf)?;
    let len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

// Build a minimal Modern (v97+) HBC: one overflowed global function with the given
// body and string table. Every real v98 function is overflowed (the 12 byte small
// header is only a pointer to an out-of-line large header), so this mirrors what
// hermesc emits and exercises the layout the runtime actually validates. The
// section order is function_headers, string_kinds, identifier_hashes,
// small_string_table, overflow_string_table, string_storage, then the empty
// buffer/table sections, then the bytecode, then the large header.
pub fn build_minimal_modern(
    version: u32,
    strings: &[String],
    global_body: &[u8],
) -> Result<Vec<u8>> {
    use super::header_write::write_modern_header;

    if version < 97 {
        return Err(Error::Write(
            "build_minimal_modern: version must be 97 or newer".into(),
        ));
    }

    let tables = create_string_tables(strings)?;
    let string_kind_raw: u32 = strings.len() as u32; // high bit 0 = String

    // Placeholder 128 byte header, filled after layout.
    let mut buf = vec![0u8; 128];

    // function headers: 1 function (modern 12 byte small header), filled later.
    let func_header_pos = buf.len();
    buf.extend_from_slice(&[0u8; 12]);
    align4(&mut buf);

    // string kinds (1 entry)
    buf.extend_from_slice(&string_kind_raw.to_le_bytes());
    align4(&mut buf);

    // identifier hashes: empty
    align4(&mut buf);

    push_string_tables(&mut buf, &tables);

    // Empty modern buffer/table sections (literal_value_buffer, obj_key_buffer,
    // obj_shape_table, bigints, regexp, cjs, function_source) are all zero-sized.

    let instruction_offset = buf.len() as u32;
    buf.extend_from_slice(global_body);
    align4(&mut buf);

    // Out-of-line large header (FunctionInfo), 4 byte aligned. The u32 prefix is
    // the same in every supported modern version; the u8 tail is NOT -- v98 has a
    // NumCacheNewObject byte that v99 removed, so the header is 37 bytes on one
    // and 36 on the other and the flags byte lands in a different place. Writing
    // the v98 shape into a v99 file put `flags` one byte past where the engine
    // reads it, which made every created v99 image throw at entry (ProhibitInvoke
    // read as 0 = ProhibitCall). Hence the descriptor. See plan_guides/06_write/RISKS.md R15.
    let layout = crate::modern_layout::ModernLayout::for_version(version)?;
    let large_header_pos = buf.len() as u32;
    let mut large = vec![0u8; layout.large_size()];
    let mut put_u32 = |at: usize, v: u32| large[at..at + 4].copy_from_slice(&v.to_le_bytes());
    use crate::modern_layout as ml;
    put_u32(ml::MODERN_LARGE_OFFSET, instruction_offset);
    put_u32(ml::MODERN_LARGE_PARAM_COUNT, 1); // `this`
    put_u32(ml::MODERN_LARGE_LOOP_DEPTH, 0);
    put_u32(ml::MODERN_LARGE_BYTECODE_SIZE, global_body.len() as u32);
    put_u32(ml::MODERN_LARGE_FUNCTION_NAME, 0); // string index of "global"
    put_u32(ml::MODERN_LARGE_NUMBER_REG_COUNT, 0);
    put_u32(ml::MODERN_LARGE_NON_PTR_REG_COUNT, 1); // r0 holds undefined
    put_u32(ml::MODERN_LARGE_FRAME_SIZE, 1);
    // Cache-size bytes stay 0. The flags byte carries ProhibitInvoke in its low
    // two bits, and that enum is `Call = 0, Construct = 1, None = 2` -- so a
    // zeroed flags byte means "plain calls prohibited", not "no restrictions".
    // A callable global must set ProhibitNone explicitly.
    large[layout.large_flags_pos()] = crate::format::PROHIBIT_NONE;
    buf.extend_from_slice(&large);
    align4(&mut buf);

    // Small header points at the large header. large_ptr = (name << 24) | (ptr &
    // 0xffffff); the overflow bit (0x20) sits in the flags byte at byte 11.
    let mut small = [0u8; 12];
    let raw: u128 = ((large_header_pos as u128) & 0x00ff_ffff)
        | (((large_header_pos as u128) >> 24) << 46)
        | ((crate::format::FLAG_OVERFLOWED as u128) << 88);
    small.copy_from_slice(&raw.to_le_bytes()[..12]);
    buf[func_header_pos..func_header_pos + 12].copy_from_slice(&small);

    write_modern_header(
        &mut buf,
        version,
        [0u8; 20],
        1, // function_count
        1, // string_kind_count
        0, // identifier_count
        strings.len() as u32,
        tables.storage.len() as u32,
    )?;
    // write_modern_header leaves overflow_string_count (byte 56) at 0.
    buf[56..60].copy_from_slice(&(tables.overflow.len() as u32).to_le_bytes());

    append_footer(&mut buf)?;
    let len = buf.len() as u32;
    buf[32..36].copy_from_slice(&len.to_le_bytes());
    rehash_footer(&mut buf)?;
    Ok(buf)
}

fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

// Ensure a file has raw_bytes attached (clone instructions path fallback).
pub fn ensure_raw(file: &mut BytecodeFile) -> Result<()> {
    if file.raw_bytes.is_some() {
        return Ok(());
    }
    Err(Error::Write(
        "BytecodeFile has no raw_bytes; re-parse the input with parse_auto".into(),
    ))
}

pub fn section_offset(file: &BytecodeFile, name: &str) -> Option<u32> {
    file.sections
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.offset)
}

// Check whether any function header is overflowed (large header out-of-line).
// For Modern headers the overflow bit is reinstated by the parser (the on-disk
// large header does not carry it); see parse_large_header_modern.
pub fn has_overflowed_functions(file: &BytecodeFile) -> bool {
    file.function_headers.iter().any(|h| match h {
        crate::format::FunctionHeader::Legacy(l) => l.flags & FLAG_OVERFLOWED != 0,
        crate::format::FunctionHeader::Modern(m) => m.flags & FLAG_OVERFLOWED != 0,
    })
}

pub fn layout_is_legacy(file: &BytecodeFile) -> bool {
    matches!(file.header.layout, HeaderLayout::Legacy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::BytecodeFile;
    use crate::opcode::BytecodeFormat;
    use crate::write::footer::verify_footer;

    #[test]
    fn identity_serialize_v96() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/react-native/v96/expressions/generator/bytecode.hbc"
        );
        if !crate::write::corpus_fixture_present(path) {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let file = BytecodeFile::parse_auto(&bytes).unwrap();
        let format = BytecodeFormat::for_version(file.header.version).unwrap();
        let out = serialize_file(&file, &format, &SerializeOptions::default()).unwrap();
        assert!(verify_footer(&out));
        // Re-parse must succeed
        let re = BytecodeFile::parse_auto(&out).unwrap();
        assert_eq!(re.header.function_count, file.header.function_count);
        assert_eq!(re.strings.len(), file.strings.len());
    }

    fn plain() -> BytecodeFile {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/plain.v96.hbc");
        BytecodeFile::parse_auto(&std::fs::read(path).unwrap()).unwrap()
    }

    // R3: a header field written at the wrong position lands in a neighbour.
    // Here `regExpCount` is corrupted by an op that declares no header change.
    #[test]
    fn an_undeclared_header_change_is_refused() {
        let mut file = plain();
        let mut raw = file.raw_bytes.clone().unwrap();
        let pos = 72; // regExpCount on v96 (bigint fields present)
        assert_eq!(u32::from_le_bytes(raw[pos..pos + 4].try_into().unwrap()), 0);
        raw[pos..pos + 4].copy_from_slice(&1u32.to_le_bytes());
        let before = file.header.clone();
        let err = commit_image(&mut file, raw, HeaderEdit::default())
            .expect_err("an undeclared header change must be refused")
            .to_string();
        assert!(err.contains("reg_exp_count"), "{err}");
        assert_eq!(
            file.header, before,
            "a refused write leaves the file as it was"
        );
    }

    // A splice declares how far it moved the debug section; an image whose
    // offset did not move by exactly that is refused.
    #[test]
    fn a_debug_offset_that_did_not_move_is_refused() {
        let mut file = plain();
        assert_ne!(
            file.header.debug_info_offset, 0,
            "fixture carries debug info"
        );
        let raw = file.raw_bytes.clone().unwrap();
        let err = commit_image(&mut file, raw, HeaderEdit::shifted(4))
            .expect_err("a declared shift that did not happen must be refused")
            .to_string();
        assert!(err.contains("debug_info_offset"), "{err}");
    }
}
