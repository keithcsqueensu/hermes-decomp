# Read path — hardening review (what is solid, what is silent, what is missing)

> **Status: all 14 findings fixed.** This document is kept as written — the
> *analysis* is the durable part, and the measurements are what the fixes are
> justified by. Each finding now carries a **Fixed** note saying what changed and
> which test holds it. The two regression harnesses are committed:
> `tests/read_robustness.rs` (the corruption sweep) and `tests/read_diagnostics.rs`
> (one test per silent-degradation case). Verified after the fixes: 260,000
> mutants, **zero** panics; the full workspace suite plus the corpus tests against
> the shipped Equinox bundle all pass; decompiled output is byte-for-byte the same
> size as before (41,447,553 bytes) with zero depth markers and zero unresolved
> string placeholders.

Companion to `WRITE_PATH_GUIDE.md`, for the **read** side: everything from
`BytecodeFile::parse_auto` through disassembly, IR, analysis and the two front ends
(`hbc-decomp-cli`, `hbc-decomp-mcp`). The write guide is a standing reference built from
its own scars; this one starts the same register for reading.

Scope: parsing, decoding, analysis, and the CLI/MCP surfaces that expose them. The write
path is out of scope except where reading feeds it — and one finding below (**F5**) is
exactly that case.

> **Ownership.** *Owns* read-path robustness: F1–F14, what degrades silently, and the two
> regression harnesses. *Delegates* the debug section's format and interpretation to
> `UNMODELED_REGIONS_PLAN.md` (F10 is the read-side symptom; the formats are there), and the
> decompiler's closure/env-slot naming to `CLOSURE_MODEL_PLAN.md`.

Everything marked **[measured]** was reproduced in this pass against the committed fixtures
or the shipped Equinox v96 bundle (`com.equinoxfitness.equinox_11.39.0`, 16,837,408 bytes,
62,909 functions, 98,917 strings). Harness sources are in the appendix.

File:line refs are to the tree **as it was when the findings were written** (branch
`feat/write-path-hardening`, `455dcbb`) — the fixes have since moved most of them. The
descriptions of *what was wrong* are the durable part; re-derive a line number before
trusting it, which is this repo's standing rule for both guides.

---

## The headline

**The read path does not crash. It lies quietly instead.**

A 260,000-mutant corruption sweep over the whole parse → decode → disassemble → inspect
surface found **one** reachable panic site **[measured]**. That is a genuinely good result,
and the bounds work in `io.rs` (`capacity_hint`, `checked_add` in `read_exact`, saturating
math in `decode_string_table`) is why. Robustness is not the problem.

The problem is that **fourteen distinct "something is wrong" conditions all resolve to a
value that looks fine**, and the caller cannot tell any of them from success. `parse_auto`
returns a file parsed under the layout the version says it is *not*. `for_version_or_latest`
silently substitutes a different opcode table. `try_parse_debug_info` maps a parse error and
"this file has no debug info" onto the same `None`. `<string:412>` appears in output as if it
were a string. None of these raise, log, or set a flag.

That matters more here than in most parsers, because this crate's consumers are (a) a human
doing surgical patching who needs to trust an offset, and (b) an **agent** reading tool output
as ground truth. Both fail closed on an error and fail open on a plausible wrong answer.

The write path already learned this lesson — R8, R19 and the v99 opcode drift were all
"reasonable-looking output, silently wrong table". Every one was caught by *asserting against
an independent source*, not by making the code more careful. This pass does the same for the
read path: the fixes below are mostly not "be more careful", they are "record what happened
and let the caller see it", with a test per case asserting the record is made.

## Status at a glance

| # | Finding | Sev | Evidence | |
|---|---|---|---|---|
| **F1** | `parse_auto` returns the wrong layout, silently, on ~2% of corrupt modern files | High | [measured] | ✅ |
| **F2** | Opcode-table substitution is invisible on 2 of 3 entry points | High | code | ✅ |
| **F3** | MCP: no output bound anywhere — `decompile_all` returns 41 MB | High | [measured] | ✅ |
| **F4** | MCP: one panic poisons the mutex and bricks the server for its lifetime | High | code | ✅ |
| **F5** | Legacy large headers never reinstate `FLAG_OVERFLOWED` — 15 real functions misreported | Med | [measured] | ✅ |
| **F6** | Nothing on the read path checks the SHA-1 footer or `file_length` | Med | [measured] | ✅ |
| **F7** | `register_summary` overflows on file-supplied register counts (the one panic) | Med | [measured] | ✅ |
| **F8** | Cache `options_key` is hand-synced to two fields, untested | Med | code | ✅ |
| **F9** | Expression rendering recurses unbounded; ceiling is 5,000 on a 2 MiB stack | Med | [measured] | ✅ |
| **F10** | Debug-info absence, unmodelled version, and parse failure are indistinguishable | Med | code | ✅ |
| **F11** | In-band error placeholders (`<string:N>`, `<invalid utf8>`) are unmarked | Low | code | ✅ |
| **F12** | The `bytecode` section entry runs to EOF, absorbing three other regions | Low | [measured] | ✅ |
| **F13** | Cache temp file races; 134 MB written next to a 16 MB input | Low | [measured] | ✅ |
| **F14** | `bigint_at` returns raw hex, not a value, above 64 bits | Low | code | ✅ |

