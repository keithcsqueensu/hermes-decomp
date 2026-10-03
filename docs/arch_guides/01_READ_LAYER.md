# 01 — The read layer: bytes → `BytecodeFile`

> **Ownership.** *Owns* everything that turns a raw `.hbc` byte image into the typed
> in-memory model, plus the opcode/instruction model and text disassembly. *Delegates*
> turning that model into IR to [`02_IR.md`](02_IR.md), and writing a model back to bytes to
> [`06_WRITE_PATH.md`](06_WRITE_PATH.md). The read path's robustness *findings* (F1–F14, what
> degrades silently) live in `../plan_guides/01_read/RISKS.md`; this guide is the structural
> map, that one is the risk register.

Files: `file/` (`structure.rs`, `parser/*`), `format.rs`, `opcode.rs`, `disasm.rs`,
`io.rs`, `modern_layout.rs`, `constants.rs`, `inspect.rs`, plus generated code from `build.rs`.

---

## What it does

Turns a raw `.hbc` image (Hermes bytecode, versions 40–99) into a fully-typed
`BytecodeFile`: validate magic/version, decode the fixed header in one of two on-disk
shapes, walk every section in exact upstream order, decode the string table, function
headers, buffer regions, CJS/function-source tables, exception handlers and debug offsets,
and lazily decode per-function instruction streams. Its defining trait is that it
**degrades rather than fails** — malformed or hand-patched images still parse, and every
degradation is recorded as a structured `Diagnostic` instead of being swallowed.

## Key types

| Type | Where | Role |
|---|---|---|
| `BytecodeFile` | `file/structure.rs:182` | top-level parsed model: header, all section vectors, `diagnostics`, `unresolved_string_ids`, original `raw_bytes` |
| `BytecodeHeader` | `format.rs:250` | decoded fixed header; version-gated fields are `Option<u32>`; carries both layouts + `options_raw` |
| `BytecodeOptions` / `CjsModuleForm` | `format.rs:92`, `:207` | decodes the version-keyed `options` byte; the CJS bit selects which of two indistinguishable module-table forms the file holds |
| `HeaderLayout {Legacy, Modern}` / `FunctionHeaderLayout {Legacy16, Modern12}` | `format.rs:238`, `:244` | the two on-disk shapes |
| `FunctionHeader` (`Legacy`/`Modern`) | `format.rs:331` | per-function metadata + flag accessors (`is_overflowed`, `prohibit_invoke`…) |
| `Diagnostic` / `DebugInfoStatus` | `file/structure.rs:117`, `:70` | the "this read is not what it looks like" enums |
| `Instruction` + `Operand`/`OperandType`/`OperandValue` | `file/structure.rs:250`, `opcode.rs:52,8,24` | decoded instruction model |
| `BytecodeFormat` / `InstructionDef` | `opcode.rs:93`, `:77` | per-version opcode table |
| `ByteReader` | `io.rs:3` | the cursor all parsing runs through — LE reads, LEB128, bounds-checked `read_exact`, `align`, `capacity_hint` |

## Data flow

Entry point `BytecodeFile::parse_auto(bytes)` (`file/parser/mod.rs:15` → `parsing.rs:78`):

1. **Peek the version first** (`header.rs:20`) and pick the version-implied layout
   (`Modern` if version ≥ 97, else `Legacy`). The *other* layout is tried only as a
   **reported** fallback (`Diagnostic::LayoutFallback`) — see the gotcha below.
2. `parse_with_layout` (`parsing.rs:119`) reads magic/version/source-hash, dispatches to
   `parse_legacy_header`/`parse_modern_header` (`header.rs:32`, `:109`), seeks past the
   128-byte header.
3. Walk sections via `track_section` (records offset/size), in the exact order of Hermes'
   `visitBytecodeSegmentsInOrder`: common tables (function headers → string kinds →
   identifier hashes → small/overflow string tables → string storage), then
   layout-specific buffers (`parse_legacy_buffers` uses array/objValue buffers;
   `parse_modern_buffers` uses literalValue/shape tables), then trailing sections (regexp,
   CJS, function-source) in `parse_trailing_and_build` (`parsing.rs:308`).
4. Remaining bytes become `instructions`; the tail is split into
   `bytecode`/`function_info`/`debug_info`/`footer` pseudo-sections.

**Instruction decoding is deferred**: `decode_function_instructions` (`instructions.rs:6`)
slices a function body by header offset/size and reads opcode-by-opcode on demand.

Version gating is by constant: `LEGACY_BIGINT_MIN_VERSION=87`,
`LEGACY_SEGMENT_ID_MIN_VERSION=78`, `LEGACY_FUNCTION_SOURCE_MIN_VERSION=84`,
`MODERN_FUNCTION_HEADER_MIN_VERSION=97`.

## Opcode / instruction model

An `Instruction` is `{offset, opcode:u8, operands:Vec<Operand>, length}`. Decoding looks up
`format.definitions[opcode]` (a dense vec indexed by the opcode byte) and reads each
declared `OperandType` off the `ByteReader` (`opcode.rs:58` `OperandType::read`).

