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

> **Revision note — a real v99 engine is now on this machine.** A compiled
> facebook/hermes (`static_h`, `v0.12.0-5581-ge9edc8b52`, `BYTECODE_VERSION = 99`) sits at
> `C:\src\hermes-v99` with binaries in `build\bin\Release`. Two things follow, and this pass
> rewrites the doc around them:
> 1. **Modern output is now verifiable, on Windows, from Rust, with no FFI** — `hvm.exe`
>    is a standalone VM driver that takes a `.hbc` path. The "modern cannot be verified"
>    design limit is repealed; see Reference VM and toolchain.
> 2. **R8 has fired.** The modern large function header is **36 bytes at v99, 37 at v98**,
>    and the write path is hard-pinned to 37. Every claim about it below is now backed by
>    a measurement against the real engine rather than by reading. See The v99 delta.
>
> Claims marked **[measured]** were reproduced against a real `hvm.exe` on `hermesc.exe`-built
> fixtures; claims marked **[source]** are read off
> `include\hermes\BCGen\HBC\BytecodeFileFormat.h` at that commit.
>
> ⚠️ **Superseded on the ref, not on the substance.** v99 now means the React Native
> release branch (`origin/260318099.0.0-stable`, `b7b58dd3c`), built at
> `C:\src\hermes-v99`; the `static_h` clone was renamed to `C:\src\hermes-src` and its
> build removed. Every **[source]** claim below survives that move unchanged, because
> `BytecodeFileFormat.h` is byte-identical between the two commits — which is exactly
> why the header layout could not detect the difference and the opcode table had to.
> See v99 means the release branch.
>
> **Follow-up pass — R8/R9/R11/R15/R20 are now fixed, and a VM harness exists.**
> `crates/hbc-decomp/src/modern_layout.rs` replaces the hardcoded v98 shape with a
> version-keyed `ModernLayout`; `crates/hbc-decomp/tests/vm_verify.rs` runs each write op on a
> real engine and asserts stdout + exit code across **v96, v98 and v99**; and
> `scripts/build_hermes_vm.ps1` builds the three VMs (including a **v96** one, which is what
> the Equinox bundles need). The historical narrative below is kept as written — the
> descriptions of *how* these failed are the durable part — with fixed items marked ⬜ in the
> register.
>
> **Harness pass — four more harnesses, and each of the first two found a bug.** The theme
> of this doc has been that the suite asserted the wrong thing; this pass attacks that
> directly. See Test harnesses for what exists and how to run it.
>
> | Harness | Found |
> |---|---|
> | `commit_image` I1 check (`serialize.rs`) | **Every write op had a partly-stale model.** Fixed structurally by re-deriving instead of hand-syncing, which also retired **R1**, the top risk. |
> | `tests/upstream_pin.rs` | **`===` and `==` decoded as numeric comparisons on v99.** Eight phantom opcodes shifted twelve later ones. See The v99 opcode drift. |
> | `tests/corpus.rs` + hbcdump differential | Nothing wrong: 62,909/62,909 bodies re-encode byte-identically, 62,637 match hbcdump. First coverage of the **1,449 overflowed string entries** no fixture can produce. |
> | `hbc-decomp-cli/tests/stdout_contract.rs` | **The debug CLI binary had always overflowed its stack**, which is very likely why R17 was never closed. |
>
> The pattern worth carrying: *three of the four found something, and none of the three was
> reachable by the kind of test that already existed.* Each new harness asserts against an
> independent source of truth — the engine, the upstream headers, a second disassembler, the
> process boundary — rather than against our own model.

---

## Status at a glance (work tracker)

A project-tracker view of the write path. Status labels: **✅ done** (implemented +
tested in CI), **🟢 done, VM-unverified** (implemented + unit-tested, but not checked by any
VM in CI), **🟡 interim** (a guard/partial in place, full feature planned), **🔵 planned**
(decided, not built), **⚪ open** (a policy call left for Keith), **🔴 broken** (measured
wrong against a real engine).

The **VM** column is what a real `hvm` says about that command's output. It is no longer a
manual result: `tests/vm_verify.rs` asserts these, on v96/v98/v99 fixtures, whenever
`HERMES_VM_V<N>` is set (R21). `n/a` means the command produces no `.hbc` to run.

### Commands (all shipped)

| Command | Modern-aware | CI test | VM | Status | Notes |
|---|---|---|---|---|---|
| `add-string` | Yes | Yes (v98) | ✅ ran | ✅ | full modern branch (`strings.rs:544`) |
| `patch-string` same-length | Layout-agnostic | Yes (v96) | ✅ ran | ✅ | in-place; `locate_string_bytes` via sections |
| `patch-string` resize | Yes | Yes (v96 grow) | ✅ grow **+ shrink** | ✅ | modern debug-off=108 confirmed **[source]**; shrink was a listed gap, now measured |
| `patch-string` ASCII→UTF-16 | Yes | Yes (v96) | ✅ ran (`élan`) | ✅ | I7 forced-resize path, measured on modern |
| `patch-string --old` (replace) | Yes | Yes (v96) | ✅ ran | ✅ | by-value lookup tested (`strings.rs:860`) |
| `retarget-string` | Layout-agnostic | Yes (v96) | ✅ ran | ✅ | small table + id hash only; refuses overflow |
| `patch-operand` | Layout-agnostic | Yes (v96) | ✅ ran | ✅ | + Q9 property-name warning |
| `asm` / `patch-function` | Yes | Yes (v96 + v98) | ✅ identity round-trip | ✅ | the Q3/Q4 gate in front of it is now correct on every supported layout |
| `asm-check` (`run_roundtrip_check`) | Yes | No | ✅ `OK` on v99 | ⚪ | still no test (`write_cmd.rs:410`) |
| `inject-stub` | Yes | Yes (v96 + v98 + v99) | ✅ ran | ✅ | was 🔴 on v99: shifted a body without relocating handlers, because the guard in front of it read the wrong byte. Fixed via `ModernLayout`; both guard directions now tested on real fixtures |
| `create` | Yes | Yes (v96 + v98 + v99) | ✅ ran | ✅ | was 🔴 on v99: emitted a 37-byte large header, so the engine read `flags` from the wrong byte and refused to call the global. `create_minimal_runs_on_vm` now asserts the output executes |
| `emit-hasm` | read-only | Yes (v96, v98 fixture) | n/a | ✅ | one emit→parse→assemble round-trip |
| `secrets` / `frida-hooks` | n/a (read) | — | n/a | ✅ | data to stdout / hooks file |

The two rows that were 🔴 were **one defect**, not two: the modern large header being 37 bytes
in our code and 36 in v99. Both are fixed by the same descriptor, and both now have a test
that fails if it regresses.

Note what this table still does not show. Every "CI test" here is against a three-function
fixture. The inputs that actually break things — overflowed string entries, UTF-16 storage,
real exception tables, the long tail of opcodes — appear only in production bundles and are
covered separately by `tests/corpus.rs`. A ✅ here means "the op works on a small image", not
"the op works".

### Open questions / decisions

| # | Topic | Status | Where |
|---|---|---|---|
| Q1 | `create` modern (v97+) + doc reconciliation | ✅ resolved, docs reconciled | Open questions |
| Q2 | Modern small-header offset 24 vs 25 bits | ✅ resolved — different fields, both correct; **re-confirmed verbatim in v99 source** | Open questions |
| Q3 | Exception-handler relocation on resize | 🟡 guard shipped and now correct on every supported layout; 🔵 full relocation planned (unblocked) | Pending impl plans |
| Q4 | `HasmFunction.exception_handlers` | 🟡 guard done and VM-tested both directions; ⚪ build-vs-guard for Keith | Open questions |
| Q5 | Library functions writing to stderr | ✅ resolved — no; status returned to CLI | Open questions |
| Q6 | `encode_instruction` operand-type tolerance | ✅ resolved — no-op branch, safe | Open questions |
| Q7 | Identifier placement (leading/contiguous?) | ✅ resolved — no requirement | Open questions |
| Q8 | `AsyncBreakCheck` as universal pad | ✅ resolved — hard error when needed+absent; **present at v99** | Open questions |
| Q9 | `patch-operand` `*ById` kind validation | ✅ resolved — warn only | Open questions |

Q1–Q9 are stable, mostly-resolved *design decisions* (why the code is the way it is); they
don't churn as work lands. The temporal "what to do next" work is **not** kept as a parallel
numbered list — it lives as attributes on durable risk rows (see below).

### Open work index

Where the not-yet-done work is tracked — pointers only, so nothing is duplicated into a second
list that can drift. Hardening priority is *derived, not maintained*: sort the **risk register**
(High-risk areas → Risk register) by `Residual` — 🟥 first, then 🟧. As of this pass the 🟥 column
holds one row, **R21**, and it is about the harness gate rather than about the write path.

- **Provision the external oracles where CI runs, or fail when they are missing** → R21. Every
  harness that found a defect this pass is env-gated, so an unconfigured run is green while
  asserting almost nothing. This is the single highest-value remaining item and it is
  infrastructure, not code.
- **A way to *fix* a drifted opcode table** → R19. `tests/upstream_pin.rs` detects drift and
  names it precisely; regeneration is still manual, because the three `Bytecode*.json` files
  are heterogeneous artifacts and a one-size regenerator destroys real data (tried, reverted).
  Normalising the three files is the prerequisite.
- **Exception-handler relocation** (the one large planned feature) → Pending impl plans (Q3),
  in two phases. Unblocked: the table can now be located correctly on every supported layout.
- **Encoding an overflow string entry** → R2. Detection is verified against 1,449 real entries;
  *writing* one is still unimplemented, and `create` refuses it.
- **CLI argument-resolution coverage** → R17. The stdout/stderr contract is now asserted; `--at`
  vs `--function`+`--insn-offset` precedence and the value→id lookups are not.
- **Remaining unit-test gaps** (identifier-resize hash refresh, HASM error paths + handler
  round-trip) → Test matrix gaps.