Two worries were **retired** by measurement rather than confirmed — see *What is fine*.

### The shape of the fix

Eleven of the fourteen were the same defect wearing different clothes: a real
difference collapsing onto a value that looks like success. They are fixed the same
way — by a `Diagnostic` recorded on `BytecodeFile` rather than by making each
individual site more careful:

```rust
pub enum Diagnostic {
    FooterMismatch,                                    // F6
    LengthMismatch { declared, actual },               // F6
    LayoutFallback { version, used, implied },         // F1
    OpcodeTableSubstituted { declared, used },         // F2
    InvalidStringStorage(usize),                       // F11
    DebugInfoUnreadable(DebugInfoStatus),              // F10
}
```

plus `BytecodeFile::debug_info_status` (F10), a shared `unresolved_string_ids`
counter for the lazily-read literal buffers (F11), and `warnings()` / `is_clean()`
to read them back. Nothing fails a parse that did not fail before — reading a
deliberately broken image is still a supported thing to do. The CLI prints them to
stderr ahead of its output; MCP `load_file` returns them inline.

---

## F1 — `parse_auto` can return the layout the version says it is not

> **Fixed.** `parse_auto` now tries the version-implied layout first and returns it
> if it parses; the other layout is attempted only as a fallback and carries a
> `LayoutFallback` diagnostic when it is used. When neither parses, the error quotes
> *both* failures instead of the old fixed string. Held by
> `read_diagnostics.rs::a_layout_contradicting_the_version_is_always_reported`, which
> re-runs the 4,000-flip sweep and asserts every wrong-layout parse is reported
> (measured after: 95 and 76 cases respectively, **0** silent), plus
> `the_version_decides_the_layout` and `an_unparseable_file_names_both_layouts`.


`parsing.rs:66-83`. Both layouts are attempted, both errors are dropped with `.ok()`, and the
result is chosen by this table:

```rust
(Some(file), None)                     => Ok(file),          // <-- version is not consulted
(None, Some(file))                     => Ok(file),          // <-- version is not consulted
(Some(legacy_file), Some(modern_file)) => if version >= 97 { modern } else { legacy },
(None, None)                           => Err("failed to parse bytecode file using known layouts"),
```

The version only breaks ties. When exactly one layout parses, that one is returned **even
when the declared version says it must be the other**.

**[measured]** — 4,000 single-bit flips per fixture, never touching magic or the version field:

| fixture | clean: legacy / modern | flips parsing **only under the wrong layout** |
|---|---|---|
| `plain.v96.hbc` | yes / no | 0 / 4000 |
| `handlers.v96.hbc` | yes / no | 0 / 4000 |
| `plain.v98.hbc` | yes / yes | **95 / 4000 (2.4%)** |
| `plain.v99.hbc` | yes / yes | **76 / 4000 (1.9%)** |

A concrete case, one flipped bit at byte 105 of `plain.v99.hbc`:

```
parse_auto on a v99 file returned  layout=Legacy fn_layout=Legacy16  functions=3 strings=9
clean baseline:                    layout=Modern fn_layout=Modern12  functions=3 strings=9
```

Three functions, nine strings, no error — and every field decoded at the wrong stride.

Note the first column of that table as well: **a clean v98/v99 file parses successfully under
the Legacy layout too.** "It parsed" carries no information on modern files; the version
tie-break is doing all the work.

This is not theoretical for this repo. The workflow here is hand-patching bundles, and a
hand-patch that leaves one section size slightly off is precisely the input that flips a
modern file into a legacy parse.

**Fix.** Try the version-implied layout first and return it if it parses. Fall back to the
other layout only as a deliberate, *reported* recovery — `parse_auto` should hand back a
"layout was not the one the version implies" signal that the CLI prints as a warning and the
MCP surfaces in `load_file`'s response. And keep both underlying errors: today a genuinely
bad file always reports the same generic string, with both real diagnoses discarded at
`.ok()`.

## F2 — opcode-table substitution is invisible on two of three entry points

> **Fixed.** Added `BytecodeFile::resolve_format`, which resolves the table and
> records `OpcodeTableSubstituted` when a different version's is used. `Decompiler::new`
> and MCP `load_file` both route through it; the CLI keeps its stderr warning, now
> spelling out the consequence rather than just the substitution.


`for_version_or_latest` (`opcode.rs:186`) falls back to the nearest table `<= version` and
returns `(format, used_version)` so the caller can tell. Three callers:

