# Write path — engineering guide (invariants, hazards, open work)

Standing reference for the bytecode **write path** (`crates/hbc-decomp/src/write/`
+ `crates/hbc-decomp-cli/src/commands/write_cmd.rs`). Read this *before* adding a new
`patch-*` op, a `create` variant, a stub kind, or any code that mutates a `.hbc` image.
It is the "where write-path changes go wrong" map: the invariants every mutation must
hold, the design limits not to "fix" by accident, the high-risk areas, what the tests do
and don't cover, the legacy/modern fork, and the decided-but-unbuilt work.

Everything below is derived from the current code, not from the prose docs. Where the
code and the docs disagree, the disagreement is called out — the code is described as it
actually behaves. File:line references are to the state of the tree when this was written
(a working tree that has implemented the Q3/Q4 guard, Q5, Q6, Q8, Q9 and added the first
independent tests for `functions.rs`/`inject.rs`); re-check them if the code has moved.

Scope: the read/decompile path is out of scope except where a write op depends on it
(`decode_function_instructions`, `disassemble_function`).

---

## Status at a glance (work tracker)

A project-tracker view of the write path. Status labels: **✅ done** (implemented +
tested in CI), **🟢 done, VM-unverified** (implemented + unit-tested, but modern output is
only checked by the external verifier, never CI — see design limits), **🟡 interim** (a
guard/partial in place, full feature planned), **🔵 planned** (decided, not built),
**⚪ open** (a policy call left for Keith).

### Commands (all shipped)

| Command | Modern-aware | CI test | Status | Notes |
|---|---|---|---|---|
| `add-string` | Yes | Yes (v98) | ✅ | full modern branch (`strings.rs:544`) |
| `patch-string` same-length | Layout-agnostic | Yes (v96) | ✅ | in-place; `locate_string_bytes` via sections |
| `patch-string` resize | Yes | Yes (v96 grow) | 🟢 | modern debug-off=108/hsize=12/overflow relocate untested-on-VM |
| `patch-string --old` (replace) | Yes | Yes (v96) | ✅ | by-value lookup now tested (`strings.rs:860`) |
| `retarget-string` | Layout-agnostic | Yes (v96) | ✅ | small table + id hash only; refuses overflow |
| `patch-operand` | Layout-agnostic | Yes (v96) | ✅ | + Q9 property-name warning |
| `asm` / `patch-function` | Yes | Yes (v96 + v98) | 🟢 | grow/shrink/align/modern-resize now directly tested |
| `inject-stub` | Yes | Yes (v96 + v98) | 🟢 | legacy + modern LogEntry, NopPad, precondition errors |
| `create` | Yes | Yes (v96 + v98) | 🟢 | legacy ≤v96, modern ≥v97; single global, no overflow tables |
| `emit-hasm` | read-only | Yes (v96, v98 fixture) | ✅ | one emit→parse→assemble round-trip |
| `secrets` / `frida-hooks` | n/a (read) | — | ✅ | data to stdout / hooks file |
| `asm-check` (`run_roundtrip_check`) | Yes | No | ⚪ | no test (`write_cmd.rs:410`) |

### Open questions / decisions

| # | Topic | Status | Where |
|---|---|---|---|
| Q1 | `create` modern (v97+) + doc reconciliation | ✅ resolved, docs reconciled | Open questions |
| Q2 | Modern small-header offset 24 vs 25 bits | ✅ resolved — different fields, both correct | Open questions |
| Q3 | Exception-handler relocation on resize | 🟡 interim guard; 🔵 full relocation planned | Pending impl plans |
| Q4 | `HasmFunction.exception_handlers` | ✅ guard done; ⚪ build-vs-guard for Keith | Open questions |
| Q5 | Library functions writing to stderr | ✅ resolved — no; status returned to CLI | Open questions |
| Q6 | `encode_instruction` operand-type tolerance | ✅ resolved — no-op branch, safe | Open questions |
| Q7 | Identifier placement (leading/contiguous?) | ✅ resolved — no requirement | Open questions |
| Q8 | `AsyncBreakCheck` as universal pad | ✅ resolved — hard error when needed+absent | Open questions |
| Q9 | `patch-operand` `*ById` kind validation | ✅ resolved — warn only | Open questions |

### Remaining work, ranked

1. 🔵 **Q3 Phase 1 — inject-stub handler relocation** (single-point insertion). Feasible
   now; lets `inject-stub` drop the Q4 guard. See Pending impl plans.
