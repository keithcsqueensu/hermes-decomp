# Write path reference — versions, layouts and reference VMs

> **What this is.** Background and derivation for `../RISKS.md`: the reference Hermes engines
> the write path is checked against, how "modern" is not one layout (the v99 delta), the v99
> opcode drift, why v97 names two opcode tables, and the full legacy/modern per-path fork
> status. This is *evidence*, not the risk register — the `R#`/`Q#`/`I#` identifiers referenced
> below are defined in `../RISKS.md`. Kept out of the register so the register stays a tracker;
> the shape of each failure is the durable lesson, which is why these are kept in full.

Evidence tags: **[source]** = read off `include/hermes/BCGen/HBC/BytecodeFileFormat.h` at the
named commit; **[measured]** = reproduced against a real `hvm.exe` on `hermesc.exe`-built
fixtures. The stage's *description* (non-risk) is `../../../arch_guides/01_READ_LAYER.md`
(version model) and `../../../arch_guides/06_WRITE_PATH.md`.

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
| **99** | `origin/260318099.0.0-stable` | The 36-byte arm. The release branch specifically — see v99 means the release branch |

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

Still **R21**, at 🟧, but narrower than it was. `HBC_REQUIRE_ORACLES` names the oracles a run
requires, and an absent one then fails with the variable to set instead of printing `[skip]`;
CI provisions the four source checkouts and runs `upstream_pin` under it. The VM half is still
opt-in — a runner with no `hvm` is green on `vm_verify` unless `HBC_REQUIRE_ORACLES=vm` says it
should not be.

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
Tracked as **R19, now fixed** — `tests/upstream_pin.rs` checks the descriptor and
`Bytecode*.json` against one checkout, so they cannot silently come from different commits, and
`tables_record_the_commit_they_came_from` additionally requires that checkout to be the commit
each table records. The episode did recur, twice, and both times the check caught it: v99
(`NewFastArray`) and v97 (`TypedLoadParent`/`TypedStoreParent` — where the disagreement turned
out to be v97 naming two tables; see v97 is two opcode tables).

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
unchanged. That abandoned attempt is why `scripts/gen_bytecode_table.py` preserves each file's
existing shape instead of imposing one, and why it is held to reproducing all four committed
tables byte for byte before it is allowed to write (R19).


---

## v97 is two opcode tables — the pin has to pick one

Adding a v97 checkout to `upstream_pin` immediately failed, in a table nobody had ever checked.
Against `e5c8ebf2f`, `Bytecode97.json` was missing `TypedLoadParent` and `TypedStoreParent`
(opcodes 149 and 150), so **every opcode from 149 onward was numbered two too low** — the whole
jump family included:

```
e5c8ebf2f v97         our table said
  149 TypedLoadParent   149 Jmp
  150 TypedStoreParent  150 JmpLong
  151 Jmp               151 JmpTrue
  153 JmpFalse          153 JmpUndefined
```

Read as a v99-style drift, and first fixed that way — regenerated from `e5c8ebf2f`, the last
commit that still declares 97. Checking the *other* end of the version's life showed that
reading was wrong, and the real finding is worse than a stale table.

v97 exists only on `static_h`, from `16b5ada82` (2024-05-24, the bump to 97) to `c00cc5759`
(2024-08-30, the bump to 98) — 518 commits. Of the three files the pin reads, only
`BytecodeList.def` moves across that span, and only at `e5c8ebf2f`:

| | `16b5ada82` — first commit declaring 97 | `e5c8ebf2f` — last commit declaring 97 |
|---|---|---|
| opcodes | 197 | 199 (`TypedLoadParent`/`TypedStoreParent`) |
| commits carrying that table | **517** | **1** |
| how long it existed | 3 months | **3 h 19 min** |
| `BytecodeFileFormat.h`, `BytecodeVersion.h` | byte-identical | byte-identical |

So **the version integer 97 names two different opcode tables**, and one `Bytecode97.json` can
only encode one of them. This is not a table that drifted away from its version; it is a version
that never had one table. The header shape is not involved: run the pin against an early tree and
`modern_layout_matches_upstream_headers` passes — only `opcode_tables_match_upstream` fails, by
exactly those two opcodes. Everything the refusal above rests on holds at both ends.

The pre-fix table was not from "some earlier commit" either: it is *exactly* the `16b5ada82`
table, names and operand types — v97-at-birth, correct for 517 of the 518 commits that ever
declared 97, and simply undeclared.

**Pinned at `16b5ada82`.** Two reasons. The rule it was supposed to follow — "the same rule as
the v96 ref" — actually picks this commit: `2afc7b09f` is `main`'s 95→96 bump, the *first*
commit declaring 96, not the last. And it is the arm 99.8% of v97's life carries, so if a v97
artifact ever did surface it is overwhelmingly the one that decodes it.

That rule is vacuous for v96, which is why the ambiguity went unnoticed: `main` has never left 96
(still 96 at HEAD, 1177 commits and three years later), and its `BytecodeList.def` and
`BytecodeFileFormat.h` are byte-identical from `2afc7b09f` to that HEAD — every commit in v96's
life gives the same tables. For v97 the choice is real, and a single table necessarily picks
which arm to be silently wrong about. Since v97 never shipped, no artifact is at stake either
way; what matters is that the arm is *declared* and the pin enforces it.

⚠ `main` and `static_h` are separate lines — they forked in 2022-08 and bumped the version
independently — so `2afc7b09f` is **not** an ancestor of the v97 bump, and "the last v96 commit
before v97" is not a thing that exists across the two.


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
  runner without those binaries passes without asserting anything, unless it sets
  `HBC_REQUIRE_ORACLES=vm`. See Reference VMs, R21.
- **Handlers on modern.** The Q3/Q4 guard correctly keys on `FLAG_HAS_EXCEPTION_HANDLER` rather
  than `info_offset != 0` (which would reject every overflowed modern function and break the
  documented modern `inject-stub` path). That choice was always right; what was wrong was
  *where the flag was read from* at v99. Now fixed: the flag comes from the large header at the
  `ModernLayout` offset for that version, while `Overflowed` comes from the small header,
  because neither header carries both. See Q4 and R9.