| caller | handles it |
|---|---|
| `hbc-decomp-cli/src/helpers.rs:128` | yes — warns to stderr |
| `hbc-decomp-mcp/.../tools_analyze.rs:28` (`load_file`) | no — `let (format, _)` |
| `hbc-decomp/src/pipeline/decompiler.rs:21` (`Decompiler::new`) | no — `let (format, _)` |

The two silent ones are the library API and the agent-facing API. Given the v99 opcode drift
recorded in the write guide — eight phantom opcodes shifting twelve later ones, so `===`
decoded as `>=` — this is the highest-consequence silence in the crate: the output is
syntactically perfect JavaScript with inverted comparisons.

**Fix.** `load_file`'s response text already carries version and counts; append the
substitution when `used_version != version`. For `Decompiler::new`, either expose the used
version or add a `Decompiler::new_strict`.

## F3 — the MCP surface has no output bound

> **Fixed.** Three layers. Every tool response goes through `text_result`, capped at
> `MAX_RESPONSE_BYTES` (256 KiB) with an explicit tail naming the real size and telling
> the caller to narrow the request — truncation is never silent. `dump` and
> `xref_search` take `limit`/`offset` and state the window they returned.
> `decompile_all` *refuses* above 2,000 functions rather than truncating, pointing at
> `list_modules` + `decompile_module`: capping 41 MB at 256 KiB would return 0.6% of the
> answer while looking like it worked. Verified end-to-end over stdio against the real
> bundle — `dump kind=strings` came back capped with `this response was 5382496 bytes`,
> and `decompile_all` refused with the pointer. Held by four `cap_text` tests including
> the multibyte-boundary case.


**[measured]** on the Equinox bundle:

| tool | output |
|---|---|
| `decompile_all` | **41,447,553 bytes** (17 s) |
| `dump --kind strings` | 5,839,550 bytes |
| `dump --kind functions` | 3,717,753 bytes |
| `callgraph` (no root) | 303,412 bytes (14 s — cost is in `analyze_module`, not the string) |

Of the 21 tools in `tools_analyze.rs`, exactly **one** (`list_modules`) takes a `limit`, and
one (`dead_code`) hardcodes `take(200)`. `decompile_all`, `dump`, `dump_table`, `xref_search`,
`disassemble` and `callgraph` are all unbounded, and each returns a single
`ContentBlock::text`.

41 MB into an agent's context is not a degraded result, it is a failed call — and an expensive
one. `render_call_graph` with `root: None` also builds the whole edge list into one `String`
before anything can truncate it.

**Fix.** `limit`/`offset` on every listing tool, a default cap (a few hundred KB) with an
explicit `"… truncated, N of M shown, pass offset=N"` tail, and a hard refusal on
`decompile_all` for bundles above some function count, pointing at `decompile_module`.

## F4 — one panic bricks the MCP server permanently

> **Fixed.** `HermesService::lock` recovers from poisoning via `into_inner()` (the
> data behind the lock is a parsed file plus a memoised context — a panic mid-read
> cannot leave it half-updated), and every tool body runs inside `catch_tool_panic`,
> which turns a panic into one failed call carrying the panic message and a note that
> the session survived. Held by `a_panic_does_not_brick_the_service`, which genuinely
> poisons the mutex and then asserts a normal tool call still returns its normal
> error.


`server/mod.rs:29` — `loaded: Mutex<Option<LoadedFile>>`, and every tool goes through
`with_file` / `with_file_mut`, which map a lock failure to an error. `std::sync::Mutex`
**poisons** on a panic while held. So any panic inside any tool body — F7's overflow, an
unforeseen index, a future regression — does not merely fail that call: it makes
`self.loaded.lock()` return `Err` for the rest of the process, and every subsequent tool
returns `lock: poisoned`. The server stays up, answers nothing, and gives no hint that a
restart is the fix.