2. 🔵 **Q3 Phase 2 — patch-function/asm handler relocation.** Blocked on HASM handler
   syntax (Q4's unbuilt feature).
3. ⚪ **Never-VM-verified modern paths.** Everything modern is 🟢, not ✅ — no CI test runs a
   VM. See design limits + Legacy/modern audit.
4. ⚪ **Test matrix gaps that remain** (below): modern-on-VM, `patch-string` shrink,
   identifier-resize hash refresh under resize, HASM error paths + handler round-trip,
   CLI argument-resolution tests. See Test matrix gaps.

---

## Write-path invariants

Contracts every mutation must satisfy. Numbered so future work can cite them.

**I1 — `raw_bytes` is the source of truth; the structured model is a hand-maintained
shadow.** Every patch op clones `file.raw_bytes`, edits bytes, finalizes, then reassigns
`file.raw_bytes = Some(out)`. `serialize_file` (`serialize.rs:28`) *errors* if `raw_bytes`
is `None`. The structured fields (`file.strings`, `file.header.*`, `file.function_headers`,
`file.string_kinds`, `file.identifier_hashes`) are updated **manually and partially** after
each edit. A new op that mutates bytes but forgets to sync the model — or updates the model
but not the bytes — leaves the two out of sync for the rest of the session.

**I2 — `file.sections` is NOT refreshed by resize ops. Re-parse before chaining.** The
resize/append paths (`add_string`, `patch_string_resize`, `patch_function_bytes`) move
section boundaries but never rewrite `file.sections`. Since `section_offset()`
(`serialize.rs:361`) reads `file.sections`, a *second* op on the same in-memory `file`
after a resize will compute offsets against the **old** layout and corrupt the image. The
tests work around this by re-parsing ("Re-parse so sections are fresh").
**Contract: after any size-changing op, `BytecodeFile::parse_auto` the returned bytes
before running another op.** `patch_function_bytes` even admits its instruction cache is
"roughly" updated (`functions.rs:238`).

**I3 — The footer is always the last 20 bytes = `sha1(image[:-20])`.** Enforced by
`footer.rs`. Every write path must end by routing through `finalize_raw_image`
(`serialize.rs:66`) or `serialize_file`/`append_footer`. A raw byte edit that skips
finalization ships a file that fails `verify_footer` and is rejected by the loader.

**I4 — `file_length` at bytes `[32..36]` counts the footer, and is inside the hashed
region.** This is why `finalize_raw_image` and `serialize_file` hash **twice**: rehash,
write `len = out.len()` (footer included) into `[32..36]`, rehash again. Any new finalizer
must replicate the double-hash or the length field's own bytes corrupt the hash.

**I5 — 4-byte alignment of everything after the code.** The FunctionInfo region (large
headers, exception tables, debug info) and `SwitchImm` jump tables are 4-aligned. **Size
deltas must be a multiple of 4.** `patch_function_body` (`functions.rs:18`) and
`build_log_entry` (`inject.rs:90`) pad with 1-byte `AsyncBreakCheck`; string-region
rebuilds pad storage to `%4`. A new resize op that emits a non-4-aligned delta silently
misaligns every downstream large header. **As of Q8, the pad paths hard-error** when
padding is required but the version lacks `AsyncBreakCheck` (rather than silently
misalign) — see Q8.

**I6 — On a size change, every downstream offset shifts by `delta`.** That set is:
each function's body offset, each function's info offset, `debug_info_offset` in the file
header, and — for **overflowed** functions — the packed large-header pointer in the small
header *plus* the large header's own body-offset / size / (legacy) info fields. Two distinct
relocation models exist and must not be confused:
  - **String-region growth** (`strings.rs`): the string region precedes all code, so
    **all** function offsets shift unconditionally by `delta`.
  - **Function-body growth** (`functions.rs`): only offsets `>= threshold`
    (`abs_off + old_size`, the end of the patched body) shift; the patched function's own
    offset is unchanged but its size field is rewritten (`resize_overflowed_function`,
    `functions.rs:277`).

**I7 — String encoding is chosen by content, never carried from the old flag.**
`needs_utf16 = value.bytes().any(|b| b > 0x7f)` (`strings.rs:366`, `:623`). Pure-ASCII →
one byte per char, length in bytes; otherwise UTF-16LE, length in **code units**. Patching
an ASCII entry to hold `é`/`€`/astral chars must flip it to UTF-16 (regression-tested).

**I8 — The only reliable overflow signal is small-entry length field `== 0xff`.** An
overflowed entry stores the overflow-table index in the offset field and `0xff` in the
length field; the real 32-bit offset+length live in the 8-byte overflow-table slot. The
`offset == 0x800000` check that appears in `locate_string_bytes`/`read_all_string_locs`
(`strings.rs:44`, `:124`) is **dead** — the offset field is masked to 23 bits
(`0x7f_ffff`) and can never equal `0x800000`. `retarget_string` correctly checks only
`len == 0xff` (`strings.rs:258`); a regression test guards this (`strings.rs:~1124`). New
overflow logic must key on `len == 0xff`, and encode overflow when `off >= 0x80_0000 ||
len_field >= 0xff`.

**I9 — Identifier hashes track identifier text and must be refreshed on any text change.**
Jenkins one-at-a-time over UTF-16 code units, seed 0 (`hermes_identifier_hash`,
`strings.rs:162`; verified against hermesc). `update_identifier_hash` (`strings.rs:188`)
covers same-length patch, resize, and retarget; `add_string` appends a fresh hash.
Identifier index = count of identifiers with lower string id (`identifier_index`,
`strings.rs:174`). A new op that alters an identifier's value without refreshing its hash
breaks every property lookup that hashes to it.

**I10 — String ids are append-only and stable.** `add_string` gives the new entry
`id = old string_count` and never renumbers (`strings.rs:544`); every instruction operand
that references an existing id stays valid. Any op that reorders or removes string entries
violates the assumption the entire instruction stream depends on.

**I11 — A string id written into an operand must fit that operand's width.**
`patch_string_operand` rejects ids exceeding `UInt8S`/`UInt16S`/`UInt32S` capacity
(`operands.rs:175`) and read-back-verifies the write (`operands.rs:199`). `build_log_entry`
guards `print_id`/`msg_id <= u16::MAX` for short `LoadConstString`. A new op that stuffs a
large id into a short operand needs the same guard, or the "Long" opcode variant.

**I12 — `string_kinds` is run-length encoded and appends extend the trailing run.**
`add_string` bumps the last run's count when kinds match, else pushes a new run. High bit
of each `u32` = Identifier, low 31 = count. This is only correct because the appended
string is the highest id (trailing position). An op that inserts a string of a different
kind anywhere but the end would need to split runs. (Interleaved runs are legal in the
format — hermesc itself emits them; see Q7.)

**I13 — Encode requires exact operand arity.** `encode_instruction` errors if
`operands.len() != def.operand_types.len()` (`encode.rs:12`). It **tolerates** an operand
*type* mismatch (`encode.rs:24` is a no-op branch); layout is always driven by the
definition's `expected_ty`, and `write_operand` range-checks every narrowing — see Q6.

---

## Known design limitations

Intentional constraints. Do not "fix" these without a deliberate decision — several are
load-bearing for keeping the crate pure-Rust or the edits surgical.

- **No string dedup/merge.** Rebuilt string storage is emitted *unpacked*, so patched
  files are larger than hermesc output (CLAUDE.md; `strings.rs` rebuild never re-packs).
  This is the mechanism that makes same-length-overlap patches safe (an overlapping entry
  gets its own storage on rebuild).
- **Debug info & RegExp are opaque `u8` buffers.** Not parsed into typed structs; resize
  ops shift `debug_info_offset` but never rewrite debug-info *internals*.
- **No JS recompilation.** The write path assembles HASM (our disasm dialect) and patches
  bytes; it does not recompile decompiled JavaScript (CONTRIBUTING.md scope note).
- **`apply_reloc` on structured headers is intentionally unimplemented** — it errors and
  points callers at `patch_function_bytes`/`finalize_raw_image` (`reloc.rs:23`).
  `RelocPlan` is a placeholder type for a future structured-rebuild path.
- **`retarget_string` refuses overflow entries** (v1 scope) and allows — but the CLI warns
  on — a string↔identifier cross-kind retarget (`strings.rs:258`; note moved to the CLI
  layer, see Q5).
- **`create` cannot emit overflow string entries.** A string with `len >= 0xff` or
  `offset >= 0x800000` is rejected (`serialize.rs:107`, `:246`). `create` is for minimal
  images, not arbitrary tables.
- **`inject-stub log` preconditions:** requires a `"print"` string already in the table,
  refuses overflowed **legacy** functions (`inject.rs:134`), and needs the version to
  expose `GetGlobalObject`/`TryGetById`/`LoadConstUndefined`/`LoadConstString`/`Call2`.
- **Modern output cannot be verified from Rust.** No C ABI to `hermesvm`; correctness of
  v97+ output is checked only by the external macOS C++ verifier
  (`scripts/build/build_hermes_v98_toolchain.sh`; USAGE.md "Why modern output cannot be
  verified"). Treat every modern write path as "verify externally or not at all." This is
  why every modern row in the tracker is 🟢, not ✅.
- **`create` produces a single global function** with hardcoded shape (legacy: flags
  `0x12`, frame 2, param 1 — `serialize.rs:179`; modern: `ProhibitNone` overflowed global
  — `serialize.rs:313`). It is a smoke-test artifact, not a general emitter.

---

## High-risk areas by category

**What the git history confirms empirically (see Git history findings).** The one bug class
that has actually shipped on this write path — four times in a single review, finding F1 — is
**missing input validation before a raw byte write**: a string id not checked against
`string_count`, an `insn_offset` not bounded by the body size, a masked field trusted as a
sentinel, and a `file.strings[x]` index taken before `x` was validated. Apply that checklist
to every new op first. Second empirical fact: the highest-severity areas below live in
files authored once as a monolith (`functions.rs`, `inject.rs`, `create.rs`, `serialize.rs`,
`header_write.rs`) — though `functions.rs` and `inject.rs` have now received their first
independent tests in the current pass (finding F5, updated).

### New string ops
- **Chaining without re-parse (I2).** The single most likely corruption: running a second
  string op against a `file` whose `sections` are stale after the first resize.
- **Overflow handling (I8).** Copying the dead `offset == 0x800000` check instead of
  `len == 0xff`; forgetting the 8-byte overflow-slot layout; forgetting to update
  `overflow_string_count` at `[56..60]` and `string_storage_size` at `[60..64]`.
- **Header field positions.** String counts sit at fixed offsets `[44..64]` shared across
  layouts, but `debug_info_offset` differs: **modern fixed at byte 108**, **legacy computed
  by `legacy_debug_info_offset_pos`** (`strings.rs:294`), which itself depends on
  version-gated fields (bigint present? function_source present?). A wrong legacy position
  writes garbage into a random header field with no immediate error.
- **`string_kinds` runs (I12)** and **identifier ordering/hash (I9).** Inserting rather
  than appending, or appending an identifier without extending the hash table + bumping
  `identifier_count`, desynchronizes the identifier hash index.
- **UTF-16-by-content (I7)** and **`%4` storage padding (I5).**
- **Model sync (I1).** Forgetting to push to `file.strings` / bump `file.header.*` leaves
  later reads (and the CLI's post-op status text) lying.

### New function ops
- **Alignment (I5).** Any body whose new length isn't `%4`-aligned relative to the old must
  be padded; the existing pad trick inserts `AsyncBreakCheck` *before the terminator*
  (`functions.rs:54`) so the function still ends on a terminator. When padding is required
  but the version has no `AsyncBreakCheck`, it now **hard-errors** (Q8) instead of shipping
  a misaligned delta.
- **Overflowed functions.** Must relocate the small-header pointer **and** the large
  header's internal fields (`resize_overflowed_function`, `functions.rs:277`). Legacy large
  header: body offset, size, info fields rewritten in the `slot..slot+16` copy
  (`functions.rs:219`); modern reads the packed pointer via `read_modern_large_pointer`
  (`functions.rs:287`). These magic offsets are v98-shaped; a version whose large header
  differs will be silently mis-patched.
- **Exception handlers are guarded, not relocated (Q3/Q4).** `patch_function_body` now
  **rejects any size-changing edit** on a function that declares an exception-handler table
  (`flags & FLAG_HAS_EXCEPTION_HANDLER`, `functions.rs:43`), because handler start/end/target
  offsets are body-relative and are not yet rewritten. Same-size edits are allowed. Full
  relocation is planned — see Pending impl plans.
- **Relative-jump safety depends on same-shift.** Body-internal `Addr8`/`Addr32` jumps hold
  deltas relative to their own instruction; front-insertion keeps caller and target moving
  together, so relative jumps survive — but this is a *property being relied on*, not a
  recomputation. A partial insertion (between a jump and its target) would break it.
- **Modern small-header field width (24 vs 25 bits).** Resolved — different fields, both
  correct. See Q2.

### Stub / inject work
- **Register/cache reservation must persist and be enough.** `log_frame_size` bumps frame
  by `max(4)+8` and reserves one read-cache slot (`inject.rs:19`, `:36`). Legacy edits the
  struct then relies on the resize path rewriting the full header; modern edits raw header
  bytes *before* the splice via `reserve_modern_log_regs` (`inject.rs:28`) at magic offsets
  (small: frame byte `+8`, cache byte `+9`, `inject.rs:61`; large: frame `+28`, cache `+32`,
  `inject.rs:56`). A stub needing more registers must widen this, and the magic offsets are
  version-fragile.
- **Hardcoded opcode operand shapes.** `build_log_entry` bakes in `TryGetById reg,reg,u8
  cache,u16 string` and `Call2 reg,reg,reg,reg`. Opcode *availability* is checked; operand
  *layout* is assumed constant across versions.
- **Exception-handler staleness (above)** applies doubly to inject, which front-inserts a
  prologue into an existing body — hence the Q3/Q4 guard covers `inject-stub` too (it funnels
  through `patch_function_body`).
- **`NopPad` insertion point.** Inserts `AsyncBreakCheck` before the last `Ret`, or at the
  end if there is none (`inject.rs:232`). "At the end" is only safe if the function already
  ended on a terminator; a function ending in a fallthrough would gain a reachable no-op
  (usually fine) but the assumption should be stated.
- **`AsyncBreakCheck` no longer silently skipped (Q8).** If the version lacks it and padding
  is required, both `patch_function_body` and `build_log_entry` hard-error. The no-pad-needed
  path (delta already `%4`, or a version that has `AsyncBreakCheck`) is unchanged.

### New `create` variants
- **Section order + header field gating.** `write_legacy_header` (`header_write.rs`) writes
  fields in a version-gated order (bigint if `has_bigint`, segment vs cjs, function_source if
  `v>=84`). Adding a populated section means emitting it in the body **and** matching its
  size into the correct gated header slot; a mis-gate shifts every later field.
- **Modern large-header field order** is hand-encoded in `build_minimal_modern`
  (`serialize.rs:227`) and must match the parser exactly, including the packed small→large
  pointer and the `ProhibitNone = 0b10` flag semantics (`serialize.rs:313`).
- **No overflow support (design limit above).** A create variant taking large tables must
  add overflow encoding first.
- **`create` now emits `warn_modern_write`** (`write_cmd.rs:403`) and still sets a zero
  `source_hash` — fine for minimal images, but a variant meant for real use should reconsider
  the latter.

---

## Stdout/stderr discipline

**The contract as it stands:** *machine-consumable data goes to stdout; human status,
progress, warnings, and notes go to stderr.* Write commands take a required `-o`/`--output`
file and print only status to stderr.

Per-command reality:

| Command | stdout | stderr |
|---|---|---|
| `add-string` | **bare new id** (`println!`, `write_cmd.rs:307`) | "Added string …" + dup note (if any) |
| `secrets` | JSON or text report (data) | — |
| `emit-hasm` (no `-o`) | HASM text (data) | — |
| `emit-hasm` (`-o`) | — | (nothing; writes file silently — see below) |
| `create` | — | "Created minimal HBC …" + modern note |
| `asm` / `patch-function` | — | "Assembled function …" |
| `patch-string` | — | "Patched string → …" |
| `retarget-string` | — | "Retargeted …" + cross-kind warning (if any) |
| `patch-operand` | — | operand-change status + `*ById` warning (if any) |
| `inject-stub` | — | "Injected stub …" |
| `frida-hooks` | — | "Wrote Frida hooks …" + export list |

**Status ownership is now entirely in the CLI layer (Q5 resolved).** Library patch
functions no longer `eprintln!`: `patch_string_operand` *returns* `(bytes, status, warning)`
and `run_patch_operand` prints them (`write_cmd.rs:200`, `:202`); the `retarget_string`
cross-kind warning (`write_cmd.rs:264`) and the `add_string` duplicate note
(`write_cmd.rs:301`) are recomputed and printed by their CLI handlers. Programmatic callers
of the library functions get no unsolicited stderr.

**Remaining inconsistency:**

- **`emit-hasm -o` prints no confirmation**, while every other `-o` writer does. The shared
  `write_output` helper *does* print "Wrote … (N lines, KiB)" — but `run_emit_hasm` uses a
  bare `std::fs::write` (`write_cmd.rs:143`) and bypasses it. ⚪ Decide before adding
  commands.

**Only `add-string` uses stdout for a result** — the deliberate scripting contract ("bare id
on stdout for script consumption"). New commands that yield a machine value (a new id, an
offset) should follow this; new commands that only transform a file should keep stdout empty.

**Exit codes** are uniform: handlers return `Result`, errors bubble to `main` which returns
`Box<dyn Error>` → non-zero exit with the error Debug-printed. Keep new commands on this
path (no `process::exit`, no `unwrap`/`panic` on user input).

---

## Test matrix gaps

Per command, cases that are **absent** from the current tests (derived from the `#[cfg(test)]`
modules). No CLI-level/integration test harness exists for the write path (only
`transforms/module_hoist/tests/`), so *all* coverage below is unit-level. The current pass
added CI tests to `functions.rs` (8), `inject.rs` (5), `operands.rs` (7), `strings.rs` (25)
that build a real image with `create_minimal` (rather than skipping on a missing fixture) —
several formerly-missing cases are now **covered** and marked so below.

- **`create`** (`create.rs`): has v96-parses, v98-parses. Still missing: the
  string-too-long / overflow **refusal** path; a boundary v97; unsupported/low versions; and
  **no test that a created file executes** (only that it parses).
- **`encode`** (`encode.rs`): v96 + v98 body round-trips. Still missing: every **error** path
  (arity mismatch, value-too-wide per operand type); `Double`/`Imm32`/`Addr8`-range
  operands; the type-tolerance no-op branch (I13/Q6).
- **`footer`** (`footer.rs`): fixture match + rehash-identity. Still missing: `rehash_footer`
  on a `< 20`-byte buffer; `verify_footer` on a truncated/short image.
- **`functions`** (`functions.rs`): **now covered** — grow, shrink, alignment-pad, modern-v98
  overflowed resize, `debug_info_offset` shift (fixture-gated), the handler-size-change
  rejection guard, and the Q8 missing-`AsyncBreakCheck` hard error. Still missing: a function
  **with a real exception-handler table** exercised through actual bytecode (the guard test
  sets the flag synthetically), and **modern-on-VM** verification.
- **`inject`** (`inject.rs`): v96 nop; v98 nop+log. **Now covered** — legacy `LogEntry` on
  v96, the no-`"print"`-string error, the overflowed-legacy refusal. Still missing: any check
  that the injected code actually runs.
- **`operands`** (`operands.rs`): absolute + function-relative round-trip; no-string-operand,
  nonexistent-id, insn-offset-OOB rejections; **now** the Q9 `*ById` warn / non-`*ById`
  no-warn cases. Still missing: **`--operand-index` selection** on a multi-string opcode
  (e.g. `CreateRegExp`); the **width-overflow rejection** (id larger than operand width);
  `UInt16S`/`UInt32S` operands; **modern v98**.
- **`strings`** (`strings.rs`): broad — same-length, grow-resize, packed→resize, ascii→utf16,
  retarget (6 cases), add_string (10 cases incl. modern v98), and **now `patch_string_replace`
  (`--old`)**: same-length, grow, and not-found error. Still missing: **shrink** resize;
  **resize of an identifier** (hash refresh under the resize path, as opposed to
  same-length/retarget); **patch/resize on modern v98** (only `add_string` is modern-tested);
  a UTF-16 in-place edit taking the forced-resize path; asserting the cross-kind retarget
  warning now that it lives in the CLI layer.
- **`hasm` emit/parse** (`emit.rs`, `parse.rs`): one v96 emit→parse→assemble round-trip.
  Still missing: **modern v98** round-trip; **all parser error paths** (unknown mnemonic,
  wrong operand count, string-not-in-table, unknown label, `Addr8`-out-of-range);
  comment/offset-prefix stripping; multi-function `parse_hasm`; and **exception-handler
  preservation** (`HasmFunction.exception_handlers` is never populated — see Q4).
- **`asm-check` / `run_roundtrip_check`** (`write_cmd.rs:410`): no test.
- **CLI handlers** (`write_cmd.rs`): no test of argument resolution (e.g. `--at` vs
  `--function`+`--insn-offset` precedence, `--string` vs `--string-id`, `--from`/`--to`
  value→id lookup) or of the stdout/stderr contract.

---

## Legacy/modern branching audit

"Modern" == `FunctionHeaderLayout::Modern12`, i.e. HBC **v97+** (12-byte function headers,
every real function overflowed). `MODERN_FUNCTION_HEADER_MIN_VERSION = 97`
(`header.rs:10`). `FLAG_OVERFLOWED = 0x20`, `FLAG_HAS_EXCEPTION_HANDLER = 0x08`
(`format.rs:22`, `:16`).

Full per-path fork status. "Tested on modern?" means a unit test actually parses/edits a
Modern12 image; ✅/🟢 track the tracker's meaning (🟢 = code + unit test, but modern output
is never VM-verified in CI).

| Path | Modern-aware? | Tested on modern? | Fork mechanism / notes |
|---|---|---|---|
| `add-string` | **Yes** | **Yes** (v98) | full modern branch (`strings.rs:544`); modern debug-off=108 |
| `patch-string` same-length | Layout-agnostic | No | `locate_string_bytes` uses sections (`strings.rs:16`); should work |
| `patch-string` resize | **Yes** | **No** | modern debug-off=108, hsize=12, overflow relocate (`strings.rs:316`) — untested on modern |
| `patch-string --old` (replace) | **Yes** (via resize) | **No** | by-value lookup then resize/same-length (`strings.rs:860`) |
| `retarget-string` | Layout-agnostic | No | touches small table + id hash only (`strings.rs:215`); refuses overflow |
| `patch-operand` | Layout-agnostic | No | decodes at offset (`operands.rs:89`); should work modern |
| `asm` / `patch-function` | **Yes** | **Yes** (v98) | `resize_modern_small` (`functions.rs:248`) + `resize_overflowed_function` (`:277`); now directly tested (`modern_v98_overflowed_resize_reparses`) |
| `inject-stub` | **Yes** | **Yes** (v98 nop+log) | `reserve_modern_log_regs` (`inject.rs:28`); legacy branch now tested on v96 |
| `create` | **Yes** | **Yes** (v98) | `create_minimal` dispatches to `build_minimal_modern` at v≥97 (`create.rs:74`) |
| `emit-hasm` | read-only | v98 fixture exists | disassemble only |
| `secrets` / `frida-hooks` | read-only | — | analysis; no layout fork on the write side |
| `asm-check` (`run_roundtrip_check`) | inherits `asm`/`emit-hasm` | No | `write_cmd.rs:410`; no test |

**`warn_modern_write` coverage:** now emitted by **every** write command that opens a file —
`asm`, `patch-operand`, `retarget-string`, `add-string`, `patch-string`, `inject-stub`, **and
`create`** (`write_cmd.rs:403`, added this pass). `emit-hasm` (read-only) does not emit it.

**Modern gaps / fragilities:**
- **Hardcoded v98 large-header field offsets.** Modern resize relies on frame `+28`, cache
  `+32` (`inject.rs:56`), size/body in `resize_overflowed_function`, and the packed pointer in
  `read_modern_large_pointer`. There is no abstraction over the large-header layout, so a
  **v99+** with a different FunctionInfo shape would be mis-encoded with no error. The whole
  write path assumes "modern" is exactly the v97/v98 12-byte layout. (Factoring a shared
  `modern_large_header_len()` is called out as a Q3-impl unknown.)
- **24 vs 25 bit body-offset field — resolved (Q2), not a bug.** The 24-bit mask
  (`read_modern_large_pointer`, `header_write.rs`) reads the **overflowed** packed large-header
  pointer (offset portion 24 bits); the 25-bit mask (`shift_modern_small_header_offset`,
  `header_write.rs:113`; `resize_modern_small`, `functions.rs:246`) shifts the **non-overflowed**
  body-offset field (25 bits). Different fields; both correct.
- **No CI VM verification.** Legacy same-length string patch and legacy resize are the only
  fully-verified-on-VM paths (a real `hermes` binary exists for ≤ v96). Everything modern is
  "verified on a real v98 engine" per the docs and the author's commit message, but **not** by
  any test that runs in CI — hence every modern row is 🟢, never ✅.
- **Handlers on modern.** In v97+ essentially every function is overflowed, so the Q3/Q4 guard
  correctly keys on `FLAG_HAS_EXCEPTION_HANDLER` (not `info_offset != 0`, which would reject
  every modern function and break the documented modern `inject-stub` path). See Q4.

---

## Git history findings

**What the committed history looks like.** On `upstream/main` (SymbioticSec) the *entire*
write path is a single commit — `50cdbf8` "Add HBC write path … (v0.2.0)", 14 files, ~3055
lines — and it has **never been revised upstream**. Every bug-fix and test commit is on the
local fork's `main`, and every one of them concerns the three commands added *after* the
monolith: `add-string`, `retarget-string`, `patch-operand`. **No reverts exist anywhere.**

**Churn is concentrated in exactly two files (as committed).** Post-authoring touch counts on
`main`: `strings.rs` = 6, `operands.rs` = 3; **`functions.rs`, `inject.rs`, `create.rs`,
`serialize.rs`, `header_write.rs`, `encode.rs`, `hasm/*`, `footer.rs` = 1 each** (the original
commit, never touched since). The churned files are the newest and the only ones that received
external (Copilot) review.

> **Update (current uncommitted pass).** The working tree now revises `functions.rs`,
> `inject.rs`, `operands.rs`, `strings.rs`, `header_write.rs`, `serialize.rs` and
> `write_cmd.rs`: it implements the Q3/Q4 guard, Q5, Q6, Q8 and Q9, corrects the Q2 comment,
> reconciles the create-modern docs (Q1), and adds the **first independent tests** to
> `functions.rs` and `inject.rs`. So F5's "never been through the impl→fix→test loop" no
> longer holds for those two files — but `create.rs`, `serialize.rs` and `header_write.rs`
> are still untested beyond what `create`/resize exercise indirectly.

### Findings

- **F1 — `7fa1bfc` "Fix Copilot review findings from PR #3" (four bugs in one commit).**
  Areas: `patch-operand` (`operands.rs`), `retarget-string` (`strings.rs`), CLI
  (`write_cmd.rs`).
  1. `patch_string_operand` wrote a string id without checking `new_string_id <
     string_count` → could produce invalid bytecode (now `operands.rs:103`).
  2. `patch_string_operand` `FunctionRelative` didn't bound-check `insn_offset <
     bytecode_size` → could patch outside the function body (now `operands.rs:122`).
  3. `retarget_string`'s overflow test had a dead branch `off == 0x800000` (unreachable
     after the 23-bit mask); the real sentinel is `len == 0xff` (now `strings.rs:258`).
  4. CLI `run_retarget_string` indexed `file.strings[fid]`/`[tid]` **before**
     `retarget_string` validated the ids → panic on a bad `--from-id`/`--to-id` (now
     reads after validation via `.get()`, `write_cmd.rs:245`).
  **Implies:** the recurring real-world bug class on this write path is **missing input
  validation before a raw write** (id-in-range, offset-in-bounds) and **trusting a masked
  field as a sentinel** (I8). Corroborates I8/I11. Every new op must validate
  `id < string_count` and `offset < body_size` up front, must never index `file.strings[x]`
  before the library has validated `x`, and must key overflow on `len == 0xff` only.

- **F2 — `fc2c4c2` "Add regression tests for Copilot-found bugs".** Added the three tests
  that would have caught F1's #1–#3. The reason those tests were needed — no up-front
  validation — is the pattern to watch.

- **F3 — `316741f` "Fix add-string stdout: emit bare numeric id".** `add-string` originally
  printed `"added string id {id}"` to **stdout**, defeating programmatic consumption; fixed
  to emit the bare id on stdout, human text on stderr (`write_cmd.rs:307`). **Implies:** the
  stdout=data / stderr=human rule was itself a *bug fix*, not a designed-in convention — so a
  new command will not inherit it by default. New commands must consciously put only a machine
  value on stdout.

- **F4 — `1211883` → `4bfd1d5` "Add missing tests from implementation plan audit"
  (14 min apart).** The first `add-string` implementation shipped without three tests the plan
  specified: modern-v98 reparse, downstream-offset integrity, and the overflow-entry path.
  **Implies:** even a *planned* command under-delivered coverage on precisely the hard edges —
  modern relocation, offset shifting, overflow.

- **F5 — the "impl → fix-within-minutes → add-regression-tests" arc.** `strings.rs`: impl
  `1211883` → tests `4bfd1d5` → retarget `3b9b673` → fix `7fa1bfc` → tests `fc2c4c2`.
  `operands.rs`: impl `166c19b` → fix `7fa1bfc` → tests `fc2c4c2`. **Implies:** each new
  command shipped with validation holes that only review caught. The never-reviewed monolith
  files had not been through this loop — the current pass has now put `functions.rs` and
  `inject.rs` through the *test* half (guards + CI tests), but `create.rs`, `serialize.rs`,
  and `header_write.rs` remain **unproven**, not *stable*. The highest-severity items in
  High-risk areas (exception-handler relocation, modern large-header magic offsets,
  create-variant field gating) still live in that never-independently-tested code.

- **F6 — `50cdbf8`'s own commit message resolves Q1.** It states, verbatim, "Create a
  minimal file from scratch, legacy layout for v96 and lower and **modern layout for v97 and
  newer**," and claims the write path is "verified on real Hermes VMs for HBC 74, 76, 83, 84,
  89, 96 and 98." **Implies:** modern `create` is the author's *intended* behavior — the code
  is authoritative. The prose has now been reconciled (Q1). Caveat: "verified on real VMs" was
  a one-time manual check; **no CI test runs a VM**, so it is not a standing guarantee,
  especially for modern output.

- **Adjacent corroboration (read path, not write).** `bf32a5d` "Fix xref on Modern (HBC98)
  layout…", plus `203671b`/`5ba55ca`/`102cc61` (v96 debug-capacity overflow, parser integer
  overflow/underflow panics, malformed-bytecode crashes) show that **Modern-layout handling
  and offset arithmetic are recurring bug loci across the whole codebase.** The write path's
  modern branch and its offset-shifting math are unlikely to be exceptions.

---

## Open questions

Decisions a future impl agent must not guess at.

- **Q1 — `create` and modern (v97+): RESOLVED, docs reconciled.** Intent settled by
  `50cdbf8`'s commit message (finding F6) and the code + `create_minimal_v98_parses`. The
  docs are now brought into line in this pass: `USAGE.md` says create emits "legacy layout
  for v96 and lower and modern layout for v97 and newer"; the `warn_modern_write` note text
  says the same (`write_cmd.rs:25`) and is now emitted by `create` too (`write_cmd.rs:403`);
  the stale `build_minimal_legacy` guard message now reads "v97 and newer use modern layout
  (build_minimal_modern)" (`serialize.rs:94`). Nothing left to decide.
- **Q2 — Modern small-header body-offset field: 24 or 25 bits? RESOLVED — no-op.** The
  24-bit and 25-bit masks cover *different* fields, and both are correct:
  `read_modern_large_pointer` reads the **overflowed** packed large-header pointer, whose
  offset portion is 24 bits (`function_name << 24 | offset & 0x00ff_ffff`, per parser);
  `shift_modern_small_header_offset` (`header_write.rs:113`) and `resize_modern_small`
  (`functions.rs:246`) shift the **non-overflowed** body-offset field, which is 25 bits (per
  parser Modern12 bitfield map `offset : (0, 25)`). No non-overflowed read uses 24 bits, so
  there is no single-field inconsistency to align. The `header_write.rs` comment was corrected
  this pass to say so explicitly.
- **Q3 — Exception-handler tables on size-changing edits: interim guard shipped; full
  relocation planned.** Contract chosen: handler `start`/`end`/`target` are **body-relative**
  (0-based, `end` exclusive; confirmed — `decode_function_instructions` emits 0-based offsets
  and the CFG compares handler offsets directly against them, `jump_analysis.rs:134`). They are
  safe under a pure *string-region* growth (the whole tail shifts uniformly, offsets stay
  relative) but **not** under a body-internal size change (`patch-function`/`asm`/`inject`).
  Interim resolution: `patch_function_body` (`functions.rs:43`) **rejects any size-changing
  edit** on a function that declares an exception-handler table, rather than ship stale
  offsets. Full relocation is planned — see Pending impl plans. Remove the guard once it lands.
- **Q4 — `HasmFunction.exception_handlers`: unimplemented feature, not a drop. Guard shipped;
  build-vs-guard for whole-body ops is a call for Keith.** The field is *never populated*:
  `parse_hasm` always sets it to `Vec::new()`, the HASM dialect has no handler syntax, and
  `emit_hasm_function` emits no handler lines. `parse_hasm_with_context` returns only
  `Vec<Instruction>`, so `asm`/`patch-function` can neither carry nor relocate a function's
  handler table. So carrying handlers through HASM is an unbuilt feature, and the field is
  vestigial. **Guard IMPLEMENTED** (`functions.rs:43`): any size-changing edit (`delta != 0`
  — covers `asm`/`patch-function`/`inject-stub`, which all funnel through `patch_function_body`)
  on a function that declares handlers is rejected with a clear error naming the function
  index. **Detection uses `flags & FLAG_HAS_EXCEPTION_HANDLER` (bit 3), the parser's own gate
  — NOT `info_offset != 0`** (which over-rejects debug-only legacy functions and, fatally,
  every overflowed modern function). Same-size edits still allowed. Covered by
  `size_change_on_function_with_handlers_is_rejected`.
- **Q5 — Should library patch functions write to stderr at all? RESOLVED — no.** All three
  library `eprintln!`s were removed. `patch_string_operand` now *returns* `(bytes, status,
  warning)`, which `run_patch_operand` prints (`operands.rs`, `write_cmd.rs:200`/`:202`). The
  `retarget_string` cross-kind warning and the `add_string` duplicate note are recomputed and
  printed by their CLI handlers (`run_retarget_string`, `run_add_string`). CLI output is
  byte-identical to before; programmatic callers get no unsolicited stderr. Status ownership
  now lives entirely in the CLI layer.
- **Q6 — `encode_instruction` operand-type tolerance: RESOLVED — no-op branch; safe.** The
  `if op.ty != *expected_ty { … }` block at `encode.rs:24` is **empty**. Encoding is always
  driven by the definition's `expected_ty`, so the decoded instruction's own `op.ty` tag is
  intentionally ignored. The only "tolerance" with effect is in `write_operand`, which accepts
  several `OperandValue` variants for a given width **but range-checks every narrowing** and
  returns a hard `Error` on overflow or an incompatible variant. It does **not** mask
  width/overflow bugs. It does not validate operand *kind/role* — that is Q9, not an encoder
  bug.
- **Q7 — Identifier placement: RESOLVED — no leading/contiguous requirement.** Empirical proof
  from a real hermesc-compiled bundle (`com.equinoxfitness.equinox_11.39.0`, HBC v96): its
  `string-kinds` table has **four interleaved runs** — `Identifier×255, String×15013,
  Identifier×50267, String×33382` — i.e. hermesc emits `String → Identifier` transitions and
  multiple non-contiguous identifier regions, and the VM loads them. The identifier hash table
  is indexed by *running identifier count* (`identifier_index`, `strings.rs:174`; I9/I12),
  which is arrangement-independent. `add_string`'s trailing Identifier run is exactly the shape
  hermesc already ships. Residual: inference from production layout, not a direct VM run of
  `add_string`'s specific output — but the resulting layout is structurally identical to
  bundles the VM already executes.
- **Q8 — `AsyncBreakCheck` as universal no-op padding: RESOLVED — hard error when needed and
  absent.** `AsyncBreakCheck` is **absent in `Bytecode40`–`Bytecode60` and present in
  `Bytecode61`–`Bytecode99`** (introduced at v61). Every version the write path realistically
  targets (≥76; Equinox is v96) has it, so the padding path is normally taken. **IMPLEMENTED:**
  the silent skip in `patch_function_body` (`functions.rs:54`) and `build_log_entry`
  (`inject.rs:90`) is now a hard `Error::Write` **only on the path where padding is actually
  required** (size delta not `%4`, or injected prologue not `%4`, and no `AsyncBreakCheck`
  available). The no-pad-needed path is unchanged. Covered by
  `missing_asyncbreakcheck_pad_is_hard_error` (v56).
- **Q9 — `patch-operand` semantic (kind) validation: RESOLVED — warn only.**
  `patch_string_operand` (`operands.rs:234`) returns an optional warning (printed by
  `run_patch_operand`; the library stays silent, per Q5) when a `*ById`-family opcode's
  property-name operand is repointed at a **non-identifier** string. **Warning, never error** —
  the edit still applies. Detection: `def.name.contains("ById") && !new_is_identifier`. This
  substring test is exact and version-independent: across **every bundled opcode table
  (v40–99)**, every opcode whose name contains `ById` has exactly one string operand — the
  property name — and nothing else matches. Full family (scanned from
  `resources/bytecode/Bytecode*.json`): `GetById`/`GetByIdShort`/`GetByIdLong`/
  `GetByIdWithReceiverLong` (v98–99); `TryGetById`/`TryGetByIdLong`; `PutById`/`PutByIdLong`
  (v40–96) and the `PutByIdLoose*`/`PutByIdStrict*` split (v97–99); `TryPutById*` (same split
  at v97); `PutNewOwnById*`/`PutNewOwnNEById*` (v45–97) and `PutOwnById*` (v40–44);
  `DefineOwnById`/`DefineOwnByIdLong` (v98–99); `DelById*` (v40–96) and `DelByIdLoose*`/
  `DelByIdStrict*` (v97). Covered by `getbyid_to_non_identifier_warns` and
  `loadconststring_to_non_identifier_does_not_warn`. Rationale for warn-not-error: a
  non-identifier property name silently breaks the runtime identifier-hash lookup (a real
  footgun), but a hard error would risk blocking an unusual-but-valid edit, and there is no
  per-operand role metadata to make enforcement airtight.

---

## Pending impl plans

Fully-scoped plans for work that is decided but not yet built. Written so an impl agent can
execute without re-deriving the format. File:line references are to the tree state noted;
re-check them.

### Q3 — Full exception-handler relocation

**Goal.** Relocate a function's exception-handler table across a size-changing edit so
`patch-function` / `asm` / `inject-stub` no longer have to be rejected (removing the Q4
guard). Ships in two phases because the two op families differ fundamentally in what
information is available.

**Handler-table layout (derived from the parser — do not re-guess).**
- Presence is gated by `flags & FLAG_HAS_EXCEPTION_HANDLER` (bit 3, `format.rs:16`; the
  parser's own gate, `parsing.rs:362`). Detection must key on this bit, NOT `info_offset
  != 0` (that over-rejects debug-only legacy functions and every overflowed modern one — Q4).
- **Location** is `aligned = (info_offset + 3) & !3` (`parsing.rs:371`).
  - *Legacy:* `info_offset` is a real field — bits 64..88 of a non-overflowed small header
    (`function.rs:53`), or `large_header + 16` for an overflowed one (`function.rs:170`).
    It points into the FunctionInfo region, which sits after all code.
  - *Modern:* `info_offset` is **not stored** — `parse_large_header_modern` computes it as
    the 4-byte-aligned position immediately after the large header's fields
    (`function.rs:207–212`). The large header is 8×u32 + 5×u8 = 37 bytes, so the table sits
    at `aligned(large_ptr + 37)`. **Derive this size from `parse_large_header_modern` at impl
    time rather than hardcoding 37/40** — the existing large-header magic offsets are already
    flagged as v98-shaped and version-fragile.
- **Table format** (`parsing.rs:378–406`): `count: u32`, then `count` entries of
  `{ start: u32, end: u32, target: u32 }`, 12 bytes each. Total table size = `4 + count*12`.
  Table size does not change under relocation (I5 stays satisfied).
- **`start`/`end`/`target` are body-relative** (0-based within the function body; `end`
  exclusive). Confirmed: `decode_function_instructions` emits 0-based offsets
  (`instructions.rs:33,54,59`) and the CFG compares handler offsets directly against them
  (`jump_analysis.rs:134–141`, `ir_builder.rs:107`).

**How offsets adjust for a size delta.** The general rule for a **single-point insertion** of
`L` bytes at body offset `P`: every body-relative offset `>= P` shifts by `+L`; offsets `< P`
are unchanged. Apply per handler field independently to `start`/`end`/`target`.
- `inject-stub LogEntry` front-inserts a prologue at `P = 0`, so **all** entries shift by
  `+L` (L = prologue length in bytes — already 4-aligned by the Q8 path).
- `inject-stub NopPad` inserts `L = 1..4` bytes before the last `Ret` (`inject.rs:232`), so
  `P` = that instruction's body offset; only entries at or past `P` shift. (Straddling case
  is an unknown — below.)
- `patch-function` / `asm` replace the **whole body** with an arbitrary new instruction
  stream. There is no single `(P, L)`; the old→new offset mapping is not recoverable from
  bytes. Relocation here is impossible without the caller supplying the new try/catch regions
  — which requires HASM handler syntax (Q4's unbuilt feature). Hence the phasing.

Note the table's **location** already relocates today: `patch_function_bytes` copies the
FunctionInfo region verbatim in the tail splice, and the info pointer is shifted by the
existing overflowed/legacy relocation (`resize_overflowed_function`,
`shift_legacy_small_header_offsets`, `functions.rs`). Q3's *new* work is rewriting the entry
**values**.

**Phase 1 — inject-stub (single-point insertion). Feasible now.**
- Files: `crates/hbc-decomp/src/write/patch/functions.rs` (new helper
  `relocate_handler_entries(rebuilt, info_loc, insert_at, shift)` operating on the spliced
  `rebuilt` buffer before `finalize_raw_image`); `crates/hbc-decomp/src/write/patch/inject.rs`
  (`inject_stub`/`build_log_entry` compute `(P, L)` and request the relocation);
  `patch_function_body` gains an internal way to carry `(P, L)` from an insertion-style caller
  (e.g. a private `patch_function_body_inserted(P, L, …)` or an options field) so the
  arbitrary-resize callers do NOT trigger it.
- After the splice, for the patched function: locate its handler table via the shifted info
  pointer (reuse the read paths in `header_write.rs`/`functions.rs`), read `count`, and for
  each 12-byte entry conditionally add `shift` to `start`/`end`/`target`. Update the in-memory
  `file.exception_handlers` to match (I1).
- Remove the Q4 guard *only for the insertion path*; arbitrary resize still rejected.

**Phase 2 — patch-function / asm (whole-body). Depends on Q4 HASM handler syntax.**
- Prerequisite: give the HASM dialect a way to express handler regions (e.g. `.try_start` /
  `.try_end` / `.catch` directives or a `.handler start,end,target` line), have
  `parse_hasm_with_context` populate `HasmFunction.exception_handlers` (`hasm/parse.rs`,
  `hasm/mod.rs`), and `emit_hasm_function` emit them (`hasm/emit.rs`) so round-trips are
  lossless. `assemble_function_hasm` then rebuilds the handler table from the parsed regions
  instead of copying the stale one.
- Only after this can the Q4 guard be removed for `patch-function`/`asm`.

**Invariants that apply.** I1 (sync `file.exception_handlers` after the byte edit), I2 (do the
relocation inside the single resize op, on `rebuilt`, before returning — never chain on a
stale `file`), I3/I4 (route the result through `finalize_raw_image`; keep the double-hash), I5
(table size is unchanged, so 4-alignment holds), I6 (entry *location* already shifts with the
tail; this adds the entry *value* shift).

**Required tests before "done".**
- inject LogEntry on a function **with** handlers (legacy non-overflowed, legacy overflowed,
  modern overflowed): every entry's `start`/`end`/`target` shifted by `+L`; reparse;
  `verify_footer`; `file.exception_handlers` matches the reparsed table.
- inject NopPad with a mid-body `P`: only entries `>= P` shift.
- Multi-handler / nested try-catch (count > 1): every entry relocated, 12-byte stride correct.
- `end == body_len` (exclusive) edge case: shifted value stays consistent, reparse clean.
- Phase 2: HASM emit→parse→assemble round-trip **preserves** the handler table (the gap called
  out in Test matrix gaps → hasm).
- External VM check for the modern cases (in-Rust cannot verify modern output; see design
  limits).
- Update/remove `size_change_on_function_with_handlers_is_rejected` (functions.rs tests) as
  the guard is lifted per phase.

**Unknowns to resolve before impl starts.**
1. **NopPad straddling a try region** (`start < P < end`): does only `end` shift (widening the
   try to cover the inserted no-op), or is inserting inside a live try region disallowed?
   Benign for a no-op but the semantics must be chosen. LogEntry (`P = 0`) never hits this.
2. **Scope of Phase 2 now vs later.** Confirm whether `patch-function`/`asm` handler support
   (and the HASM directive syntax) is in scope, or whether those keep the Q4 guard until a
   separate effort. Recommend: ship Phase 1, keep the guard for arbitrary resize.
3. **Exact modern large-header size** feeding `aligned(large_ptr + size)` — derive from
   `parse_large_header_modern`, and decide whether to factor a shared
   `modern_large_header_len()` so this and the existing `reserve_modern_log_regs` magic offsets
   stop drifting.
4. **FunctionInfo beyond the handler table.** Debug-info offsets may follow the handler table
   in the same region and are also body-relative in part; confirm Phase 1 does not need to
   touch them (debug info is an opaque buffer per design limits) or scope it explicitly.
