# Implementation Plan — Wire String into Instructions

> Status: **commands 1–2 implemented** on branch `feat/wire-string` (merged to main,
> PR #3). `retarget-string` (6 tests) and `patch-operand` (3 tests) are complete.
> Command 3 (`asm --auto-add-strings`) is deferred — independently useful convenience.
> File-path/line-number citations below are accurate as of the commit this doc was
> written against; re-grep before relying on an exact line.

## Problem statement

`add-string` (implemented on `feat/add-string`) appends a new string to the table and
returns its id. But that id is inert — no bytecode instruction references it. Today,
wiring a string into an instruction requires either a full HASM round-trip (dump the
entire function, hand-edit one quoted string, reassemble) or a raw Python byte write.
Both are manual and error-prone.

Three distinct operations cover all real-world cases in this project:

| Operation | What changes | Size of edit | CLI today |
|---|---|---|---|
| **retarget-string** | SmallStringTableEntry (4 bytes) | metadata-only | none |
| **patch-operand** | one operand inside a function body | 1–4 bytes in code | none |
| **asm auto-add** | string table + function body | table rebuild + body | partial (two manual steps) |

## Scope

**In scope:** all three commands, prioritized by value and independence.
**Out of scope:** adding new *instructions* (new opcodes, new call sites), changing
instruction *length* (opcode widening, e.g. `GetByIdShort` → `GetById`), or modifying
the overflow string table entries directly.

---

## Command 1 — `retarget-string` (highest priority)

### What it does

Copy the 4-byte `SmallStringTableEntry` at `--to-id` over the entry at `--from-id`, so
that `from_id` now resolves to the same storage bytes as `to_id`. This is the
metadata-only retarget technique used by the time-format patches (`H:mm` → `HH:mm`).
No table rebuild, no storage growth, no code change — 4 bytes + SHA-1 footer.

### Why it matters

It is the safest, smallest, and most elegant patch technique in the project (4 bytes, no
relayout, no code change), used by three live patches — yet it is the *only* technique
with no CLI form. `CLI_RECIPES.md` §3.16 states: "The three stage-2 retargets have no
CLI form."

### Design decisions

1. **By-id or by-value.** Accept `--from-id N --to-id M` (explicit) or
   `--from "H:mm" --to "HH:mm"` (lookup by value, first match). When by-value, error if
   either string is not found or is ambiguous (multiple matches).
2. **Overflow guard.** If `to_id`'s entry is an overflow sentinel (length 0xff or offset
   0x800000), copy the overflow table slot too — but for v1, refuse and suggest
   `patch-string` instead. Real retargets in this project always target small entries.
3. **Identifier hash.** If `from_id` is an Identifier and `to_id` is also an Identifier,
   copy the hash. If the kinds differ, warn — the caller is retargeting across
   string/identifier boundaries, which changes runtime semantics.
4. **No table rebuild.** This is the entire point — the file grows by 0 bytes.

### Library function

Add to `crates/hbc-decomp/src/write/patch/strings.rs`:

```rust
pub fn retarget_string(
    file: &mut BytecodeFile,
    _format: &BytecodeFormat,
    from_id: u32,
    to_id: u32,
    _opts: &PatchOptions,
) -> Result<Vec<u8>>
```

### Algorithm

1. Validate `from_id` and `to_id` are in range (`< string_count`).
2. Read `small_off = section_offset(file, "small_string_table")`.
3. Read the 4-byte entry at `small_off + to_id * 4`.
4. Clone `raw_bytes`; overwrite `[small_off + from_id * 4 .. +4]` with the copied entry.
5. If `from_id` is an Identifier and `to_id` is an Identifier, copy the identifier hash
   (compute `identifier_index` for both, copy 4 bytes at `ids_off + idx * 4`).
   If only one is an Identifier, emit a warning to stderr.
6. `finalize_raw_image` (SHA-1 footer).
7. Sync `file.strings[from_id].value = file.strings[to_id].value` and `.is_utf16`.
8. Return the image.

### CLI

```
hermes-decomp retarget-string input.hbc -o out.hbc --from-id 5 --to-id 42
hermes-decomp retarget-string input.hbc -o out.hbc --from "H:mm" --to "HH:mm"
```

### CLI wiring (3 files, mirror `patch-string`)

1. `cli_args.rs` — `Command::RetargetString { input, output, from_id, to_id, from, to,
   format_version, layout, function_layout }`.
2. `main.rs` — match arm → `commands::write_cmd::run_retarget_string(...)`.
3. `write_cmd.rs` — `run_retarget_string`: resolve by-value to by-id if needed, call
   `retarget_string`, write output.

### Tests

- **Basic retarget + reparse**: retarget string A to B, reparse, `strings[A].value == strings[B].value`.
- **Identifier hash copied**: retarget one identifier to another, hash at the from-id
  slot matches `hermes_identifier_hash(to_value)`.