There is no `catch_unwind` anywhere in either binary (the sole one in the tree is in the
TUI's git-diff view, `tui/gitdiff.rs:249`).

**Fix.** Recover from poisoning (`.unwrap_or_else(|e| e.into_inner())`) — the invariant being
protected is "a parsed file", which a panic mid-read does not corrupt — and wrap tool bodies
in `catch_unwind` so a panic becomes one failed call with a diagnosable message.

A related note: `pipeline_ctx.as_ref().unwrap()` at `tools_analyze.rs:115, 228, 258, 560, 586`
is locally sound (each is preceded by `ensure_pipeline()?`), but it is five unwraps standing
on a call-order convention. A `let … else { return Err(…) }` costs nothing.

## F5 — legacy large headers never reinstate `FLAG_OVERFLOWED`

> **Fixed.** One line in `parse_large_header_legacy`, mirroring the modern path.
> Measured after: `raw small-header Overflowed = 15, is_overflowed() = 15`. Held by
> `overflowed_legacy_headers_report_themselves_as_overflowed`, which checks the count
> against the raw table and separately asserts every function with `frame_size > 127`
> is flagged. Confirmed not to leak into written images: `serialize_file` is pure
> identity from `raw_bytes`, and the patch ops read the overflow bit from the on-disk
> bytes (`rebuilt[slot + flag_byte]`), never from the parsed struct — the corpus
> round-trip and `identity_serialize_v96` both still pass.


Upstream's `SmallFuncHeader(uint32_t largeHeaderOffset)` zeroes the small header and sets only
`Overflowed`, so the **large** header on disk never carries that bit. The modern parser knows
this and puts it back (`function.rs:246`, with a good comment explaining why). The legacy
parser does not — `function.rs:199` reads `flags` straight through.

**[measured]**, on the shipped Equinox v96 bundle:

```
functions                       : 62909
small headers with Overflowed   : 15      <- ground truth, read from the raw table
is_overflowed() reports         : 0
frame_size > 127                : 6       <- impossible in a 7-bit small-header field
example large-header fn 8673: frame_size=180 flags=0x02 is_overflowed=false
```

Function 8673 provably came from a large header (frame_size 180 cannot fit the small header's
7 bits) and `is_overflowed()` says no.

Consequences today are cosmetic — `inspect.rs:367` never prints `overflowed` for a legacy
bundle. The consequence tomorrow is not: `write/serialize.rs:421`, `has_overflowed_functions`,
returns **false** for this bundle. It is currently uncalled, so it is a correct-looking guard
sitting in the write path waiting for the first op that gates on it. Legacy is the v96 case,
which is every bundle this repo actually patches.

**Fix.** One line, mirroring `function.rs:246`:

```rust
flags: reader.read_u8()? | crate::format::FLAG_OVERFLOWED,
```

plus a test asserting the `is_overflowed()` count equals the raw-table count on a corpus
bundle. The write guide's own lesson applies: assert against an independent derivation, do
not hand-sync.

## F6 — no integrity check on read

> **Fixed.** `parse_with_layout` now compares `header.file_length` against the real
> byte count and verifies the trailing SHA-1, recording `LengthMismatch` /
> `FooterMismatch`. Neither fails the parse. Cost: parsing the 16 MB bundle went from
> 17.7 ms to 22.6 ms — the SHA-1 adds ~5 ms, partly offset by F1 removing the wasted
> second parse attempt. Verified on a deliberately stale-footered and on a truncated
> copy of the real bundle; held by `a_stale_footer_is_reported` and
> `a_length_mismatch_is_reported`.


`verify_footer` exists (`write/footer.rs:33`) and is exercised by write-path tests and the
`asm-check` command (`write_cmd.rs:433`). **No read command calls it.** `header.file_length` is
parsed into the struct (`header.rs:38, 115`) and never compared to `bytes.len()`.

**[measured]** — both invariants hold exactly on the real bundles, so both are meaningful:

```
backup   len=16837408  header.file_length=16837408  delta=0  footer_ok=True
patched  len=16837408  header.file_length=16837408  delta=0  footer_ok=True
```

This repo's whole bundle workflow ends in "refresh the trailing SHA-1 footer", and the project
CLAUDE.md carries a standing warning that the usual tool for that is broken. A read-side check
turns "I forgot to rehash" from a device-side crash into a line of stderr — and it is two
comparisons.

**Fix.** In `parse_with_layout`, record `footer_valid: bool` and `length_matches: bool` on the
parsed file (never fail the parse — reading a deliberately broken image is a legitimate use).
Print a warning from the CLI and include both in MCP `load_file` / `file_info`.

## F7 — the one reachable panic

> **Fixed.** `saturating_add`. The sweep is now a committed test,
> `tests/read_robustness.rs`, run in debug so integer overflow is caught (release wraps
> silently, which is how this hid). Re-run after the fix at the original scale —
> `HBC_FUZZ_FLIPS=20000`, 260,000 mutants — **zero panics**, where it previously fired
> 7 times. The committed default is 750 flips per fixture so `cargo test` stays ~20 s.


`inspect.rs:341`:

```rust
h.number_reg_count + h.non_ptr_reg_count,
```

In the *small* modern header these are 5-bit fields and cannot overflow. In the **large**
modern header both are full `u32` read from the file, so the sum is file-controlled. Debug
builds panic (`attempt to add with overflow`); release builds wrap and print a wrong count.

**[measured]** — this was the only distinct panic across 260,000 mutants (13 fixtures ×
20,000 single-bit flips, plus truncations at 1/2…1/128, plus every 4-byte header field
smashed to `u32::MAX` / `0x7fffffff` / `0xffff0000` / `1<<24`), covering `parse_auto` →
`decode_function_instructions` → `disassemble_function` → all eight `dump_table` kinds and
their JSON forms → `function_info_banner` → `render_call_graph` → key/value/array buffer
series → `bigint_at`. A separate release + `overflow-checks` pass extended the same sweep
through `Decompiler::decompile_function`, `build_metro_registry` and `scan_secrets`: also
clean.

**Fix.** `saturating_add`. And keep the sweep — see the appendix.

## F8 — the cache key is hand-synced

> **Fixed.** `options_key` now hashes the whole `DecompileOptionsV2` (which gained
> `Hash`), so a new field cannot desync the key from what `build_with_options` reads.
> Held by `every_option_field_changes_the_cache_key`, which flips each field in turn
> and asserts the key moves — and destructures the struct exhaustively, so adding a
> field without adding it to the test stops compiling.


`pipeline/cache.rs:66`:

```rust
fn options_key(options: &DecompileOptionsV2) -> u32 {
    (options.assembly_mode as u32) | ((options.include_offsets as u32) << 1)
}
```

This is correct **today**: `build_with_options` (`pipeline/context/mod.rs:60-61`) reads exactly
those two fields and forces the rest to `optimized()`. Nothing enforces it. Add a seventh field
to `DecompileOptionsV2`, consume it in `build_with_options`, and every cache hit silently
returns a context built with the old value — with the file hash and binary fingerprint both
matching, so the cache looks perfectly valid.

The rest of the cache design is careful (SHA-256 of the bytes, a build.rs fingerprint that
auto-invalidates on any rebuild, temp-file-then-rename). This one field is the exception, and
it is the same "partly-stale model, hand-synced" shape that the write guide's `commit_image`
harness found in **every** write op.

**Fix.** Derive the key from the whole struct — `#[derive(Hash)]` plus a `DefaultHasher`, or
serialize it. Over-invalidation costs one rebuild; under-invalidation costs a wrong answer
that looks right.

## F9 — recursion is bounded by stack size, not by a depth check

> **Fixed.** New `ir::depth` module: a thread-local RAII `DepthGuard` with
> `MAX_RENDER_DEPTH = 512`, applied at the three recursive renderers — `format_expr`
> (every other formatter in that file routes through it), `Codegen::generate_expr`, and
> `Codegen::generate_statements` for block nesting. Past the bound they emit
> `/* hbc-decomp: nesting exceeds MAX_RENDER_DEPTH */` rather than descending:
> greppable, syntactically inert, and not silent. 512 is ~6x the deepest expression
> measured in a real bundle (79) and ~10x under the 2 MiB stack ceiling (~5,000).
> `hermes-mcp`'s `main` now calls `configure_thread_pool()` at startup, so the 64 MB
> pool is configured before anything can initialise Rayon lazily — closing the
> cache-hit hole. Held by `deep_expression_renders_instead_of_overflowing_the_stack`,
> which renders a 50,000-deep tree on a deliberately 2 MiB thread. The `Drop`-recursion
> caveat is unchanged and documented in the module: a guard in a renderer cannot help
> with it, which is the other half of why the stack matters.


`lib.rs:38-52` configures a 64 MB Rayon stack with a comment saying the default 2 MB
"overflows and aborts the process on large real-world bundles". That is the mitigation: a
bigger stack, applied to one thread pool.

**[measured]** — nesting `Expression::Binary` and calling `Display`, on a 2 MiB thread:

```
depth   1000: rendered ok (5002 chars)
depth   5000: STATUS_STACK_OVERFLOW   (exit code 0xc00000fd)
```

≈400 bytes of stack per level. A stack overflow is an **abort**, not a panic: no
`catch_unwind`, no error, the process dies — which for the MCP server means the client loses
the session with no message.

**[measured]** — the real headroom, over all 62,018 IR functions of the Equinox bundle:

```
max expression depth: 79 (function 61510)
histogram <10 / <50 / <200 / <1000 / <5000 / >=5000:
          61794 / 222 / 2 / 0 / 0 / 0
```

So: **not a live problem for real React Native bundles** (60× headroom), and trivially
reachable on a crafted or generated one. This is an RE tool pointed at unknown APKs, so
"crafted input" is the job description, not an edge case.

Two related notes. `configure_thread_pool()` is called from `main.rs:48` (CLI) and from
`build_with_options` (`context/mod.rs:56`) — but **not** from `hermes-mcp`'s `main`, and
`build_cached` returns early on a cache hit *before* reaching it. Today nothing breaks,
because the rayon work in `rendering.rs:102,157` is only reached via paths that also build the
pipeline. It is an invariant with no assertion and no test, one refactor away from being false.
And `Drop` of a deeply nested `Box<Expression>` chain is itself recursive, so a depth guard in
`Display` alone is not sufficient.

**Fix.** A depth counter in the expression/statement walkers that emits
`/* expression too deeply nested */` past a limit; explicitly configure a large stack in
`hermes-mcp`'s `main`, or spawn tool work onto a thread with one.

## F10 — every debug-info failure looks like "no debug info"

> **Fixed.** `DebugInfo::parse_with_status` returns a `DebugInfoStatus`
> (`Present` / `Absent` / `OffsetOutOfRange` / `UnsupportedVersion(v)` /
> `HeaderOutOfRange` / `ParseFailed`), stored on `BytecodeFile::debug_info_status`;
> `is_failure()` separates "the file has none" from "we could not read what is there",
> and only the latter becomes a diagnostic. `try_parse_debug_info` no longer relabels an
> error as absence. MCP `debug_info` and `load_file` both report it. Held by
> `debug_info_absence_carries_a_reason`.


`DebugInfo::parse` returns `Ok(Self::default())` for **five** distinct conditions
(`debug.rs:203, 208, 215, 232`, plus the header-past-EOF case), and `try_parse_debug_info`
(`debug.rs:599`) then flattens any residual error to `None` with `.ok()`. Indistinguishable to
every caller:

1. the file genuinely has no debug info (`debug_info_offset` is 0 or `NO_OFFSET`)
2. the offset points past EOF (corrupt)
3. the version is unmodelled — **v97, and everything ≤ 95, and ≥ 100**
4. the header's own offsets point past EOF (corrupt)
5. the streams failed to parse

Case 3 deserves emphasis: the crate advertises HBC 40–99, and `DebugLayout::for_version`
(`debug.rs:128-146`) answers for `<= 96` and `98 | 99` only. Every v97 file, and every file
below v96, reports "no debug info" — correctly *refusing to guess*, which is the right call
and well-commented, but reported as absence rather than as a limitation.

**Credit where due:** DI3 in `UNMODELED_REGIONS_PLAN.md` ("the header parser is version-blind")
is **fixed** — `parse` now takes a version and `parse_header` branches on `DebugLayout`. That
plan document is stale on this point and should be updated.

**Fix.** Return the reason. An enum, or a `debug_info_status: &'static str` on `BytecodeFile`,
surfaced by `hermes-decomp debug` and MCP `debug_info` — the latter currently reports only
`offset == 0 || offset == u32::MAX` (`tools_analyze.rs:456`) and calls everything else empty.

## F11 — error placeholders are in-band and unmarked

> **Fixed.** `decode_string_table` returns the count of entries that decoded to an
> `<invalid utf8>` / `<invalid utf16>` placeholder (becoming `InvalidStringStorage`),
> and the literal-buffer reader increments a shared `unresolved_string_ids` counter on
> every `<string:N>`. That counter lives on the file behind an `Arc<AtomicUsize>`
> because those buffers are read lazily from the IR builder, long after the parse
> returns, so a parse-time total was not possible. Measured on the real bundle after
> decompiling all 62,909 functions: **0**. Held by
> `unresolved_literal_string_ids_are_counted`.


Four sites substitute a plausible-looking string for a failure, inside the value itself:

| site | placeholder |
|---|---|
| `buffer.rs:96, 106, 116` | `<string:412>` — string id out of range |
| `table.rs:171` | `<invalid utf16>` |
| `table.rs:180` | `<invalid utf8>` |

These flow into decompiled output, `dump --kind strings`, `xref` results and secret scanning
as if they were content. The `parse_legacy_buffers` comment records the precedent: getting the
section order wrong once produced **~93,000** `<string:N>` placeholders on a Discord HBC96
bundle — a signal that was there to be counted, and was not.

**Fix.** Count them during the parse and expose the total. A non-zero unresolved-string count
is the single best "this file was decoded wrong" indicator the crate has, and it costs a
counter.

Two adjacent leniencies in the same area, both deliberate and both worth a counter for the
same reason: `read_buffer_series` skips zero-length tags rather than aborting
(`buffer.rs:42-46`), and `decode_string_table` clamps `string_count` to the actual small-entry
table (`table.rs:114`).

## F12 — the `bytecode` section entry is really "everything left"

> **Fixed.** The tail is split into `bytecode` / `function_info` / `debug_info` /
> `footer`, bounded by the last function body's end, `debug_info_offset` and the fixed
> footer length. On the real bundle `dump --kind sections` now ends:
> 
> ```
> bytecode               0x63b56c     10266842
> function_info          0x1005e46    36010
> debug_info             0x100eaf0    28
> footer                 0x100eb0c    20
> ```
> 
> which sums to the 10,302,900 the single entry used to claim.


`parsing.rs:300` — `let instructions = bytes[instruction_offset..].to_vec();` — and the matching
`SectionInfo` runs to EOF.

**[measured]** on the Equinox bundle: `instruction_offset` = 6,534,508, the reported `bytecode`
size is 10,302,900, and `debug_info_offset` = 16,837,360 — 48 bytes before EOF. So that one
entry spans the bytecode **plus** the per-function info areas (exception tables,
`DebugOffsets`) **plus** the debug-info section **plus** the 20-byte footer. On this stripped
release build the overstatement is small; on a `-g3` build it is megabytes.

`dump --kind sections` is a primary orientation tool for exactly the offset arithmetic this
repo does by hand. It should not report one section where there are four.

**Fix.** Bound `bytecode` at `debug_info_offset` where that is set, and emit `function_info`,
`debug_info` and `footer` as their own entries. `UNMODELED_REGIONS_PLAN.md` already has the
layouts.

## F13 — cache hygiene

> **Fixed** (the race). The temp file is now `...hdcache.<pid>.tmp`, so concurrent
> processes cannot interleave into one another's write. The 134 MB size and the
> unauthenticated-cache note are documentation items rather than defects — recorded
> here rather than changed.


- **Temp-file race.** `cache.rs:306` — `path.with_extension("hdcache.tmp")` is a fixed name.
  Two processes analysing the same bundle write the same temp file concurrently; the rename is
  atomic but the *content* is interleaved. It degrades to a cache miss (`try_load`'s
  `rmp_serde…ok()?`), never to a wrong answer, but it leaves a corrupt file in place until
  something rewrites it. A PID/random suffix fixes it.
- **Size.** **[measured]** the `.hdcache` for the 16,837,408-byte Equinox bundle is
  **134,208,814 bytes** — 8× the input, written silently next to it, with no eviction and no
  mention in the docs. Worth stating in `USAGE.md` at minimum.
- **Trust.** The cache is unauthenticated MessagePack whose header check requires only the
  file hash and the build fingerprint — both derivable by anyone who can write next to the
  input. It deserializes into plain data (no code), so the ceiling is falsified analysis
  output, not execution. Low risk for a local tool; worth one sentence in the doc rather than
  a fix.

## F14 — `bigint_at` above 64 bits is not a value

> **Fixed.** Added `twos_complement_le_to_decimal`: extract the sign, negate via
> invert-and-add-one, then convert with schoolbook base-1e9 long division — no bignum
> dependency for one call site. The `<= 8` byte path is unchanged; a unit test asserts
> the two branches agree across the values where they overlap (`0`, `+/-1`, `+/-255`,
> `i32::MAX`, `i64::MIN`, `i64::MAX`) so they cannot drift apart, plus wide cases
> including `-(2^64)`, which previously printed as an unsigned hex blob.


`parser/helpers.rs:44-46`: BigInts longer than 8 bytes are rendered as reversed raw hex with no
sign handling, so a large negative BigInt prints as an unsigned hex blob. Not wrong so much as
not implemented; the `<= 8` path is correct including sign extension. Worth a `// TODO` and a
mention in `LIBRARY.md`'s limitations list, since `dump --kind big-int` presents it as a value.

---

## What is fine — two worries retired by measurement

Reporting these matters as much as the findings; both looked like real problems on a read.

**`parse_auto` does not double the parse cost.** It runs both layouts and drops one. On a
legacy file the modern attempt fails in **2.8 µs** against a 17.7 ms full parse **[measured]**,
because the modern header's field mapping puts an impossible size in an early field. The
concern is real only for modern files, where both layouts do run to completion — but modern
files are not what this repo handles, and the absolute cost (≈20 ms on 16 MB) does not justify
restructuring. F1 is a correctness argument for changing `parse_auto`, not a performance one.

**Exception handlers are in bounds.** `parse_exception_handlers` (`parsing.rs:398`) never
validates `start`/`end`/`target` against the function body, which reads like an obvious gap.
**[measured]** on the Equinox bundle: 1,544 functions carry 2,438 handlers; **0** have
`start > end`, **0** fall outside their own body (the offsets are function-relative), and **0**
are recorded for a function lacking `FLAG_HAS_EXCEPTION_HANDLER`. The `count > 1000` sanity
bound plus the reader's own bounds checks are carrying it. A corrupt table still yields a wrong
CFG rather than an error — the fix is a validation counter, not a bounds check — but nothing
here is currently broken.

Also solid, and worth not re-litigating:

- `io.rs` end to end — `capacity_hint` clamping allocations to bytes remaining,
  `checked_add` in `read_exact`, LEB128 shift bounds, `align` erroring on 0.
- `decode_string_table`'s saturating offset/length math and its `string_count` clamp.
- `decode_function_instructions`' `checked_sub` / `checked_add` and explicit range check.
- The fixed-point iteration caps across the analysis layer (`MAX_ASYNC_PROPAGATION_ITERATIONS`,
  `MAX_PARAM_LINK_ITERATIONS`, `MAX_MODULE_NAME_ITERATIONS`, `MAX_REEXPORT_ITERATIONS`,
  `MAX_PARENT_CHAIN_DEPTH`, `MAX_WRAPPER_CHAIN_DEPTH`, `MAX_INLINE_BODY_PASSES`) — the
  analysis layer *is* bounded; it is only the IR tree walk (F9) that is not.
- The CLI's `warn_layout_mismatch` (`helpers.rs:42`), which is exactly the diagnostic F1 asks
  for, applied to the manual-override case. F1 is asking to extend it to the automatic one.

---

## Order the fixes were applied

Cheap and high-value first; the first four were small, local edits.

1. **F7** `saturating_add` — one line, removes the only known panic.
2. **F5** reinstate `FLAG_OVERFLOWED` on legacy large headers — one line, plus a corpus assert.
3. **F6** footer + `file_length` check recorded on the parsed file, warned by the CLI.
4. **F2** surface the opcode-table substitution in MCP `load_file` and `Decompiler`.
5. **F8** derive `options_key` from the whole options struct.
6. **F1** make `parse_auto` version-first and report a fallback.
7. **F3** `limit`/`offset` + default caps on the MCP listing tools.
8. **F4** poison recovery + `catch_unwind` in the MCP tool handlers.
9. **F10 / F11** a `status` on debug info and an unresolved-string counter — the two cheapest
   "this decode is wrong" signals available.
10. **F9** depth guard, and an explicit stack for `hermes-mcp`.
11. **F12 / F13 / F14** section table split, cache temp suffix, BigInt note.

A structural suggestion cutting across F1/F2/F6/F10/F11: give `BytecodeFile` a
`diagnostics: Vec<Diagnostic>` populated during the parse — footer mismatch, length mismatch,
layout fallback, opcode-table substitution, unresolved-string count, debug-info status. One
field turns six silent degradations into one thing the CLI prints and the MCP returns, and it
is additive rather than a change to any existing signature.

---

## Appendix — harnesses

A1 and A2 are now committed as `tests/read_robustness.rs` and
`tests/read_diagnostics.rs`; A3 lives in `src/ir/depth.rs`'s test module. The sketches
below are kept for the reasoning behind each — what it is for, and how to run it at a
scale beyond the committed default.

### A1 — corruption sweep (found F7; established the 260k-mutant baseline)

Per fixture: truncations at 1/2…1/128, N single-bit flips from a seeded xorshift, and every
4-byte header field smashed to four hostile values. Each mutant runs the full read surface
under `catch_unwind`, and panics are deduplicated by message.

```rust
fn probe(name: &str, bytes: &[u8], report: &mut Vec<String>) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let file = hbc_decomp::BytecodeFile::parse_auto(bytes)?;
        let fmt = hbc_decomp::BytecodeFormat::for_version(file.header.version)?;
        let n = file.function_headers.len().min(64);
        for i in 0..n {
            let _ = file.decode_function_instructions(&fmt, i as u32);
            let _ = hbc_decomp::disassemble_function(
                &file, &fmt, i as u32, &hbc_decomp::DisasmOptions::default());
        }
        for k in [/* all eight TableKind variants */] {
            let _ = hbc_decomp::dump_table(&file, k);
            let _ = hbc_decomp::dump_table_json(&file, k);
        }
        let _ = hbc_decomp::function_info_banner(&file, 0);
        let _ = hbc_decomp::render_call_graph(&file, &fmt, Some(0), 3, false);
        for sh in file.obj_shape_table.iter().take(64) {
            let _ = file.read_key_buffer_series(sh.key_buffer_offset, sh.num_props);
        }
        for i in 0..file.big_int_table.len().min(64) { let _ = file.bigint_at(i as u32); }
        for off in [0u32, 1, 2, 4, 8] {
            let _ = file.read_array_buffer_series(off, 16);
            let _ = file.read_value_buffer_series(off, 16);
        }
        Ok::<_, hbc_decomp::Error>(())
    }));
    if let Err(e) = r { /* downcast to String, push onto report */ }
}
```

Run it two ways. **Debug** catches integer overflow (this is how F7 surfaced) and does 20,000
flips × 13 fixtures in ~6 minutes. **Release with `RUSTFLAGS="-C overflow-checks=on"`** keeps
overflow detection at release speed, which is what makes it affordable to extend the body with
`Decompiler::decompile_function`, `build_metro_registry` and `scan_secrets`.

This belongs in CI. It is the assertion that the read path's robustness is a property and not
an accident, and it is the harness that catches the next F7 before a user does.

### A2 — layout disagreement (F1)

For each fixture, flip one bit anywhere **except** bytes 0..12 (magic and version, so the
declared version stays fixed), then classify `(right_layout_parses, wrong_layout_parses)`.
`WRONG_ONLY` is the count where `parse_auto` returns a layout the version contradicts.

### A3 — expression depth ceiling (F9)

Nest `Expression::Binary` n deep, render it with `Display` on a thread created with
`.stack_size(2 * 1024 * 1024)`, and walk n upward until the process dies. Pair it with a pass
over `PipelineContext::all_ir` on a real bundle to measure actual headroom — the ceiling alone
is alarming; the ceiling next to "max observed 79" is a decision.

### Reproducing the measurements

The bundle-backed numbers used:

```
C:\apks\equinox\com.equinoxfitness.equinox_11.39.0\hermes_bundle\assets\index.android.bundle.backup
```

passed via a `BENCH_HBC` env var so the tests skip cleanly when it is absent.