Per-version tables are **JSON, embedded at build time**: `build.rs` scans
`resources/bytecode/Bytecode<N>.json` and generates `format_json_for_version()` /
`available_versions()` via `include_str!`; `BytecodeFormat::from_json_str` (`opcode.rs:137`)
inflates them, filling gaps with `<invalid>` defs. A version with no exact table uses
`for_version_or_latest` (`opcode.rs:186`, nearest ≤ version) routed through
`BytecodeFile::resolve_format` (`file/parser/mod.rs:46`), which records
`Diagnostic::OpcodeTableSubstituted` because a wrong table silently mis-decodes. `build.rs`
also embeds per-version `Builtins<N>.json` and an FNV build fingerprint used for cache-keying.

## Disassembly

`disasm.rs` renders decoded instructions to text. Entry points `disassemble_function`
(`disasm.rs:29`) and `disassemble_all` (`:55`); `DisasmOptions` gates
offsets/labels/string-resolution/color. It computes branch-target labels
(`collect_label_offsets`, `:91`), renders registers as `rN`, jump targets as `L<addr>`, and
resolves string-id operands (`UInt*S`) against the string table to inline quoted literals.

## Notable design decisions / gotchas

- **Version is authoritative, not a tie-break.** Trying both layouts and accepting whichever
  parsed was measurably wrong (95/76 single-bit-flip fixtures parsed under the wrong
  layout). `parse_auto` asks the version first and *flags* any fallback.
- **Degrade-and-report, never crash.** `Diagnostic` covers footer SHA-1 mismatch, length
  mismatch, layout fallback, opcode-table substitution, invalid string storage, unreadable
  debug info; `warnings()`/`is_clean()` surface them. The lazy `unresolved_string_ids`
  counter (`structure.rs:219`) is the strongest signal of wrong-offset decoding (~93K hits
  when BigInt was once parsed before the array buffer).
- **Section order is load-bearing** — misordering shifts every later offset and corrupts
  literal string-ids.
- **Overflow / "large" headers.** When `FLAG_OVERFLOWED` is set, the inline header packs a
  pointer to an out-of-line header (Legacy `(info_offset<<16)|offset`; Modern
  `(functionName<<24)|(offset&0xffffff)`); the overflow bit is **re-instated** on the large
  header because Hermes zeroes it there (`function.rs:212,259`).
- **Modern function-header layout is version-keyed and allow-listed.**
  `ModernLayout::for_version` (`modern_layout.rs:87`) supports only v98 (37-byte large
  header) and v99 (36-byte); v97 and unknowns are **hard errors** — a wrong guess would emit
  a VM-misread file. This is the "refuse rather than approximate" principle.
- **Offset conventions:** a function's `offset` is absolute in the file;
  `decode_function_instructions` subtracts `instruction_offset`. All multi-byte reads are
  little-endian and bounds-checked; `capacity_hint` clamps allocations against corrupt counts.

## File map

| File | Purpose |
|---|---|
| `file/mod.rs` | re-exports `parser` + `structure` |
| `file/structure.rs` | core parsed types: `BytecodeFile`, section rows, `Diagnostic`, `Instruction` |
| `file/parser/mod.rs` | `BytecodeFile` methods: entry wrappers, `resolve_format`, section accessors |
| `file/parser/parsing.rs` | top-level orchestration: `parse_auto`, section walk, tail split, exception handlers |
| `file/parser/header.rs` | magic/version constants, `peek_version`, legacy/modern header decode |
| `file/parser/function.rs` | Legacy16 & Modern12 function-header bitfield decode; large/overflow headers |
| `file/parser/table.rs` | string-kind / string-table / shape / pair decoders; `decode_string_table` |
| `file/parser/buffer.rs` | lazy literal-buffer series decoding + `<string:N>` miss counting |
| `file/parser/helpers.rs` | BigInt / array / key / value buffer decode wrappers |
| `file/parser/instructions.rs` | per-function instruction stream decode |
| `format.rs` | header/function-header structs, `BytecodeOptions`, `CjsModuleForm`, flags, layout enums |
| `opcode.rs` | operand/instruction-def types, `BytecodeFormat` JSON loader + version fallback, builtins |
| `modern_layout.rs` | version-keyed Modern (v97+) large-header byte layout (allow-listed v98/v99) |
| `disasm.rs` | text disassembly of decoded instructions |
| `io.rs` | `ByteReader` — bounds-checked LE/LEB128 cursor |
| `constants.rs` | JS reserved words / transformation-method lists (used by later naming passes) |
| `inspect.rs` | CLI/MCP-shared structural-table dumps, function banners, call-graph rendering |
| `build.rs` | generates version→JSON opcode/builtin lookups and a build fingerprint |

> `constants.rs` and `inspect.rs` sit slightly outside the pure read path — `constants.rs`
> feeds later naming/transform passes, and `inspect.rs` is a presentation layer over the
> parsed model consumed by the frontends (guide 07).