- **Cross-kind warning**: retarget a String to an Identifier (or vice versa), confirm
  warning emitted (capture stderr or check return).
- **Overflow refused**: if either entry is overflowed, error returned.
- **All other strings unchanged**: `strings[i]` for `i != from_id` are identical.
- **File size unchanged**: `out.len() == raw.len()` (metadata-only, no growth).

### Effort: ~0.5 day

---

## Command 2 — `patch-operand` (medium priority)

### What it does

Rewrite a single string-id operand inside one instruction, identified by its absolute
byte offset in the file. Validates the instruction shape, resolves the new string by
value or id, and writes only the operand bytes. No function-body rebuild.

### Why it matters

This is the `checkin.screenBgBlack` and `clubHours` pattern — changing which property
name a `GetById` / `GetByIdShort` / `PutById` loads, without touching any other byte in
the function. Today this requires either a full HASM round-trip (safe but touches every
byte) or a raw Python write (fragile, no validation).

### Design decisions

1. **Address by file offset.** `--at 0xD83E27` points at the *opcode byte* of the
   instruction. The tool decodes that instruction, finds the string operand (there is
   exactly one for all ById/LoadConstString opcodes; `CreateRegExp` has two — require
   `--operand-index` in that case), and overwrites it.
2. **Address by function + instruction offset.** Alternative: `--function 42 --insn-offset 0x1A`.
   Computes `function_header.offset + insn_offset` to get the absolute address. More
   ergonomic for the `anchors.py` workflow where offsets are relative to function start.
3. **New value by string or id.** `--string "black"` looks up the string table for the
   id; `--string-id 72` uses the id directly. By-value fails if the string is not in
   the table (use `add-string` first).
4. **Width check.** If the new id exceeds the operand width (`UInt8S` max 255, `UInt16S`
   max 65535), error — suggest using the `Long` variant of the opcode, or `add-string`
   returned a lower id.
5. **No opcode widening.** Changing a `GetByIdShort` (UInt8S) to `GetById` (UInt16S)
   would change instruction length, requiring a full function-body rebuild. Out of scope;
   suggest `asm` for that case.
6. **Read-back verification.** After the write, decode the instruction again and verify
   the operand matches. Print the old → new values to stderr.

### Library function

Add to a new file `crates/hbc-decomp/src/write/patch/operands.rs`:

```rust
pub fn patch_string_operand(
    file: &mut BytecodeFile,
    format: &BytecodeFormat,
    target: OperandTarget,
    new_string_id: u32,
    opts: &PatchOptions,
) -> Result<Vec<u8>>

pub enum OperandTarget {
    /// Absolute byte offset of the opcode in the file.
    AbsoluteOffset(u32),
    /// Function id + relative offset within the function body.
    FunctionRelative { function: u32, insn_offset: u32 },
}
```

### Algorithm

1. Resolve `OperandTarget` to an absolute file offset `abs_off`.
   - `AbsoluteOffset`: use directly.
   - `FunctionRelative`: `abs_off = function_headers[function].offset() + insn_offset`.
2. Decode the single instruction at `abs_off` from `raw_bytes` using the format's opcode
   table (`format.decode_instruction_at(raw, abs_off)`). This needs a new helper or
   reuse of the per-instruction decode logic in `file/parser`.
3. Find the string operand(s): scan `insn.operands` for `UInt8S` / `UInt16S` / `UInt32S`.
   If exactly one, use it. If zero, error ("instruction has no string operand"). If
   multiple (e.g. `CreateRegExp`), require `--operand-index`.
4. Compute the byte position of that operand within the instruction: `abs_off + 1` (skip
   opcode byte) + sum of preceding operand byte widths.
5. Validate width: `new_string_id` must fit in the operand's byte width.
6. Clone `raw_bytes`, write `new_string_id` as LE bytes at the computed position.
7. `finalize_raw_image`.
8. Read-back: decode again, confirm operand value == `new_string_id`.
9. Print `old_id (old_value) → new_id (new_value)` to stderr.

### Decoding a single instruction at an arbitrary offset

The existing `decode_function_instructions` iterates the whole function body. For
`patch-operand` we need to decode exactly one instruction at a known offset. Options:

- **Simplest (recommended):** Read the opcode byte at `abs_off`, look up its operand
  signature in the format's opcode table, compute the instruction length, and extract
  operand values. This is ~30 lines of new code, essentially inlining what
  `decode_instruction` does for one instruction. Add as
  `BytecodeFormat::decode_one(raw: &[u8], offset: usize) -> Result<Instruction>`.
- **Alternative:** Call `decode_function_instructions` on the containing function and
  find the instruction at the target offset. Works but decodes the entire function body
  for one instruction.

### CLI

```
hermes-decomp patch-operand input.hbc -o out.hbc --at 0xD83E27 --string "black"
hermes-decomp patch-operand input.hbc -o out.hbc --function 42 --insn-offset 0x1A --string-id 72
```

