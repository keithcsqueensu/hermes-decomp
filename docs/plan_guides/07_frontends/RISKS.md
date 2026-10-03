# Frontends — risk register

> **Ownership.** *Owns* the risk that the CLI/MCP surfaces mislead their consumer — a human
> patching by hand or an agent reading tool output as ground truth. That splits in two by
> surface. **The MCP surface:** two findings, **F3** (no output bound — `decompile_all`
> returned 41 MB) and **F4** (one panic poisoned the mutex and bricked the server for its
> lifetime), split here from the read-path hardening review because both live in
> `hbc-decomp-mcp`. **The CLI output surface:** the stdout/stderr two-channel contract and the
> four CLI-surface risks **R17** (no CLI/integration coverage), **R18** (stderr has no formal
> log levels), **R20** (a CLI note pointed at a script that does not exist) and **R22** (an
> unoptimized CLI build overflows its stack) — all relocated here from `../06_write/RISKS.md`,
> because they are about how the `hbc-decomp` CLI *presents* a write rather than about the
> mutation itself. *Delegates* the write *mechanics* those risks sit on — the ops, invariants
> and the rest of the R-series — back to `../06_write/RISKS.md`, and the frontends'
> *description* (command surface, TUI, MCP tool list) to `../../arch_guides/07_FRONTENDS.md`;
> the upstream framing of the F-series stays in `../01_read/RISKS.md`. Finding numbers are shared
> across the stage registers and indexed in `../README.md`; F3, F4, R17, R18, R20 and R22 keep
> theirs — an `R#` names one hazard wherever it is hosted.

Status: ✅ fixed (F3, F4). Evidence tag **[measured]** means reproduced against the shipped
Equinox v96 bundle over a real stdio MCP session (see `../01_read/RISKS.md` for the bundle
identity). The CLI-surface risks below carry their own residual.

---

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
**poisons** on a panic while held. So any panic inside any tool body — F7's overflow (now in
`../01_read/RISKS.md`), an
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

---

## CLI output surface

Relocated from `../06_write/RISKS.md`: the `hbc-decomp` CLI's presentation contract and the four
risks about it. These are *surface* concerns — how a write is reported to a human or a script —
not mutation concerns; the write *mechanics* they attach to stay in the write register and are
referenced by `R#`. The risks keep their write-register numbers (R17, R18, R20, R22); nothing is
renumbered by the move.

### CLI-surface risk rows

Same register scheme as `../06_write/RISKS.md`: `Inherent` = likelihood × impact *before*
mitigation, `Residual` = the risk that remains today (🟥 high · 🟧 medium · 🟩 low · ⬜ resolved).
`Mitigation` is what is already true in the tree; **`Hardening` is the single home for the todo**,
with any open decision inline. Sort by `Residual` for priority. The `§` column is the op/area the
risk touches, carried over verbatim from the write register.

| R# | Hazard | § | Inherent | Residual | Mitigation (in tree) | Hardening (todo + open decision) |
|---|---|---|---|---|---|---|
| R17 | No CLI / integration coverage | all | M×M | 🟧 | `hbc-decomp-cli/tests/stdout_contract.rs` covers the stdout/stderr contract and the exit-code path across six commands, and needed the debug-stack fix (F9) to be possible at all | Extend beyond the stdout contract to argument resolution: `--at` vs `--function`+`--insn-offset` precedence, `--string` vs `--string-id`, `--from`/`--to` value→id lookup. Those are still untested. |
| R18 | stderr has no formal log levels — ad-hoc `warning:`/`note:`/plain prefixes | cli | L×M | 🟩 | two-channel split is honored (data→stdout, diagnostics→stderr); ERROR is the `Result`/exit path; implicit severity via wording | Formalize the INFO/WARN prefixes (a tiny `eprintln`-wrapping helper, no external crate — keeps the pure-Rust ethos); keep ERROR on the `Result`/exit path, not a stderr line. **Decision:** local 2-line helper vs a `log`/`tracing` dep — recommend the local helper. See Stdout/stderr discipline. |
| R20 | CLI points users at a verifier script that does not exist | cli | H×L | ⬜ | `warn_modern_write` now points at `scripts/build_hermes_vm.ps1` and `tests/vm_verify.rs`, both of which exist; docs/USAGE.md's "cannot be verified" section is rewritten around `hvm` | — (fixed). Note nothing *tests* stderr text, so this class can rot again; see R17/R18. |
| R22 | An unoptimized build of the CLI overflows its stack | cli | H×M | ⬜ **fixed** | `run` is one large match over every subcommand and a debug build gives each arm's locals their own slot in one frame, exceeding Windows' 1 MiB main-thread stack. Work now runs on a 64 MiB-stack thread (F9) | — (fixed). The underlying shape is unchanged: the match still holds every arm's locals at once, so splitting arms into functions is the real fix if the frame grows again. Note the release build was always fine, which is why this survived — *test what CI builds*. |

R17 and R18 are open (🟧 / 🟩); R20 and R22 are fixed (⬜). The write register's risk-grid no
longer plots these — it points here instead. The findings behind R20 and R22 are catalogued in
`../06_write/reference/HARNESSES_AND_HISTORY.md` (F9 is the debug-stack overflow; the harness
list records R20), which reference them by number.

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
of the library functions get no unsolicited stderr. (Q5 is a write-register design decision —
see `../06_write/RISKS.md` — reached here because it settled *who* owns this surface's output.)

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
finding F9 (`../06_write/reference/HARNESSES_AND_HISTORY.md`).

**Exit codes** are uniform: handlers return `Result`, errors bubble to `main` which returns
`Box<dyn Error>` → non-zero exit with the error Debug-printed. Keep new commands on this
path (no `process::exit`, no `unwrap`/`panic` on user input).