- **Hardening actions** (each lowers one risk's residual) → the register's `Hardening` column.

---

## Write-path invariants

Contracts every mutation must satisfy. Numbered so future work can cite them.

**I1 — `raw_bytes` is the source of truth; the structured model is *derived* from it.**
Every patch op clones `file.raw_bytes`, edits bytes, and finishes by calling `commit_image`
(`serialize.rs`), which finalizes and then **re-parses the result to rebuild the whole
model**. `serialize_file` *errors* if `raw_bytes` is `None`.

> **This used to say "hand-maintained shadow", and that was the problem.** Each op updated
> some subset of `file.strings` / `file.header.*` / `file.function_headers` by hand, and a
> debug assertion added at the commit point found *every* op getting that subset wrong on its
> first run (F8). Deriving the model removes the class rather than the instances: an op added
> later inherits correctness instead of having to remember it.

For a new op the practical rule is simply **end with `commit_image(file, buf)`**. Do not
assign `file.raw_bytes` directly, and do not bother hand-updating model fields — anything you
write there is overwritten by the re-parse. (The hand-sync code still present inside some ops
is now redundant rather than load-bearing; leave it, but do not add more.)

**I2 — after a size-changing op the model, including `file.sections`, is already fresh.**
The resize/append paths (`add_string`, `patch_string_resize`, `patch_function_bytes`) move
section boundaries, and `section_offset()` reads `file.sections`. Historically those were not
rewritten, so a *second* op on the same in-memory `file` computed offsets against the **old**
layout and silently corrupted the image — the highest-residual risk in the register (R1),
held back only by a written contract and by tests that dutifully re-parsed.

`commit_image`'s re-parse refreshes `sections` along with everything else, so **chaining ops
on one `BytecodeFile` is now safe and needs no explicit re-parse.** The old contract
("`parse_auto` the returned bytes before running another op") is satisfied by construction.
Pinned by `chained_size_changing_ops_need_no_reparse`, which runs three chained size-changing
ops with no re-parse and requires the output to execute on a real VM; it fails if the refresh
is removed.

The cost is one parse per write op — ~40ms on the 5MB Equinox bundle, unmeasurable on
fixtures. That is the price the old contract already asked callers to pay, now paid in one
place instead of at every call site.

**I3 — The footer is always the last 20 bytes = `sha1(image[:-20])`.** Enforced by
`footer.rs`. Every write path must end by routing through `commit_image`, which calls
`finalize_raw_image` (`serialize.rs`) for you — or, outside the patch ops, through
`serialize_file`/`append_footer`. A raw byte edit that skips finalization ships a file that
fails `verify_footer` and is rejected by the loader.

**I4 — `file_length` at bytes `[32..36]` counts the footer, and is inside the hashed
region.** This is why `finalize_raw_image` and `serialize_file` hash **twice**: rehash,
write `len = out.len()` (footer included) into `[32..36]`, rehash again. Any new finalizer
must replicate the double-hash or the length field's own bytes corrupt the hash.

**I5 — 4-byte alignment of everything after the code.** The FunctionInfo region (large
headers, exception tables, debug info) and `SwitchImm` jump tables are 4-aligned
(**[source]** `BYTECODE_ALIGNMENT = alignof(uint32_t)` and `INFO_ALIGNMENT = 4`, unchanged at
v99; note `SwitchImm` is spelled `UIntSwitchImm` from v99, which also adds a `StringSwitchImm`
and a matching `numStringSwitchImms` file-header field — neither affects alignment). **Size
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
**[source] Confirmed upstream at v99**, which settles it beyond inference:
`SmallStringTableEntry::isOverflowed()` is literally `return getLength() == INVALID_LENGTH;`
with `INVALID_LENGTH = (1 << 8) - 1`. `INVALID_OFFSET = (1 << 23)` exists, but appears only
inside the *constructor's* does-it-fit test (`entry.getOffset() < INVALID_OFFSET && …`) — it
is never a read-side sentinel anywhere in Hermes. F1's finding #3 was exactly right, and the
23/8 bitfield split (plus the 1-bit `IsUTF16` that I7 keys on) is unchanged from v96 to v99.

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
- ~~**Modern output cannot be verified from Rust.**~~ **REPEALED, and replaced by a working
  harness** (`tests/vm_verify.rs`, `scripts/build_hermes_vm.ps1`). The premise
  was that driving a modern VM needs C++ because `hermesvm` exports only mangled C++/JSI
  symbols with no C ABI. True — and irrelevant, because you do not need to *link* the VM.
  `hvm.exe` is a standalone command-line VM driver that takes a `.hbc` path; a
  `std::process::Command` reaches it and the crate stays pure Rust. See Reference VM and
  toolchain. Two knock-on corrections:
  - **USAGE.md § "Why modern output cannot be verified inside the Rust tool"** (docs/USAGE.md:150)
    is now wrong on its central claim and on "macOS only". Rewrite it.
  - ~~**The `warn_modern_write` note points at a script that does not exist.**~~ Fixed: it
    now names `scripts/build_hermes_vm.ps1` and `tests/vm_verify.rs`, and both exist. It also
    now states the real constraint — only v98 and v99 modern layouts are known, anything else
    is refused. (R20.)
- **`create` produces a single global function** with hardcoded shape (legacy: flags
  `0x12`, frame 2, param 1 — `serialize.rs:179`; modern: `ProhibitNone` overflowed global
  — `serialize.rs:313`). It is a smoke-test artifact, not a general emitter. **At v99 it is
  also not executable** — see The v99 delta.
- **Opcode tables are regenerated by hand.** `tests/upstream_pin.rs` detects drift against a
  checkout and says exactly what differs, but there is no tool that applies the fix. A general
  regenerator was written and abandoned: the three `Bytecode*.json` files are artifacts from
  different eras with different indentation and different per-entry fields, and one that
  imposes a single shape silently destroys real data (v96's populated `AbstractDefinitions`).
  Normalise the three files before trying again. See R19 and The v99 opcode drift.
- **Only two modern layouts are known: v98 and v99.** This *is* a real limitation, but now a
  declared one. `ModernLayout::for_version` is an allow-list; v97 (a genuinely different
  20-byte-large-header vintage with a 16-bit packed pointer and different small-header bit
  widths) and any future v100+ are **hard errors**, not best-effort guesses. Adding a version
  means adding one row. This replaces the old unstated assumption that "modern" was one shape,
  which is what R8 was.

---

## Reference VMs and toolchain (facebook/hermes)

A facebook/hermes clone lives at `C:\src\hermes-src`, with one built `git worktree` per
bytecode version beside it — `C:\src\hermes-v96`, `-v98`, `-v99`. These are the ground truth
this doc is checked against, and each is a *build* as well as a checkout, so both the source
and the running engine are available. `scripts/build_hermes_vm.ps1` produces them.

### v99 means the release branch

`C:\src\hermes-v99` is `origin/260318099.0.0-stable` (`b7b58dd3c`) — the branch React Native
ships from — and **not** `static_h`, which is what the clone is on. The distinction is easy to
miss and expensive:

| | `static_h` (`e9edc8b52`) | release (`b7b58dd3c`) |
|---|---|---|
| `BYTECODE_VERSION` | 99 | 99 |
| `BytecodeFileFormat.h` | \<byte-identical\> | \<byte-identical\> |
| `NewFastArray` | `DEFINE_OPCODE_3(…, Reg8, Reg8, UInt16)` — 5 bytes | `DEFINE_OPCODE_2(…, Reg8, UInt16)` — 4 bytes |

So the version integer does not identify the dialect, and neither does the header layout —
`modern_layout_matches_upstream_headers` passes against *either* checkout. Only
`opcode_tables_match_upstream` separates them. A real v99 bundle comes from the release
branch, so that is what `resources/bytecode/Bytecode99.json` encodes; pointing
`HERMES_SRC_V99` at `static_h` fails the pin with a one-opcode operand-count mismatch, and
that failure is the tripwire doing its job.

When a checkout legitimately moves, re-derive the table rather than hand-editing it:

```powershell
python scripts/gen_bytecode_table.py --version 99 --src C:/src/hermes-v99 `
    --commit (git -C C:/src/hermes-v99 rev-parse HEAD)
python scripts/gen_bytecode_table.py --version 99 --src C:/src/hermes-v99 --check  # verify only
```

### What each binary is good for

| Binary | Use | Notes |
|---|---|---|
| `hvm.exe <f.hbc>` | **Execute a patched image.** The verifier. | Prints program output; non-zero exit + a JS stack trace on an uncaught error. This is the whole "modern verification" problem, solved by a subprocess. |
| `hvm.exe -d <f.hbc>` | VM-side header dump + disassembly | Disassembles *instead of* running. Independent of `hbcdump`'s path, so a useful second opinion |
| `hermesc.exe -emit-binary -out f.hbc f.js` | **Mint fixtures.** | Deterministic, sub-second; the only way to get a *known-good* modern image to diff against |
| `hbcdump.exe -mode=objdump <f.hbc>` | **Reference disassembler + table dump.** | Interactive; drive it non-interactively as `echo disassemble \| hbcdump -mode=objdump f.hbc`. Prints the string table with kind, byte range **and identifier hash** (`i3[ASCII, 14..18] #CE5FC8AC: risky`) — a direct oracle for I9's Jenkins implementation |
| `hbc-diff.exe`, `hbc-deltaprep.exe` | delta form | Not used here; note `DELTA_MAGIC = ~MAGIC` marks a non-executable form |
| `hbc-attribute.exe` | per-function byte attribution | Useful in principle (confirms `headers:function:small` = 12 B, `headers:global:bundle` = 128 B) but **crashes partway through on Windows** — do not build tooling on it |

### The one real constraint: each VM is version-locked

`hvm.exe` refuses anything but its own version, with a clean message rather than a crash:

```
$ hvm.exe c96.hbc
Wrong bytecode version. Expected 99 but got 96
```

So this build verifies **v99 only**. That is the *opposite* of the coverage the project most
needs — Equinox is **v96** — so do not read "we can verify modern now" as "we can verify the
bundle we actually ship". Two independent gaps remain:

**Both gaps are now closed** — `scripts/build_hermes_vm.ps1` builds a per-version VM into a
`git worktree` beside the clone, leaving the original checkout untouched:

| Version | Upstream ref | Why it matters |
|---|---|---|
| **96** | `2afc7b09f` | **The Equinox bundles.** The only VM that can verify the legacy paths this project actually ships against |
| **98** | `origin/250829098.0.0-stable` | The RN-shipped v98, and the 37-byte arm of `ModernLayout` |
| **99** | `origin/260318099.0.0-stable` / `static_h` | The 36-byte arm |

Each needs small MSVC/CMake portability patches, applied idempotently by the script and
explained at each call site (upstream does not build these tools on Windows/MSVC, so these
are toolchain fixes, not semantic ones): v96 needs a CMake-4 policy fix and a union
value-init fix in `HadesGC.cpp`; v98 needs two `__builtin_expect` calls routed through the
project's own `SH_LIKELY` macro and `winmm` linked for the sampling profiler; v99 needs
none. The script smoke-tests each build (compile `print("ok")`, run it) before reporting
success, because a VM binary that exists but rejects its own compiler's output is worse than
no binary.

### What this changes about testing

`hvm` is a subprocess, so a VM check is an ordinary integration test, not an FFI project.
This is now built, as `crates/hbc-decomp/tests/vm_verify.rs`:

1. Fixtures are committed `.hbc` files (~700 bytes each), compiled from
   `tests/fixtures/*.js` by `build_hermes_vm.ps1 -Fixtures`. Two programs:
   `plain` (no handlers anywhere) and `handlers` (a try/catch/finally exercised on **both**
   paths, so a stale handler table changes the output rather than hiding).
2. Each write op runs, then its output runs under the matching `hvm`, asserting **stdout and
   exit code** — not "it reparses".
3. Gated on `HERMES_VM_V<N>`; with none set the tests still assert everything that does not
   need a VM and print a skip note for the rest.

Point 2 is the part that matters, and it was checked the only way worth checking: **the three
original defects were reintroduced one at a time and the suite failed each time**, with
diagnostics that name the cause ("fixture should have at least one function with a handler
table, found none — the header layout is being misread"). A test that has never been seen to
fail is a hypothesis.

Two design notes worth keeping:

- **Assert the fixture's own shape first.** `size_change_on_real_handler_table_is_refused`
  asserts *some* function has handlers before it iterates. Without that line, a layout drift
  that hides every handler makes the loop body run zero times and the test pass green — which
  is exactly the failure it exists to catch.
- **"The stub ran" is not "the output is correct."** On v99 the injected `log` prologue
  printed its function name *and* corrupted the handler table in the same edit. Assert program
  behaviour, not that the injected code executed.

Still **R21**, at 🟧: the harness exists and works, but it is opt-in, so a CI runner with no
VM configured is green while asserting nothing.

---

## The v99 delta — modern is not one layout

The write path *used to* treat "modern" as a single layout for all of v97+
(`FunctionHeaderLayout::Modern12`, `MODERN_FUNCTION_HEADER_MIN_VERSION = 97`). v99 falsifies
that. This section is the concrete statement of R8.

> **Fixed** — `crates/hbc-decomp/src/modern_layout.rs` is now the single source of truth for
> this, and every reader and writer of a modern large header indexes through it. The section
> is kept in full because the *shape* of the failure is the durable lesson, and because the
> field tables below are what a future version's row must be derived against.

### Where the layout lives now

| Concern | Where |
|---|---|
| The descriptor itself | `ModernLayout` in `crates/hbc-decomp/src/modern_layout.rs` |
| Reading a large header | `parse_large_header_modern` (`file/parser/function.rs`) |
| Relocating one on resize | `resize_overflowed_function` (`write/patch/functions.rs`) |
| Reserving stub registers | `reserve_modern_log_regs` (`write/patch/inject.rs`) |
| Emitting one from scratch | `build_minimal_modern` (`write/serialize.rs`) |
| Proving it against an engine | `crates/hbc-decomp/tests/vm_verify.rs` |

Adding a version is one row in `ModernLayout::for_version` plus a fixture. Until that row
exists the version is **refused**, which is the whole point.

### What is unchanged from v98 to v99

Reassuring, and worth stating so the fix stays small **[source]**:

- **The file header is byte-identical.** Same 23 `u32` fields in the same order, so
  `debug_info_offset` is at **108** on both, `overflow_string_count` at 56, `string_storage_size`
  at 60, `file_length` at 32. Every string-path offset the write path uses is still right.
- **`SmallFuncHeader` is still exactly 12 bytes**, same bitfields: `Offset:25, ParamCount:5,
  LoopDepth:2 | BytecodeSizeInBytes:14, FunctionName:8, NumberRegCount:5, NonPtrRegCount:5 |
  FrameSize:u8 | ReadCacheSize:u8 | WriteCacheSize:7+PrivateNameCacheSize:1 | flags:u8`. So
  `reserve_modern_log_regs`'s small-header offsets (frame `+8`, cache `+9`, `inject.rs:61`)
  are still correct, and so is `resize_modern_small`'s 25-bit body-offset field.
- **Q2 is confirmed verbatim.** `SmallFuncHeader(uint32_t largeHeaderOffset)` does
  `setOffset(x & 0xffffff); setFunctionName((x >> 24) & 0xff)` and reads it back as
  `(getFunctionName() << 24) | getOffset()` — the 24-bit packed pointer. The 25-bit field is
  the separate non-overflowed body offset. Two fields, both masks correct, exactly as Q2 said.
- **`AsyncBreakCheck` still exists** (`BytecodeList.def:687`), so Q8's padding path is live.
- **The handler table format is unchanged**: `ExceptionHandlerTableHeader { u32 count }` then
  `count × HBCExceptionHandlerInfo { u32 start; u32 end; u32 target; }`, and
  `INFO_ALIGNMENT = 4`.
- **The first 8 `u32`s of the large header are unchanged**, which is why `frame +28` and
  `read-cache +32` (R11) still land on the right bytes.

### What changed: one byte

`FUNC_HEADER_FIELDS` lost `NumCacheNewObject` in `7193d4485` "Remove CacheNewObject".
`FunctionHeader` (the large header) is `LLVM_PACKED`, so its size is just the sum of its
api-typed fields:

| | u32 fields | u8 fields | `sizeof(FunctionHeader)` |
|---|---|---|---|
| **v98** (`origin/250829098.0.0-stable`) | 8 | 5 — Read, Write, **NumCacheNewObject**, PrivateName, flags | **37** |
| **v99** (`origin/260318099.0.0-stable`, `static_h` HEAD) | 8 | 4 — Read, Write, PrivateName, flags | **36** |

`parse_large_header_modern` (`file/parser/function.rs:181`) hardcodes the v98 shape — it reads
a `num_cache_new_object` byte and computes `info_offset = align4(pos_after_37_bytes)`. Against
v99 that means:

- **`flags` is read one byte late**, so it is whatever follows the header (padding, or the
  handler table's `count`, or the next function's large header).
- **`info_offset` is 4 too high**: `align4(large + 37) = large + 40`, truth is `large + 36`.

### Measured consequences (before the fix)

Kept as the record of what the defect actually did, and as the specification for the
regression tests that now cover it.

**[measured]** on `hermesc`-built v99 fixtures against a build of
`feat/write-path-hardening`. The rebuild matters: the binary sitting in `target/release/`
predated the Q3/Q4 guard entirely, so an earlier run of these tests proved nothing about it.
Check what you are running before concluding a guard does not fire.

**1. The Q3/Q4 exception-handler guard is a coin flip.** It keys on
`fh.flags() & FLAG_HAS_EXCEPTION_HANDLER` (`functions.rs:43`) — a byte that is now garbage.
Both failure directions fire, in a three-function file:

```
true flags   read as   verdict
  0x1a         0x04     handlers MISSED  (function `risky`, 4 real handlers)
  0x12         0x4d     handlers INVENTED (function `plain`, zero handlers)
  0x12         0x00     correct by luck
```

*False negative* — `inject-stub log` on a function with four live handlers is accepted,
front-inserts a prologue, does not relocate the table, and the result is broken on the VM:

```
$ hvm t2.hbc            # baseline
no-throw: 3
throw: -1
$ hermes-decomp inject-stub --kind log --function 1 -o t2log.hbc t2.hbc   # accepted (!)
$ hvm t2log.hbc
no-throw: 3
Uncaught Error: risky                                    # catch no longer catches
    at risky (t2.js:7:13)
```

That is precisely the corruption Q3's guard exists to prevent, shipping with the guard in place.

*False positive* — in a file whose three functions have **no** handlers at all, two of them are
refused any size-changing edit, with a confident and wrong error:

```
Error: Write("function 1 has an exception-handler table; size-changing edits are not
supported (handler offsets are body-relative and would be left stale). See ... Q3.")
```

The same misread also surfaces in read-only output: `disasm --info` reports
`flags=[strict,overflowed] exc_handlers=1` for a function that has neither. And because
`parse_exception_handlers` (`parsing.rs:362`) gates on the same byte, `file.exception_handlers`
comes back empty for functions that do have tables — so the decompiler, which reconstructs
`try`/`catch` from that map, emits a bare `throw` with no catch. Out of scope for this doc, but
the same root cause, and worth knowing before trusting v99 decompiler output.

**2. `create --version 99` produces a file that loads but cannot run.** The 37-byte write puts
the flags byte at `large+36`; v99 reads `large+35`, finds `0x00`, and `0x00` is not "no flags"
— per `enum ProhibitInvoke { Call = 0, Construct = 1, None = 2 }` it means *plain calls are
prohibited*:

```
$ hermes-decomp create --version 99 -o c99.hbc && hvm c99.hbc
Uncaught TypeError: Class constructor invoked without new
```

Moving that one byte from offset 196 to 195 and refreshing the SHA1 footer makes the identical
file run clean (exit 0) — which isolates the cause to the header size and nothing else.

### The rule this yields

**The version number does not identify the layout.** On `static_h`, `BYTECODE_VERSION` stayed
at 98 across *four* distinct large-header shapes: `NumCacheNewObject` added 2025-03-19
(`a0298ddc9`), `PrivateNameCacheSize` added 2025-03-31 (`e42564dc6`), `NumCacheNewObject`
removed 2026-01-21 (`7193d4485`) — the bump to 99 came only on 2026-02-12. A file stamped
"v98" can be any of them.

This is the same disease the project's own CLAUDE.md warns about for patch anchors — *an
incidental value that looks structural*. A version integer is a fine corroborator and a
terrible layout selector. **Derive the layout from a descriptor keyed to a known-good Hermes
commit, and hard-error on a version outside the allow-list** (see R8's Hardening).

The repo already contains evidence of the drift, in its own resources: `Bytecode99.json`
carries `"GitCommitHash": "913d31acd…"` (2026-03-05), which is *after* `7193d4485` removed
`NumCacheNewObject`. **The opcode table and the header struct in this crate are pinned to
different Hermes commits.** And the opcode table has since drifted too — `d4f5193f0` changed
`NewFastArray` from `(Reg8, UInt16)` to `(Reg8, Reg8, UInt16)`, a 4→5 byte instruction, which
desynchronizes decoding for the remainder of any body containing one. (Static-Hermes-only
today, so not yet reachable from `hermesc` output — but it is the same failure shape.)
Tracked as **R19, still open** — the descriptor now records *a* layout per version, but
nothing yet asserts that it and `Bytecode*.json` were derived from the same upstream commit.
That check is the only thing standing between this project and a rerun of the whole episode.

---

## The v99 opcode drift — `===` read as `>=`

A second, independent drift from the same root cause as R8: something derived from upstream
was hand-carried instead, and nothing re-derived it. Found by `tests/upstream_pin.rs` on its
first run.

### What was wrong

`resources/bytecode/Bytecode99.json` contained four numeric-jump pairs — `JGreaterN`,
`JGreaterEqualN`, `JNotGreaterN`, `JNotGreaterEqualN` — that upstream had deleted in
`d2cd42a34` "Delete unnecessary numeric jumps". They were already gone at `913d31acd`, the
commit the file's own `GitCommitHash` names, so the table was never generated from the commit
it claims.

**Opcode number is position in `BytecodeList.def`.** Eight phantom entries therefore pushed
every later opcode eight positions up:

| Opcode | Upstream v99 | Our table said |
|---|---|---|
| 208–209 | `JEqual` / `JEqualLong` | 216–217 |
| 210–211 | `JNotEqual` / `…Long` | 218–219 |
| 212–213 | `JStrictEqual` / `…Long` | 220–221 |
| 214–215 | `JStrictNotEqual` / `…Long` | 222–223 |
| 216–219 | `JmpBuiltinIs(-Not)` / `…Long` | 224–227 |

So on v99, **every `===` and `==` in a conditional decoded as a numeric comparison**. This
source:

```js
function pick(a, b) {
  if (a === b) { return "same"; }
  if (a == 1)  { return "one"; }
  return "other";
}
```

disassembled as `JGreaterEqualN` and `JGreaterN` — i.e. it read as `a >= b` and `a > 1`.

### Why nothing caught it

The substituted opcodes have **identical operand shapes** (`Addr8, Reg8, Reg8`), so nothing
desynchronised, nothing errored, and every downstream consumer saw a well-formed instruction
stream. A decompiler would confidently emit `if (a >= b)`. This is the worst failure mode
available to a disassembler: not a crash, not garbage, but a fluent lie.

It also explains why fixtures could not find it. The earlier v99 work disassembled `risky`
and `plain` and matched `hbcdump` exactly — because neither used `===`. Opcode-numbering
errors are invisible until you execute the specific opcode that moved.

`NewFastArray` had drifted the same way (`d4f5193f0` gave it a third operand), and *that* one
would have desynchronised decoding for the rest of any body containing it. It is
Static-Hermes-only, so no `hermesc` output reaches it — a latent version of the same bug.

### What fixed it, and what did not

`Bytecode99.json` was regenerated from `BytecodeList.def` at the current upstream commit,
preserving the two things that are ours rather than upstream's:

- the trailing `S` on string-id operands (`UInt16S`), which is the same width as the unmarked
  type and marks which operand holds a string-table id — `patch-operand` uses it;
- `IsJump`, which is **not** derivable: v96's `SwitchImm` has an `Addr32` operand and is
  deliberately not flagged as a jump, so "has an Addr operand" is wrong as a rule even though
  it happens to hold for every entry of the v99 table.

The emitter was required to reproduce the untouched file byte for byte before being trusted
with modified data. That check is worth keeping as a habit: a generator that has never been
shown to reproduce its input is not a generator, it is a reformatter.

**A general regenerator was attempted and abandoned**, after it destroyed `Bytecode96.json`
twice. The three tables are heterogeneous artifacts from different eras — v96 is tab-indented
and carries a populated `AbstractDefinitions` plus a per-entry `AbstractDefinition` field, v98
has no `GitCommitHash` at all, v99 has an empty `AbstractDefinitions` — and a regenerator that
imposes one shape silently drops real data. Only v99 was wrong; v96 and v98 pass the pin check
unchanged. Regenerating a drifted table is therefore still manual (R19).

---

## High-risk areas by category

**What the git history confirms empirically (see Git history findings).** There are now **two**
bug classes with a track record on this write path.

**Class 1 — missing input validation before a raw byte write.** Four instances in a single
review (finding F1): a string id not checked against `string_count`, an `insn_offset` not
bounded by the body size, a masked field trusted as a sentinel, and a `file.strings[x]` index
taken before `x` was validated. Apply that checklist to every new op first.

**Class 2 — a layout constant not keyed to the version it was read from** (finding F7, added
this pass). Three instances, all one root cause: the modern large-header size is hardcoded at
the v98 value, so at v99 the handler-flag read, the handler-table location and `create`'s
emitted header are all wrong. This class is **invisible to review** — the code is
self-consistent, the comments are correct, and the output reparses — and invisible to the
existing tests, which assert reparse rather than execution. It is caught only by running
output on the engine. When adding an op, ask not just "did I validate the input" but "**which
constants in this code would change if Hermes bumped a version, and what tells me**".

Second empirical fact: the highest-severity areas below live in
files authored once as a monolith (`functions.rs`, `inject.rs`, `create.rs`, `serialize.rs`,
`header_write.rs`) — though `functions.rs` and `inject.rs` have received their first
independent tests (finding F5, updated). Class 2 lands squarely in that same never-independently-
reviewed set.

### Risk register (re-evaluated)

The durable spine of this doc. A **risk is permanent** — it exists as long as the hazard's
shape exists in the code — so `R#` IDs are never renumbered or deleted; a mitigated risk just
drops its `Residual`. Everything temporal lives as *columns you edit in place*, never as a
separate list that drifts: `Residual` is the live status, `Mitigation` is what's already true
in the tree, and **`Hardening` is the single home for the todo — the action that lowers
residual, with any open decision stated inline** (this is where former "questions" and
"backlog tasks" now live). Cross-references everywhere use `R#` only.

`Inherent` = likelihood × impact *before* mitigation; `Residual` = the risk that remains today.
Ratings: 🟥 high · 🟧 medium · 🟩 low · ⬜ resolved (inherent was high; a shipped guard/fix
retired it — kept to show the downgrade). Sort by `Residual` for priority.

| R# | Hazard | § | Inherent | Residual | Mitigation (in tree) | Hardening (todo + open decision) |
|---|---|---|---|---|---|---|
| R1 | Chaining a 2nd op on stale `file.sections` (I2) | string/fn | M×H | ⬜ **fixed** | `commit_image` re-derives the whole model (including `sections`) from the finalized bytes, so a second op cannot see a stale layout. Pinned by `chained_size_changing_ops_need_no_reparse`, which runs three chained size-changing ops with no re-parse and requires the output to execute on a real VM | — (fixed). **Decision taken** (was: refresh vs dirty-guard): refresh. The guard would have removed the footgun; the refresh removes the *class*, and costs one parse per op (~40ms on a 5MB bundle) that I2 already told callers to pay. |
| R2 | Overflow entry encode/decode (I8) | string | M×H | 🟩 | create/retarget refuse overflow; **now exercised against 1,449 real overflowed entries** by `tests/corpus.rs`, which re-implements the `len == 0xff` sentinel independently and requires it to agree with the header count | One `is_overflow_entry`/`encode_overflow_entry` keyed on `len == 0xff`, used by every string path; delete the dead `off == 0x800000` branches (`strings.rs:44`, `:124`). Downgraded from 🟧 because the *detection* rule is now verified against production data; **encoding** an overflow entry is still unbuilt and untested (`create` refuses it). |
| R3 | Legacy `debug_info_offset` position mis-gated | string/create | L×H | 🟧 | `legacy_debug_info_offset_pos` centralizes it | Round-trip assert after create/resize: reparse and check `debug_info_offset` + gated section sizes match intent (shared with R14). |
| R4 | `string_kinds` / id-hash desync (I9/I12) | string | L×H | 🟩 | append-only path handled; Q7; model can no longer drift from the bytes (R5) | Assert identifier hashes against `hbcdump`'s printed values (`i3[…] #CE5FC8AC: risky`) rather than against our own Jenkins implementation — the corpus harness has the plumbing for it. |
| R5 | Structured model ↔ bytes drift (I1) | all | M×M | ⬜ **fixed** | `commit_image` re-derives the model by reparsing, so the two cannot disagree. The debug assertion that preceded it found *every* op partly stale on its first run — see Git history findings F8 | — (fixed). The remaining hand-sync code inside the ops is now redundant rather than load-bearing; harmless, but do not add more. |
| R6 | UTF-16-by-content / %4 padding (I7/I5) | string | L×M | 🟩 | content-driven + tested | — |
| R7 | Non-%4 body delta misaligns FunctionInfo (I5) | fn/inject | L×H | ⬜ | Q8 hard-error | — (retired) |
| R8 | Modern large-header magic offsets (v99+) | fn | L×H | ⬜ **fixed** | `ModernLayout` (`modern_layout.rs`) is a version-keyed descriptor; `parse_large_header_modern`, `resize_overflowed_function`, `reserve_modern_log_regs` and `build_minimal_modern` all index through it. Unknown modern versions hard-error instead of extrapolating. Both arms (v98=37B, v99=36B) VM-verified | — (fixed; the standing task is to add a new version's row to `ModernLayout::for_version` when one appears, which R19 makes checkable) |
| R9 | Exception-handler offsets stale on resize | fn/inject | M×H | ⬜ | Q3/Q4 guard rejects the edit, and now reads `flags` from the right byte on every supported layout (R8). Both directions pinned by `size_change_on_real_handler_table_is_refused` and `handler_free_functions_accept_size_change_and_still_run` on **real v96/v98/v99 fixtures**, not synthetic flags | — (guarded; full relocation = Pending impl plans / Q3, no longer blocked) |
| R10 | Relative jump broken by a partial insertion | fn | L×H | 🟧 | only whole-body / front-insert used today | Keep insertions whole-body or front-only; document the same-shift invariant so a future partial-insert op can't quietly break it. |
| R11 | Reg/cache reservation magic offsets | inject | L×H | ⬜ | The offsets were correct (the 8×u32 prefix never moved) but were literals; now taken from `ModernLayout`, so they are a checked fact rather than luck | — (fixed with R8) |
| R12 | Hardcoded injected-opcode operand shapes | inject | L×H | 🟧 | availability checked, layout assumed | Validate injected opcodes' arity/types against the def table at inject time instead of hardcoding `TryGetById`/`Call2`. |
| R13 | `NopPad` appended after a non-terminator | inject | L×L | 🟩 | usually terminator-ended | Guard: error if the function doesn't end on a terminator. |
| R14 | `create` section-order / header field gating | create | M×H | 🟧 | version-gated writer, no round-trip assert | Round-trip header assert (shared with R3) + CLI/integration harness (shared with R17) + a VM run (R21). |
| R15 | Modern large-header field order in `create` | create | L×H | ⬜ **fixed** | `build_minimal_modern` writes fields at `ModernLayout` offsets and sets `PROHIBIT_NONE` at `large_flags_pos()`. `create_minimal_runs_on_vm` asserts the output **executes** on the matching engine for every fixture version | — (fixed with R8) |
| R16 | `create` writes a zero `source_hash` | create | L×L | 🟩 | fine for minimal images | **Decision:** compute the real `source_hash` vs keep zero and document created files as "unsigned at source". Minor; only once `create` backs a real emitter. |
| R17 | No CLI / integration coverage | all | M×M | 🟧 | `hbc-decomp-cli/tests/stdout_contract.rs` covers the stdout/stderr contract and the exit-code path across six commands, and needed the debug-stack fix (F9) to be possible at all | Extend beyond the stdout contract to argument resolution: `--at` vs `--function`+`--insn-offset` precedence, `--string` vs `--string-id`, `--from`/`--to` value→id lookup. Those are still untested. |
| R18 | stderr has no formal log levels — ad-hoc `warning:`/`note:`/plain prefixes | cli | L×M | 🟩 | two-channel split is honored (data→stdout, diagnostics→stderr); ERROR is the `Result`/exit path; implicit severity via wording | Formalize the INFO/WARN prefixes (a tiny `eprintln`-wrapping helper, no external crate — keeps the pure-Rust ethos); keep ERROR on the `Result`/exit path, not a stderr line. **Decision:** local 2-line helper vs a `log`/`tracing` dep — recommend the local helper. See Stdout/stderr discipline. |
| R19 | Bundled `Bytecode*.json` and the header-struct code are pinned to **different** Hermes commits, and neither pin is checked | all | M×H | 🟧 | `tests/upstream_pin.rs` re-derives both from a checkout and fails when either disagrees. It found the v99 opcode drift immediately | **Detection is done; regeneration is not.** Fixing a drifted table is still manual JSON surgery, because the three tables are heterogeneous artifacts (v96 is tab-indented with a populated `AbstractDefinitions`; v98 has no `GitCommitHash`) and a one-size regenerator destroys real data — verified the hard way. Either normalise the three files first, or write a regenerator that edits `Definitions` in place. |
| R20 | CLI points users at a verifier script that does not exist | cli | H×L | ⬜ | `warn_modern_write` now points at `scripts/build_hermes_vm.ps1` and `tests/vm_verify.rs`, both of which exist; docs/USAGE.md's "cannot be verified" section is rewritten around `hvm` | — (fixed). Note nothing *tests* stderr text, so this class can rot again; see R17/R18. |
| R21 | No VM check anywhere in CI — "reparses" is treated as "correct" | all | H×H | 🟧 | `tests/vm_verify.rs` runs each write op on a real `hvm` (v96/v98/v99) and asserts stdout + exit code; `tests/corpus.rs` sweeps a production bundle; `tests/upstream_pin.rs` re-derives the format from upstream. Verified to fail on every defect they were written for | Residual stays 🟧 for one reason: **every one of these is gated on an env var, so a runner with none set is green while asserting nothing.** Either provision the Hermes builds and the corpus on the runner, or add a job that fails when they are absent. A silently-skipping suite is the same failure shape as a suite that asserts the wrong thing. |
| R22 | An unoptimized build of the CLI overflows its stack | cli | H×M | ⬜ **fixed** | `run` is one large match over every subcommand and a debug build gives each arm's locals their own slot in one frame, exceeding Windows' 1 MiB main-thread stack. Work now runs on a 64 MiB-stack thread (F9) | — (fixed). The underlying shape is unchanged: the match still holds every arm's locals at once, so splitting arms into functions is the real fix if the frame grows again. Note the release build was always fine, which is why this survived — *test what CI builds*. |
| R23 | An op's output is only ever checked against our own model | all | M×H | 🟩 | Three independent oracles now exist: a real VM (does it run), upstream headers and `BytecodeList.def` (does our format model match theirs), and `hbcdump` (does a second implementation read the same instructions) | Keep reaching for an external oracle when adding a check. The three findings this pass — stale model, opcode drift, debug stack overflow — were each invisible to a test written against our own assumptions, and each fell out immediately once something else was asked. |

Grid (residual likelihood × impact; resolved items listed below it for the downgrade earned):

```
                       IMPACT  →
             Low             Medium            High
  High        ·               ·                R21
L
I Med         ·               R17              R14  R19
K
E Low       R13 R16           R6 R18           R3 R10 R12
L
  Fixed (each was a live defect, not a hypothetical):
      R1 ⬜ · R5 ⬜ · R8 ⬜ · R9 ⬜ · R11 ⬜ · R15 ⬜ · R20 ⬜ · R22 ⬜
  Resolved earlier or downgraded by evidence: R4 🟩 · R7 ⬜ · R2 🟩 · R23 🟩
```

Reading it: **R21 is now the lone high/high**, and it is a peculiar one — the harnesses it
asks for all exist and are all proven to catch what they were written for, but every one is
gated on an env var, so a CI runner without a Hermes build and the corpus passes while
asserting almost nothing. That is the same shape as the problem this whole document has been
about: a suite that looks green because it is checking the wrong thing, or nothing.

**R19** is the standing tripwire for the next upstream reshape; detection is built, the fix
path is not. **R2** dropped to 🟩 on evidence rather than on work — 1,449 real overflowed
entries now exercise the detection rule — but note the asymmetry: *reading* an overflow entry
is verified, *writing* one is still unimplemented. The remaining low-likelihood / high-impact
cluster (R3, R10, R12) is the offset-arithmetic and hardcoded-shape hazards that fire rarely
and corrupt silently, so the payoff there is still **loud failure** over silent mis-encode.

**R9 is the cautionary tale worth keeping, even now that it is fixed.** A guard was written,
tested, and reasoned about carefully — the Q4 note even explains, correctly, why
`FLAG_HAS_EXCEPTION_HANDLER` beats `info_offset != 0` — and it was retired to ⬜ on the
strength of that reasoning plus a unit test that *set the flag synthetically*. The test never
read a real header, so it could not notice that on v99 the flag is not where we look. **A
guard is only as good as the field it reads, and a synthetic test asserts your own assumption
back to you.** Its replacement, `size_change_on_real_handler_table_is_refused`, reads
hermesc-built fixtures and asserts up front that *some* function has handlers — so a future
layout drift that hides them all fails the test instead of vacuously passing it. That
up-front assertion is the load-bearing line; without it the test would still pass while
testing nothing.

### New string ops
- ~~**R1 · Chaining without re-parse (I2)**~~ — **retired.** This was the single most
  likely corruption: a second string op against a `file` whose `sections` were stale
  after the first resize. `commit_image` re-derives the model, `sections` included, so
  chaining is safe. Nothing to remember beyond ending the op with `commit_image`.
- **R2 · Overflow handling (I8).** Copying the dead `offset == 0x800000` check instead of
  `len == 0xff`; forgetting the 8-byte overflow-slot layout; forgetting to update
  `overflow_string_count` at `[56..60]` and `string_storage_size` at `[60..64]`. The
  *detection* rule is now checked against 1,449 real overflowed entries by
  `tests/corpus.rs`; *encoding* one is still unimplemented.
- **R3 · Header field positions.** String counts sit at fixed offsets `[44..64]` shared across
  layouts, but `debug_info_offset` differs: **modern fixed at byte 108**, **legacy computed
  by `legacy_debug_info_offset_pos`** (`strings.rs:294`), which itself depends on
  version-gated fields (bigint present? function_source present?). A wrong legacy position
  writes garbage into a random header field with no immediate error.
- **R4 · `string_kinds` runs (I12)** and **identifier ordering/hash (I9).** Inserting rather
  than appending, or appending an identifier without extending the hash table + bumping
  `identifier_count`, desynchronizes the identifier hash index.
- **R6 · UTF-16-by-content (I7)** and **`%4` storage padding (I5).**
- ~~**R5 · Model sync (I1)**~~ — **retired.** Forgetting to push to `file.strings` or
  bump `file.header.*` used to leave later reads (and the CLI's post-op status text)
  lying, and every op was doing exactly that. The model is derived from the bytes now,
  so there is nothing to forget.

### New function ops
- **R7 · Alignment (I5).** Any body whose new length isn't `%4`-aligned relative to the old must
  be padded; the existing pad trick inserts `AsyncBreakCheck` *before the terminator*
  (`functions.rs:54`) so the function still ends on a terminator. When padding is required
  but the version has no `AsyncBreakCheck`, it now **hard-errors** (Q8) instead of shipping
  a misaligned delta — residual risk retired (⬜).
- **R8 · Overflowed functions.** Must relocate the small-header pointer **and** the large
  header's internal fields (`resize_overflowed_function`, `functions.rs:277`). Legacy large
  header: body offset, size, info fields rewritten in the `slot..slot+16` copy
  (`functions.rs:219`); modern reads the packed pointer via `read_modern_large_pointer`
  (`functions.rs:287`). These magic offsets are v98-shaped; a version whose large header
  differs will be silently mis-patched. **This happened** — v99's large header is 36 bytes,
  not 37. The packed-pointer read and the 8×`u32` prefix survived; the trailing `u8` block and
  the derived `info_offset` did not. **Fixed:** these are no longer magic offsets; they come
  from `ModernLayout` and an unknown version is refused. See The v99 delta.
- **R9 · Exception handlers are guarded, not relocated (Q3/Q4).** `patch_function_body`
  **rejects any size-changing edit** on a function that
  declares an exception-handler table (`flags & FLAG_HAS_EXCEPTION_HANDLER`,
  `functions.rs:43`), because handler start/end/target offsets are body-relative and are not
  yet rewritten. Same-size edits are allowed. The logic was always right; for a while the
  **input** was not — `fh.flags()` for a modern overflowed function comes from
  `parse_large_header_modern`, which read that byte one position late at v99, so the guard
  fired essentially at random (measured both ways — see The v99 delta). **Fixed** via
  `ModernLayout`, and pinned by two tests on real fixtures rather than synthetic flags. Two
  facts shaped the fix and still constrain anything built on it **[source]**:
  - `BytecodeSerializer::serializeFunctionInfo` emits a large header for **any** function with
    handlers *or* debug info, regardless of whether it would fit in a small one. So on modern,
    **a function with handlers is necessarily overflowed** — the guard never needs to consider
    a non-overflowed modern function.
  - `SmallFuncHeader(uint32_t largeHeaderOffset)` `memset`s to zero and sets **only**
    `Overflowed`. An overflowed function's *small* header therefore has
    `FLAG_HAS_EXCEPTION_HANDLER == 0` **always**, on every version. The VM reads the flag from
    the large header (`BCProviderFromBuffer::getExceptionTableAndDebugOffsets`), and so must we.
    Falling back to the small header's flags is not a safe simplification — it is a silent
    always-allow. The converse also bites: the *large* header never carries `Overflowed`, so
    the parser reinstates that one bit when building `FunctionHeader::Modern` (otherwise
    `is_overflowed()` and `has_overflowed_functions()` answer "no" for a modern file in which
    every function is overflowed). Overflow is decided by the small header, every other flag by
    the large one.
- **R10 · Relative-jump safety depends on same-shift.** Body-internal `Addr8`/`Addr32` jumps hold
  deltas relative to their own instruction; front-insertion keeps caller and target moving
  together, so relative jumps survive — but this is a *property being relied on*, not a
  recomputation. A partial insertion (between a jump and its target) would break it.
- **Modern small-header field width (24 vs 25 bits).** Resolved — different fields, both
  correct. See Q2.

### Stub / inject work
- **R11 · Register/cache reservation must persist and be enough.** `log_frame_size` bumps frame
  by `max(4)+8` and reserves one read-cache slot (`inject.rs:19`, `:36`). Legacy edits the
  struct then relies on the resize path rewriting the full header; modern edits raw header
  bytes *before* the splice via `reserve_modern_log_regs` (`inject.rs:28`) at magic offsets
  (small: frame byte `+8`, cache byte `+9`, `inject.rs:61`; large: frame `+28`, cache `+32`,
  `inject.rs:56`). A stub needing more registers must widen this, and the magic offsets are
  version-fragile.
- **R12 · Hardcoded opcode operand shapes.** `build_log_entry` bakes in `TryGetById reg,reg,u8
  cache,u16 string` and `Call2 reg,reg,reg,reg`. Opcode *availability* is checked; operand
  *layout* is assumed constant across versions.
- **R9 · Exception-handler staleness (above)** applies doubly to inject, which front-inserts a
  prologue into an existing body — hence the Q3/Q4 guard covers `inject-stub` too (it funnels
  through `patch_function_body`).
- **R13 · `NopPad` insertion point.** Inserts `AsyncBreakCheck` before the last `Ret`, or at the
  end if there is none (`inject.rs:232`). "At the end" is only safe if the function already
  ended on a terminator; a function ending in a fallthrough would gain a reachable no-op
  (usually fine) but the assumption should be stated.
- **R7 · `AsyncBreakCheck` no longer silently skipped (Q8).** If the version lacks it and padding
  is required, both `patch_function_body` and `build_log_entry` hard-error. The no-pad-needed
  path (delta already `%4`, or a version that has `AsyncBreakCheck`) is unchanged.

### New `create` variants
- **R14 · Section order + header field gating.** `write_legacy_header` (`header_write.rs`) writes
  fields in a version-gated order (bigint if `has_bigint`, segment vs cjs, function_source if
  `v>=84`). Adding a populated section means emitting it in the body **and** matching its
  size into the correct gated header slot; a mis-gate shifts every later field.
- **R15 · Modern large-header field order** was hand-encoded in `build_minimal_modern` and
  had to match the parser exactly, including the packed small→large pointer and the
  `ProhibitNone` flag semantics. **Matching the parser was the bug**: the parser was v98-shaped,
  so at v99 the flags byte was written one position too late and the VM read `0x00` there —
  which is `ProhibitInvoke::Call`, i.e. *calls prohibited*, not "no flags". **Fixed:** fields
  are written at `ModernLayout` offsets and `create_minimal_runs_on_vm` asserts the result
  executes. Note the asymmetry worth remembering: the enum is
  `{ Call = 0, Construct = 1, None = 2 }`, so **zero is not the permissive value** and a
  misaligned or zero-filled flags byte fails closed and loudly rather than quietly — the one
  piece of luck in this whole finding.
- **No overflow support (design limit above).** A create variant taking large tables must
  add overflow encoding first.
- **R16 · `create` now emits `warn_modern_write`** (`write_cmd.rs:403`) and still sets a zero
  `source_hash` — fine for minimal images, but a variant meant for real use should reconsider
  the latter.

---

## Stdout/stderr discipline

**The two-channel model — the split *is* the contract:**

- **stdout = the requested output data**, and nothing else — the machine-consumable result the
  invocation was *for*. Only data-producing commands write here: `secrets` (report), `emit-hasm`
  without `-o` (HASM text), `add-string` (the bare new id). A command that only transforms a
  file into `-o` writes **nothing** to stdout. This is load-bearing for scripting:
  `id=$(hbc-decomp add-string …)` must capture the id and *only* the id. `add-string` originally
  broke it (human text on stdout) — a bug fixed in `316741f` (finding F3), which is why the rule
  is stated rather than assumed.
- **stderr = the diagnostics / log channel** — human status, progress, notes and warnings:
  everything *about* the run rather than the run's output. Redirecting or discarding stderr must
  never change the captured data.

**On severity levels (your INFO/WARN/ERROR model — agreed in spirit, but implicit today):**
stderr *is* the log channel, but the levels are not formalized:

- **WARN** — lines prefixed `warning:` (cross-kind retarget, `*ById` non-identifier) or `note:`
  (duplicate string).
- **INFO** — plain status lines (`Patched string → …`, `Created minimal HBC …`, `Injected
  stub …`, the modern-write note). No prefix; the level is only inferable from wording.
- **ERROR** — **not a stderr log line at all.** Errors bubble as `Result` to `main`, which
  Debug-prints and sets a non-zero exit code (see Exit codes). So "ERROR level" lives in the
  exit path, not the log.

There is **no `log`/`tracing` crate**; the prefixes are ad-hoc. So the durable contract is the
stdout/stderr *split*, not the levels — formalizing the INFO/WARN prefixes is tracked as **R18**
(low residual). Do **not** teach a consumer to parse stderr by level; parse stdout for data and
read the exit code for success/failure.

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

**A wrong INFO line, not just a missing one (R20 — fixed):** the modern-write note printed by
every write command used to tell the user to build
`scripts/build/build_hermes_v98_toolchain.sh`, a file that has never existed in this repo. It
was the most frequently emitted sentence the tool produces and it sent people nowhere. It now
names `scripts/build_hermes_vm.ps1` and `tests/vm_verify.rs`, and states the real constraint
(only v98 and v99 modern layouts are known; anything else is refused). docs/USAGE.md's
"cannot be verified" section is rewritten to match. The discipline point survives the fix:
**stderr text ages exactly like prose docs, and nothing tests it** — the same reason F3's
stdout bug survived. If the stdout/stderr contract ever gets a test (R17), the note's
existence claims are worth asserting too.

**Remaining inconsistency (an INFO-line gap, part of R18):** `emit-hasm -o` prints no
confirmation, while every other `-o` writer emits an INFO status. The shared `write_output`
helper *does* print "Wrote … (N lines, KiB)" — but `run_emit_hasm` uses a bare `std::fs::write`
(`write_cmd.rs:143`) and bypasses it. Fix alongside R18's prefix formalization.

**Guidance for new commands:** a command that yields a machine value (a new id, an offset)
puts *only* that value on stdout, like `add-string`; a command that only transforms a file into
`-o` keeps stdout empty and reports on stderr.

**This is now asserted, not just documented** (R17). `hbc-decomp-cli/tests/stdout_contract.rs`
checks that `add-string` puts a bare parseable id on stdout, that file-transforming commands
leave stdout empty, that `emit-hasm` without `-o` writes HASM to stdout, that discarding
stderr does not change stdout, and that failures keep stdout clean. It also asserts that the
file paths named in the modern-write note exist in the repo — a dead reference is what R20
was. Writing it required fixing a long-standing stack overflow in unoptimized CLI builds; see
finding F9.

**Exit codes** are uniform: handlers return `Result`, errors bubble to `main` which returns
`Box<dyn Error>` → non-zero exit with the error Debug-printed. Keep new commands on this
path (no `process::exit`, no `unwrap`/`panic` on user input).

---

## Test harnesses

What exists, what each one asserts against, and how to run it. The organising idea is that
**every harness checks against something outside this crate.** The unit suite compares our
output to our expectations, which is exactly why it could not see any of the defects found
this pass.

| Harness | Oracle | Env | Gated? |
|---|---|---|---|
| unit tests (`#[cfg(test)]`) | our own expectations | — | no |
| `tests/vm_verify.rs` | **a real Hermes VM** — does the patched image run and print the right thing | `HERMES_VM_V96` / `_V98` / `_V99` | skips |
| `tests/upstream_pin.rs` | **the upstream headers** — `FUNC_HEADER_FIELDS` and `BytecodeList.def` | `HERMES_SRC_V96` / `_V98` / `_V99` | skips |
| `tests/corpus.rs` | **a production bundle**, plus `hbcdump` as a second disassembler | `HBC_CORPUS_BUNDLE`, `HBC_CORPUS_LIMIT`, `HERMES_HBCDUMP_V96` | skips |
| `hbc-decomp-cli/tests/stdout_contract.rs` | **the process boundary** — real stdout, stderr, exit codes | — | no |
| `commit_image` (`serialize.rs`) | **the bytes themselves** — the model is re-derived from them | — | no, always on |

```powershell
# One-time: build the VMs (and the fixtures and hbcdump they need).
# Each lands in its own worktree beside the clone: C:\src\hermes-v96, -v98, -v99.
96, 98, 99 | ForEach-Object {
    ./scripts/build_hermes_vm.ps1 -Version $_ -HermesRepo C:\src\hermes-src -Fixtures
}

$env:HERMES_VM_V96    = 'C:\src\hermes-v96\build\bin\Release\hvm.exe'
$env:HERMES_SRC_V96   = 'C:\src\hermes-v96'
$env:HERMES_HBCDUMP_V96 = 'C:\src\hermes-v96\build\bin\Release\hbcdump.exe'
$env:HBC_CORPUS_BUNDLE = 'C:\apks\...\index.android.bundle.backup'
$env:HBC_CORPUS_LIMIT = '0'      # sweep all 62,909 functions (~9s); default 2000
cargo test
```

⚠️ **"Gated? skips" is the live weakness (R21).** With no env vars set, those three suites
pass while asserting nothing at all. That is deliberate — a checkout without a Hermes build
must still be testable — but it means a green CI run does not currently imply much. Provision
the binaries where CI runs, or add a job that fails when they are missing.

### What each is good at, and what it cannot see

- **`vm_verify`** is the only thing that distinguishes "reparses" from "runs". Every defect
  in the modern branch reparsed cleanly. It cannot tell you *why* something broke, and it
  only covers what a small fixture can express.
- **`upstream_pin`** is the only thing that catches upstream changing the format. It cannot
  see a bug in our own handling of a format we model correctly. Note it fails *loudly and
  specifically* — the message names the opcode and whether the operand count changed.
- **`corpus`** is the only thing that exercises inputs we cannot construct: 1,449 overflowed
  string entries, 4,786 UTF-16 strings, functions with real exception tables, and every
  opcode the compiler actually emits. The bundle is third-party and not committed, so this
  suite is the most likely to be silently skipped.
- **`stdout_contract`** is the only thing that observes the tool the way a script does. It is
  also the only harness that needs no external artifact, so it is the one that will still be
  running in five years.
- **`commit_image`** is not a test but a structural guarantee, which is stronger: I1 cannot
  be violated because the model is no longer independently maintained.

### Two design notes that are load-bearing

Both came from harnesses that would otherwise have passed while testing nothing:

1. **Assert the fixture's own shape before iterating over it.**
   `size_change_on_real_handler_table_is_refused` asserts that *some* function has an
   exception table before looping. Without that line, a layout drift that hides every
   handler makes the loop body run zero times and the test pass green — the exact failure it
   exists to catch.
2. **Align by identity, never by position.** The hbcdump differential keys on the function id
   parsed from hbcdump's header line, because hbcdump omits the outer stubs of generator
   functions (it jumps 1137 → 1139 → 1141). Aligned positionally, it silently compares
   different functions from the first generator onward and reports a flood of "mismatches"
   that are really one desync.

---

## Test matrix gaps

Per command, cases that are **absent** from the current tests (derived from the `#[cfg(test)]`
modules). Everything listed below is unit-level; the integration harnesses that sit
alongside it are described under Test harnesses. An earlier pass
added CI tests to `functions.rs` (8), `inject.rs` (5), `operands.rs` (7), `strings.rs` (25)
that build a real image with `create_minimal` (rather than skipping on a missing fixture) —
several formerly-missing cases are now **covered** and marked so below.

> ⚠️ **The gap this list did not have a row for — now largely closed.** Every test in
> the `#[cfg(test)]` modules asserts that output *reparses*. Not one asserts that it
> *runs*, that it matches an independent implementation, or that our format model
> matches upstream's. Four harnesses now cover those (see Test harnesses), and between
> them they found three defects the unit suite was structurally incapable of seeing.
>
> Read the per-command gaps below as **second-order**. The first-order question is no
> longer "which case is missing" but "which oracle is missing" — and the remaining
> answer there is R21: every external oracle is opt-in, so an unconfigured run asserts
> almost nothing.
>
> A second, subtler lesson from R9: `size_change_on_function_with_handlers_is_rejected` passes,
> and the behaviour it names is broken on v99, because the test **sets the handler flag
> synthetically** rather than reading a real header. Prefer a `hermesc`-built fixture over a
> hand-set field whenever the thing under test is "do we read this format correctly".
>
> **Now covered by `vm_verify.rs`**, VM-asserted on all three versions: `patch-string`
> same-length / grow / **shrink** / **ASCII→UTF-16**, `add-string`, `retarget-string`,
> `inject-stub` on handler-free functions, `create`, and the handler guard in both directions.
> Several of these were listed below as gaps and are struck through accordingly.
>
> **Now covered by `corpus.rs`**, against a production bundle: the **overflow-entry**
> detection rule (1,449 real entries — no fixture has one), encode/decode symmetry for
> **62,909 of 62,909** function bodies, uniform offset relocation across all of them, and an
> instruction-level differential against `hbcdump` for 62,637 of them. Together these close
> the "only a dozen opcodes are ever exercised" gap that no fixture-based test could.
>
> **Now covered by `stdout_contract.rs`**: the stdout/stderr split and the exit-code path,
> across six commands (R17, partly).

- **`create`** (`create.rs`): has v96-parses, v98-parses. Still missing: the
  string-too-long / overflow **refusal** path. ~~a boundary v97; unsupported/low versions~~
  — now covered by `create_refuses_unknown_modern_version` plus `ModernLayout`'s own tests
  (v97 and v100 both hard-error). ~~**no test that a created file executes**~~ — now
  `create_minimal_runs_on_vm`, which is the assertion that would have caught the v99
  `ProhibitInvoke::Call` failure.
- **`encode`** (`encode.rs`): v96 + v98 body round-trips. Still missing: every **error** path
  (arity mismatch, value-too-wide per operand type); `Double`/`Imm32`/`Addr8`-range
  operands; the type-tolerance no-op branch (I13/Q6).
- **`footer`** (`footer.rs`): fixture match + rehash-identity. Still missing: `rehash_footer`
  on a `< 20`-byte buffer; `verify_footer` on a truncated/short image.
- **`functions`** (`functions.rs`): **now covered** — grow, shrink, alignment-pad, modern-v98
  overflowed resize, `debug_info_offset` shift (fixture-gated), the handler-size-change
  rejection guard, and the Q8 missing-`AsyncBreakCheck` hard error. Still missing: a function
  **with a real exception-handler table** exercised through actual bytecode — **this is the
  test whose absence hid R9**, because the guard test sets the flag synthetically and so
  asserts our own layout assumption back at us. A `hermesc`-built try/catch fixture plus an
  `hvm` run is the fix — **done**: `tests/fixtures/handlers.*.hbc` plus
  `size_change_on_real_handler_table_is_refused`, which also asserts the fixture really has
  handlers so a layout drift fails instead of vacuously passing. **modern-on-VM** is likewise
  covered, opt-in via `HERMES_VM_V98`/`HERMES_VM_V99`.
- **`inject`** (`inject.rs`): v96 nop; v98 nop+log. **Now covered** — legacy `LogEntry` on
  v96, the no-`"print"`-string error, the overflowed-legacy refusal. Still missing: any check
  that the injected code actually runs. Note these are two separate assertions, not one:
  measured on v99, the `log` stub **did** run (it printed the function name) while the same
  edit **corrupted** the handler table. "The stub works" does not imply "the output is correct".
- **`operands`** (`operands.rs`): absolute + function-relative round-trip; no-string-operand,
  nonexistent-id, insn-offset-OOB rejections; **now** the Q9 `*ById` warn / non-`*ById`
  no-warn cases. Still missing: **`--operand-index` selection** on a multi-string opcode
  (e.g. `CreateRegExp`); the **width-overflow rejection** (id larger than operand width);
  `UInt16S`/`UInt32S` operands; **modern v98**.
- **`strings`** (`strings.rs`): broad — same-length, grow-resize, packed→resize, ascii→utf16,
  retarget (6 cases), add_string (10 cases incl. modern v98), and **now `patch_string_replace`
  (`--old`)**: same-length, grow, and not-found error. Still missing as *tests*: **shrink**
  resize; **resize of an identifier** (hash refresh under the resize path, as opposed to
  same-length/retarget); **patch/resize on modern** (only `add_string` is modern-tested); a
  UTF-16 in-place edit taking the forced-resize path; asserting the cross-kind retarget warning
  now that it lives in the CLI layer. Of these, **shrink, modern patch/resize and modern
  ASCII→UTF-16 were run on a real v99 VM this pass and were correct** — they need transcribing
  into tests, not investigating. The identifier-hash gap is the one with no evidence either
  way, and `hbcdump` prints identifier hashes directly (`i3[…] #CE5FC8AC: risky`), so it can be
  asserted against the engine's own value rather than against our reimplementation of Jenkins.
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

"Modern" == `FunctionHeaderLayout::Modern12`, i.e. HBC **v97+** (12-byte function headers).
`MODERN_FUNCTION_HEADER_MIN_VERSION = 97` (`header.rs:10`). `FLAG_OVERFLOWED = 0x20`,
`FLAG_HAS_EXCEPTION_HANDLER = 0x08` (`format.rs:22`, `:16`).

⚠️ **Two corrections to the framing itself, both from the v99 source.**

1. **"Modern" is not one layout.** `Modern12` is accurate about the *small* header (12 bytes,
   same bitfields v97→v99) and wrong about the *large* one, which changed size at v99. Every
   row below that says "Yes" to modern-aware means "aware of the v98 modern layout". See
   The v99 delta.
2. **"every real function overflowed" is the wrong reason, and it matters.** Functions are not
   overflowed because their fields don't fit — they are overflowed because
   `serializeFunctionInfo` forces it for anything with exception handlers **or debug info**.
   In a `hermesc`-built file with debug info that is indeed every function; strip debug info
   and it is not. The load-bearing consequence for Q3/Q4 is narrower and always true:
   *a modern function that has handlers is always overflowed*, so a handler-aware guard only
   ever needs the large-header path.

Full per-path fork status. "Tested on modern?" means a unit test actually parses/edits a
Modern12 image. "v99 VM" is this pass's manual `hvm.exe` result — ✅ ran correctly,
🔴 measured broken (none remain; kept in the key because the tests exist to bring it back if
a layout drifts again), `—` blocked before it could run.

| Path | Modern-aware? | Tested on modern? | VM | Fork mechanism / notes |
|---|---|---|---|---|
| `add-string` | **Yes** | **Yes** (v98) | ✅ | full modern branch (`strings.rs:544`); modern debug-off=108, **confirmed unchanged at v99** |
| `patch-string` same-length | Layout-agnostic | No | ✅ | `locate_string_bytes` uses sections (`strings.rs:16`) |
| `patch-string` resize | **Yes** | **No** | ✅ grow, shrink, ASCII→UTF-16 | modern debug-off=108, hsize=12, overflow relocate (`strings.rs:316`). Untested in CI but now **measured** on a real v99 engine |
| `patch-string --old` (replace) | **Yes** (via resize) | **No** | ✅ | by-value lookup then resize/same-length (`strings.rs:860`) |
| `retarget-string` | Layout-agnostic | No | ✅ | touches small table + id hash only (`strings.rs:215`); refuses overflow |
| `patch-operand` | Layout-agnostic | No | ✅ | decodes at offset (`operands.rs:89`) |
| `asm` / `patch-function` | **Yes** | **Yes** (v98) | ✅ identity | `resize_modern_small` + `resize_overflowed_function` (now `ModernLayout`-driven); tested (`modern_v98_overflowed_resize_reparses`). Reachability is gated by the Q3/Q4 check, which is now correct |
| `asm-check` (`run_roundtrip_check`) | inherits `asm`/`emit-hasm` | No | ✅ `OK` | `write_cmd.rs:410`; no test |
| `inject-stub` | **Yes** | **Yes** (v96 + v98 + v99) | ✅ | `reserve_modern_log_regs` was already correct at v99 (frame `+28`/cache `+32` never moved), now via `ModernLayout`; the failure had been upstream, in the handler guard that let it run |
| `create` | **Yes** | **Yes** (v96 + v98 + v99) | ✅ runs | `create_minimal` dispatches to `build_minimal_modern` at v≥97; writes a `ModernLayout`-sized large header and is asserted to execute |
| `emit-hasm` | read-only | v98 fixture exists | n/a | disassemble only. Cross-checked against `hbcdump` on v99: **instruction-for-instruction identical** |
| `secrets` / `frida-hooks` | read-only | — | n/a | analysis; no layout fork on the write side |

**The pattern that column showed is worth keeping.** Before the fix, everything string-shaped
was ✅ and everything function-header-shaped was 🔴. That was not luck: the string paths key off
the *file* header, which is byte-identical v98→v99, while the function paths key off the *large
function* header, which is the one thing that changed. It is also why the fix was small — one
descriptor, one root cause — and why the string half of the write path was in better shape than
its 🟢s suggested. Expect the same split next time upstream reshapes something.

**`warn_modern_write` coverage:** now emitted by **every** write command that opens a file —
`asm`, `patch-operand`, `retarget-string`, `add-string`, `patch-string`, `inject-stub`, **and
`create`** (`write_cmd.rs:403`, added this pass). `emit-hasm` (read-only) does not emit it.

**Modern gaps / fragilities:**
- ~~**Hardcoded v98 large-header field offsets**~~ — **R8, fired and fixed.** Modern resize
  used to rely on literal frame `+28`, cache `+32`, size/body offsets and the packed pointer,
  with no abstraction over the layout, so v99's different FunctionInfo shape was mis-encoded
  with no error. All of it now goes through the version-keyed `ModernLayout`. Which offsets
  survived the drift is recorded in The v99 delta (all of them in the unchanged 8×`u32`
  prefix; only the trailing `u8` block moved) — worth reading before assuming the next change
  will be equally kind.
- **24 vs 25 bit body-offset field — resolved (Q2), not a bug, and re-confirmed at v99.** The
  24-bit mask (`read_modern_large_pointer`, `header_write.rs`) reads the **overflowed** packed
  large-header pointer (offset portion 24 bits); the 25-bit mask
  (`shift_modern_small_header_offset`, `header_write.rs:113`; `resize_modern_small`,
  `functions.rs:246`) shifts the **non-overflowed** body-offset field (25 bits). Different
  fields; both correct. The v99 source states both verbatim (see Q2).
- **VM verification: built, opt-in.** The old blocker (no C ABI, macOS-only helper) was never
  real — `hvm` is a subprocess. `tests/vm_verify.rs` now runs every write op on **v96, v98 and
  v99** engines, so the legacy paths that matter for Equinox are verified by machine rather
  than by a one-time manual check. What remains: the tests are gated on `HERMES_VM_V<N>`, so a
  runner without those binaries passes without asserting anything. See Reference VMs, R21.
- **Handlers on modern.** The Q3/Q4 guard correctly keys on `FLAG_HAS_EXCEPTION_HANDLER` rather
  than `info_offset != 0` (which would reject every overflowed modern function and break the
  documented modern `inject-stub` path). That choice was always right; what was wrong was
  *where the flag was read from* at v99. Now fixed: the flag comes from the large header at the
  `ModernLayout` offset for that version, while `Overflowed` comes from the small header,
  because neither header carries both. See Q4 and R9.

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

> **Update (hardening pass, on `feat/write-path-hardening`).** That pass revised `functions.rs`,
> `inject.rs`, `operands.rs`, `strings.rs`, `header_write.rs`, `serialize.rs` and
> `write_cmd.rs`: it implemented the Q3/Q4 guard, Q5, Q6, Q8 and Q9, corrected the Q2 comment,
> reconciled the create-modern docs (Q1), and added the **first independent tests** to
> `functions.rs` and `inject.rs`. So F5's "never been through the impl→fix→test loop" no
> longer holds for those two files — but `create.rs`, `serialize.rs` and `header_write.rs`
> are still untested beyond what `create`/resize exercise indirectly, and **both of the v99
> 🔴s land in exactly those files** (`serialize.rs`'s `build_minimal_modern`, and the parser
> the guard trusts). F5's "unproven, not stable" reading held up.

> **Update (v99 pass, docs-only).** This revision changed no code. It re-derived the format
> facts from a compiled facebook/hermes at `BYTECODE_VERSION = 99` and ran the write path's
> output on that engine. Everything it found is recorded above as R8/R9/R15/R19/R20/R21 and
> finding F7; no fix has been made yet.

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
  especially for modern output. **That caveat has now cashed out**: the claim was true for the
  versions listed, and v99 — which did not exist when it was written — is measured broken. A
  one-time verification is a statement about a moment, not a property of the code.

- **F7 — the v99 pass (this revision), and what it says about the failure model.** Three
  defects, one root cause, none of them a coding error: the modern large header changed size
  upstream and nothing in this crate is positioned to notice. Note what *did* hold up —
  string handling, the packed pointer, the register-reservation offsets, the 25-bit field,
  `AsyncBreakCheck`, the overflow sentinel, the handler-table format. The parts derived from a
  written-down invariant survived a version bump; the parts derived from a hardcoded byte count
  did not. **Implies:** add a third entry to the empirical bug class alongside F1's "missing
  input validation" — **a layout constant that is not keyed to the version it was read from.**
  It fails differently from F1's class: F1's bugs produced invalid bytecode on bad *input*,
  whereas this one produces invalid bytecode on perfectly good input, on a version nobody
  tested. Review cannot catch it (the code is self-consistent and the comments are accurate);
  only running the output against the engine can.

- **Adjacent corroboration (read path, not write).** `bf32a5d` "Fix xref on Modern (HBC98)
  layout…", plus `203671b`/`5ba55ca`/`102cc61` (v96 debug-capacity overflow, parser integer
  overflow/underflow panics, malformed-bytecode crashes) show that **Modern-layout handling
  and offset arithmetic are recurring bug loci across the whole codebase.** The write path's
  modern branch and its offset-shifting math are unlikely to be exceptions.

---

- **F8 — the model was stale in every write op, and no test could see it.** A debug assertion
  at the single point where an op commits its result failed immediately, on four separate
  tests, for two distinct causes: `patch_function_bytes` rewrote every function's header
  *bytes* and never touched `file.function_headers` at all; `patch_string_replace` on a
  growing string shifted every function offset in the bytes and none in the model.
  **Implies:** a hand-maintained parallel representation does not stay correct, and its
  drift is invisible to any test that only round-trips the bytes. The fix that lasts is to
  delete the parallel representation, not to synchronise it harder — which is why
  `commit_image` re-derives (I1). Note the assertion was written expecting to find nothing;
  it is worth writing checks you expect to pass.

- **F9 — the debug CLI binary had always overflowed its stack, and that is probably why R17
  stayed open.** Writing the first CLI integration test revealed that *any* invocation of an
  unoptimized `hermes-decomp` — including `--help` — died with `thread 'main' has overflowed
  its stack`. Verified against the branch point, so it long predates this work. `run` is one
  large `match` over every subcommand, and an unoptimized build gives each arm's locals their
  own slot in a single frame; the total exceeds Windows' 1 MiB main-thread stack.
  **Implies two things.** First, *release-only correctness is a real category*: the optimized
  build was always fine, so nothing surfaced it, while `cargo test` builds debug and so any
  CLI harness was impossible — a missing test caused by a bug that only a test would reveal.
  Second, when a gap in coverage persists across several passes with no clear reason, suspect
  a mechanical blocker rather than lack of will.

- **F10 — a table claiming a provenance it did not have.** `Bytecode99.json` records
  `GitCommitHash: 913d31acd…`, and the opcodes it contains had already been deleted upstream
  at that commit. The pin was decorative: written once, never checked, and wrong.
  **Implies:** recorded provenance is worth nothing without something that verifies it. When
  adding a pin, add the check in the same change, or the pin becomes a claim that ages
  silently — see R19 and The v99 opcode drift.

## Open questions

Decisions a future impl agent must not guess at.

- **Q1 — ✅ `create` and modern (v97+): RESOLVED, docs reconciled.** Intent settled by
  `50cdbf8`'s commit message (finding F6) and the code + `create_minimal_v98_parses`. The
  docs are now brought into line in this pass: `USAGE.md` says create emits "legacy layout
  for v96 and lower and modern layout for v97 and newer"; the `warn_modern_write` note text
  says the same (`write_cmd.rs:25`) and is now emitted by `create` too (`write_cmd.rs:403`);
  the stale `build_minimal_legacy` guard message now reads "v97 and newer use modern layout
  (build_minimal_modern)" (`serialize.rs:94`). Nothing left to decide.
- **Q2 — ✅ Modern small-header body-offset field: 24 or 25 bits? RESOLVED — no-op.** The
  24-bit and 25-bit masks cover *different* fields, and both are correct:
  `read_modern_large_pointer` reads the **overflowed** packed large-header pointer, whose
  offset portion is 24 bits (`function_name << 24 | offset & 0x00ff_ffff`, per parser);
  `shift_modern_small_header_offset` (`header_write.rs:113`) and `resize_modern_small`
  (`functions.rs:246`) shift the **non-overflowed** body-offset field, which is 25 bits (per
  parser Modern12 bitfield map `offset : (0, 25)`). No non-overflowed read uses 24 bits, so
  there is no single-field inconsistency to align. The `header_write.rs` comment was corrected
  in an earlier pass to say so explicitly.
  **[source] Now confirmed directly against v99**, which puts it beyond inference:
  `SmallFuncHeader(uint32_t largeHeaderOffset)` writes `setOffset(x & 0xffffff)` +
  `setFunctionName((x >> 24) & 0xff)` and reads back `(getFunctionName() << 24) | getOffset()`
  — the 24-bit packed pointer, byte-for-byte what `read_modern_large_pointer` does. And
  `FUNC_HEADER_FIELDS` declares `Offset, 25` — the separate non-overflowed body offset. Two
  fields, two masks, both right. Nothing to do; this Q can be considered closed permanently.
- **Q3 — 🟡 Exception-handler tables on size-changing edits: interim guard shipped; 🔵 full
  relocation planned.** Contract chosen: handler `start`/`end`/`target` are **body-relative**
  (0-based, `end` exclusive; confirmed — `decode_function_instructions` emits 0-based offsets
  and the CFG compares handler offsets directly against them, `jump_analysis.rs:134`). They are
  safe under a pure *string-region* growth (the whole tail shifts uniformly, offsets stay
  relative) but **not** under a body-internal size change (`patch-function`/`asm`/`inject`).
  Interim resolution: `patch_function_body` (`functions.rs:43`) **rejects any size-changing
  edit** on a function that declares an exception-handler table, rather than ship stale
  offsets. Full relocation is planned — see Pending impl plans. Remove the guard once it lands.
  **Status: the guard was inoperative on v99 and is now fixed.** The contract above was always
  correct — body-relative, 0-based, `end` exclusive, safe under string-region growth — and the
  v99 source confirms the table format unchanged. What had broken was the guard's *input*: see
  Q4 and The v99 delta. R8 was a prerequisite for Q3 and **is now done**, so Phase 1 is
  unblocked: the table can be located correctly on every supported layout, which is what
  relocation needs.
- **Q4 — ✅/⚪ `HasmFunction.exception_handlers`: unimplemented feature, not a drop. Guard
  shipped; build-vs-guard for whole-body ops is a call for Keith.** The field is *never populated*:
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
  **🔴→⬜ The guard did not work on v99, in both directions; fixed.** The choice of *which
  flag* was always right. The defect was *where the flag is read*: for a modern overflowed
  function `fh.flags()` comes from `parse_large_header_modern`, which was pinned to the 37-byte
  v98 large header, so at v99 it read the byte one past `flags`. Measured: a function with four
  live handlers was accepted and corrupted; two functions with none were refused. See The v99
  delta for the numbers. Three constraints shaped the fix, all **[source]**-confirmed at v99:
  1. Read `flags` from the **large** header at the offset for *that version* (R8's descriptor).
  2. Never fall back to the small header. `SmallFuncHeader(uint32_t largeHeaderOffset)`
     `memset`s and sets only `Overflowed`, so an overflowed function's small-header
     `FLAG_HAS_EXCEPTION_HANDLER` is `0` on every version — a fallback is a silent always-allow.
  3. You only need the overflowed path. `serializeFunctionInfo` forces overflow for any
     function with handlers or debug info, so on modern a handler-bearing function is always
     overflowed; and per `getExceptionTableAndDebugOffsets`, the VM does not even look at a
     non-overflowed function's info.
  The regression test is a `hermesc`-built try/catch fixture, not another synthetic flag:
  `size_change_on_real_handler_table_is_refused` (rejects the unsafe edit) and
  `handler_free_functions_accept_size_change_and_still_run` (does not block the safe one), both
  across v96/v98/v99. `handler_bearing_function_is_never_silently_corrupted` is the belt-and-
  braces one: if the guard is ever lifted, the patched program must still take its catch path.
- **Q5 — ✅ Should library patch functions write to stderr at all? RESOLVED — no.** All three
  library `eprintln!`s were removed. `patch_string_operand` now *returns* `(bytes, status,
  warning)`, which `run_patch_operand` prints (`operands.rs`, `write_cmd.rs:200`/`:202`). The
  `retarget_string` cross-kind warning and the `add_string` duplicate note are recomputed and
  printed by their CLI handlers (`run_retarget_string`, `run_add_string`). CLI output is
  byte-identical to before; programmatic callers get no unsolicited stderr. Status ownership
  now lives entirely in the CLI layer.
- **Q6 — ✅ `encode_instruction` operand-type tolerance: RESOLVED — no-op branch; safe.** The
  `if op.ty != *expected_ty { … }` block at `encode.rs:24` is **empty**. Encoding is always
  driven by the definition's `expected_ty`, so the decoded instruction's own `op.ty` tag is
  intentionally ignored. The only "tolerance" with effect is in `write_operand`, which accepts
  several `OperandValue` variants for a given width **but range-checks every narrowing** and
  returns a hard `Error` on overflow or an incompatible variant. It does **not** mask
  width/overflow bugs. It does not validate operand *kind/role* — that is Q9, not an encoder
  bug.
- **Q7 — ✅ Identifier placement: RESOLVED — no leading/contiguous requirement.** Empirical proof
  from a real hermesc-compiled bundle (`com.equinoxfitness.equinox_11.39.0`, HBC v96): its
  `string-kinds` table has **four interleaved runs** — `Identifier×255, String×15013,
  Identifier×50267, String×33382` — i.e. hermesc emits `String → Identifier` transitions and
  multiple non-contiguous identifier regions, and the VM loads them. The identifier hash table
  is indexed by *running identifier count* (`identifier_index`, `strings.rs:174`; I9/I12),
  which is arrangement-independent. `add_string`'s trailing Identifier run is exactly the shape
  hermesc already ships. Residual: inference from production layout, not a direct VM run of
  `add_string`'s specific output — but the resulting layout is structurally identical to
  bundles the VM already executes.
- **Q8 — ✅ `AsyncBreakCheck` as universal no-op padding: RESOLVED — hard error when needed and
  absent.** `AsyncBreakCheck` is **absent in `Bytecode40`–`Bytecode60` and present in
  `Bytecode61`–`Bytecode99`** (introduced at v61) — **[source]** still present at v99 upstream
  (`BytecodeList.def:687`), so the top of that range is confirmed against the engine and not
  just against our own bundled tables. Every version the write path realistically
  targets (≥76; Equinox is v96) has it, so the padding path is normally taken. **IMPLEMENTED:**
  the silent skip in `patch_function_body` (`functions.rs:54`) and `build_log_entry`
  (`inject.rs:90`) is now a hard `Error::Write` **only on the path where padding is actually
  required** (size delta not `%4`, or injected prologue not `%4`, and no `AsyncBreakCheck`
  available). The no-pad-needed path is unchanged. Covered by
  `missing_asyncbreakcheck_pad_is_hard_error` (v56).
- **Q9 — ✅ `patch-operand` semantic (kind) validation: RESOLVED — warn only.**
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

> The former Q10–Q12 (chaining refresh-vs-guard, unrecognized-modern-version policy, and
> `create`'s `source_hash`) were **decisions attached to hardening tasks**, so they now live
> inline in the risk register's `Hardening` column — see R1, R8, and R16. Q-numbers are kept
> stable for the resolved design decisions above; new work is tracked as risk attributes, not
> new questions.

---

## Pending impl plans

Fully-scoped plans for work that is decided but not yet built. Written so an impl agent can
execute without re-deriving the format. File:line references are to the tree state noted;
re-check them.

**Order matters: R8 before Q3.** Q3 relocates entries in a table it locates through the modern
large header, and R8 is the reason that location is wrong at v99. Doing Q3 first would build
correct relocation logic on top of a wrong pointer.

### R8 — Version-keyed modern large-header descriptor ✅ DONE

Kept as the record of what was built and why, since the field tables are what a future
version's row must be derived against.

**Shipped as** `crates/hbc-decomp/src/modern_layout.rs`: `ModernLayout::for_version(u32)`
over an allow-list, with `large_size()`, per-field byte offsets, `info_offset_for(large_ptr)`
and `small_write_cache_bits()`. Callers: `parse_large_header_modern`,
`resize_overflowed_function`, `reserve_modern_log_regs`, `build_minimal_modern`. Unknown
modern versions are a hard error, as decided.

**The data it encodes** — derived from `FUNC_HEADER_FIELDS` in
`include/hermes/BCGen/HBC/BytecodeFileFormat.h` **[source]**:

| | v97 / v98(early) | v98(late) | v99 |
|---|---|---|---|
| `SmallFuncHeader` size | 12 | 12 | 12 |
| small: frame / read-cache byte | — (different fields) | `+8` / `+9` | same |
| small: packed large pointer | `(name << 16) \| (off & 0xffff)` | `(name << 24) \| (off & 0x00ff_ffff)` | same |
| small: write-cache bits | — | 6 (1 bit to NumCacheNewObject) | 7 |
| `sizeof(FunctionHeader)` (large) | **20** | **37** | **36** |
| large: 8 × `u32` prefix | — (4 × u32) | `0..32` | same |
| large: trailing `u8`s | Frame, Read, Write, flags | Read `+32`, Write `+33`, **NumCacheNewObject `+34`**, PrivateName `+35`, flags `+36` | Read `+32`, Write `+33`, PrivateName `+34`, flags `+35` |
| handler table location | `align4(large + 20)` | `align4(large + 37)` | `align4(large + 36)` |
| supported here? | **no — hard error** | yes | yes |

Reference refs: `origin/250829098.0.0-stable` (v98), `origin/260318099.0.0-stable` (v99),
`16b5ada82` (the v97 bump, for the shape that is refused).

**Why v97 is refused rather than approximated.** It is not merely a different large-header
size: the *small* header's bit widths differ (paramCount 7, size 15, functionName 17) and the
packed large pointer is a 16-bit split. Supporting it needs a second small-header decoder, not
a number. No React Native release ever shipped v97 — only 98 and 99 have stable branches — so
the cost/benefit is clear, and the error message says exactly this.

**One subtlety the descriptor does not cover, and must not.** Overflow is decided by the
*small* header's flags byte (`MODERN_SMALL_FLAGS_POS`); every other flag comes from the large
header. Hermes' `SmallFuncHeader(uint32_t)` zeroes everything and sets only `Overflowed`, so
neither header alone tells the whole truth. `parse_large_header_modern` reinstates that one
bit when building `FunctionHeader::Modern`; the raw write paths read it from the small header
directly. Do not "simplify" either half.

**Tests.** `modern_layout.rs`'s own unit tests pin both arms and the refusals;
`tests/vm_verify.rs` pins the behaviour on real v96/v98/v99 fixtures under real VMs.

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
    (`function.rs:207–212`). ⚠️ **Do not take 37 from that function.** It is 8×u32 + 5×u8 = 37
    only at v97/v98; at v99 it is 8×u32 + 4×u8 = **36**, and `parse_large_header_modern` is
    itself the code R8 is fixing. Take the size from R8's descriptor, which is keyed to the
    version — and note the VM's own arithmetic is literally
    `buf += smallHeader.getLargeHeaderOffset(); buf += sizeof(FunctionHeader); align(buf);`
    **[source]**, so "size of the large header for this version, then align to 4" is the exact
    contract, with no separate stored offset to reconcile.
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
- **A VM run, not just a reparse.** `hvm.exe` on a `hermesc`-built fixture whose catch block
  is actually taken, asserting stdout and exit code — this is the only assertion that would
  have caught the v99 failure, and it is now a subprocess call rather than an external
  toolchain (see Reference VM). Run it for the modern cases; the legacy cases need a
  separately built older `hvm` (R21).
- Update/remove `size_change_on_function_with_handlers_is_rejected` (functions.rs tests) as
  the guard is lifted per phase.

**Unknowns to resolve before impl starts.**
1. **NopPad straddling a try region** (`start < P < end`): does only `end` shift (widening the
   try to cover the inserted no-op), or is inserting inside a live try region disallowed?
   Benign for a no-op but the semantics must be chosen. LogEntry (`P = 0`) never hits this.
2. **Scope of Phase 2 now vs later.** Confirm whether `patch-function`/`asm` handler support
   (and the HASM directive syntax) is in scope, or whether those keep the Q4 guard until a
   separate effort. Recommend: ship Phase 1, keep the guard for arbitrary resize.
3. ~~**Exact modern large-header size**~~ — **ANSWERED, and it was not a constant.** 37 at
   v97/v98, 36 at v99. The shared helper this unknown proposed is now specified as R8's
   version-keyed descriptor and is a **prerequisite**, not an optional tidy-up.
4. ~~**FunctionInfo beyond the handler table**~~ — **ANSWERED [source].** Per
   `BCProviderFromBuffer::getExceptionTableAndDebugOffsets`, the region is exactly:
   large header → `align(4)` → handler table (only if `HasExceptionHandler`) → `align(4)` →
   `DebugOffsets` (only if `HasDebugInfo`). Both subsections are gated by flags and both are
   4-aligned, and `serializeExceptionHandlerTable` pads to `INFO_ALIGNMENT = 4` before the
   table. Since Phase 1 keeps the handler table the same *size*, the debug offsets neither move
   within the region nor need rewriting — the region as a whole already relocates with the tail
   splice. Phase 1 does **not** need to touch debug info. (`DebugOffsets` holds offsets into the
   debug-info section, not body-relative values, so they are unaffected by a body-size change;
   this is the one part still worth an assertion rather than a claim, since debug info is an
   opaque buffer here per design limits.)