### CLI wiring

1. `cli_args.rs` — `Command::PatchOperand { input, output, at, function, insn_offset,
   string, string_id, operand_index, format_version, layout, function_layout }`.
2. `main.rs` — match arm.
3. `write_cmd.rs` — `run_patch_operand`: resolve addressing mode, resolve string value
   to id if needed, call `patch_string_operand`.
4. `operands.rs` — new module under `write/patch/`, add to `mod.rs` and re-export chain.

### Tests

- **GetById swap + reparse**: patch a `GetById`'s string operand, reparse, disassemble
  the function, confirm the new string appears.
- **GetByIdShort swap**: same for the 1-byte variant.
- **LoadConstString swap**: change a string literal reference.
- **Width overflow refused**: try to set id 300 on a `UInt8S` operand → error.
- **Non-string instruction refused**: target a `Jmp` → "no string operand" error.
- **Read-back verification**: the decoded instruction after patching has the correct id.
- **Other instructions unchanged**: disassemble the full function before and after,
  only the target instruction differs.

### Effort: ~1–1.5 days

---

## Command 3 — `asm --auto-add-strings` (lower priority)

### What it does

When the HASM assembler encounters a quoted string literal that is not in the string
table, instead of erroring, it calls `add_string` to append it and then uses the new id.
This closes the gap between `add-string` and `asm` — the user writes the HASM with the
desired string value and the tool handles the table mutation.

### Why it matters

Today the workflow is: (1) `add-string` to get an id, (2) edit HASM to use that string,
(3) `asm` to assemble. Step 1 is disconnected from step 3. With auto-add, the user just
writes the HASM and runs `asm`.

### Design decisions

1. **Opt-in flag.** `--auto-add-strings` (default: off). The current "string not in
   table" error is a safety feature — it catches typos. Auto-add should be explicit.
2. **Identifier inference.** When auto-adding, how to decide String vs Identifier?
   Heuristic: if the operand type is on a `*ById*` opcode (property access), mark as
   Identifier; if on `LoadConstString`, mark as String. This matches the Hermes
   convention.
3. **Multiple adds in one pass.** If the HASM references three new strings, all three
   are added before the function body is assembled. The table grows by 3 entries, and
   the body is assembled against the updated table.
4. **Output includes both mutations.** The output `.hbc` has both the new strings and
   the new function body.

### Implementation sketch

Modify `parse_hasm_with_context` (`hasm/parse.rs:218`) or add a wrapper:

1. First pass: parse normally, collect "string not found" errors with the missing values
   and their operand contexts (opcode name → String or Identifier).
2. For each missing string, call `add_string(file, format, value, is_identifier, opts)`.
   This mutates `file` in place (the model is synced by `add_string`).
3. Rebuild the `string_lookup` HashMap from the updated `file.strings`.
4. Second pass: parse again — all strings now resolve.
5. Continue with `patch_function_body` as normal.

### CLI

```
hermes-decomp asm input.hbc func.hasm --function 5 -o out.hbc --auto-add-strings
```

### Tests

- **Auto-add one string**: HASM references a string not in the table, output has the
  string in the table and the function body uses its id.
- **Auto-add identifier vs string**: `GetById` context → Identifier, `LoadConstString`
  context → String.
- **Multiple auto-adds**: three new strings, all present in the output.
- **Without flag, still errors**: default behavior is preserved.

### Effort: ~0.5–1 day

---

## Sequencing

1. **`retarget-string`** — standalone, no dependencies, highest real-world value.
   Unblocks the time-format patches from needing Python.
2. **`patch-operand`** — standalone, depends only on a small `decode_one` helper.
   Unblocks the checkin/clubHours patterns from needing HASM round-trips.
3. **`asm --auto-add-strings`** — depends on `add_string` (already merged). A
   convenience that chains two existing operations.

Commands 1 and 2 can be developed in parallel (no shared code). Command 3 is
independent of both.

## Risks to validate

1. **Retarget across overflow entries.** V1 refuses overflow entries. If a real use case
   needs it, extend to copy the overflow slot too — but verify the overflow index
   embedded in the small entry is correct after the copy.
2. **`patch-operand` on modern v97+ files.** The function body offsets stored in
   overflowed large headers should not move (operand patching does not change any
   section size), so no relocation is needed. Verify with a modern fixture.
3. **Cache index operands on `*ById*` opcodes.** `GetById` has both a string operand
   (`UInt16S`) and a cache index operand (`UInt8`). The tool must patch only the string
   operand, not the cache index. The `S` suffix on `OperandType` distinguishes them —
   verify the operand-finding logic uses this correctly.
4. **HASM auto-add ordering.** If two HASM files are assembled in sequence with
   auto-add, the second file sees strings added by the first. The in-memory model
   sync in `add_string` handles this, but verify with a test.

## Total effort

~2–3 days across all three commands. Each is independently useful and independently
shippable.
