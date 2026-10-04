# Write path reference — test harnesses and git history

> **What this is.** Background for `../RISKS.md`: what each test harness asserts against and how
> to run it, and what the committed git history reveals about the write path's bug classes. The
> `R#`/`Q#`/`I#`/`F#` identifiers are defined in `../RISKS.md`. The organising idea throughout is
> that every harness checks against something *outside* this crate — which is exactly why the
> unit suite could not see the defects these found.

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
| `tests/upstream_pin.rs` | **the upstream headers** — `FUNC_HEADER_FIELDS`, `BytecodeList.def`, the `BytecodeOptions` bits, the debug-info shapes | `HERMES_SRC_V96` / `_V97` / `_V98` / `_V99` | skips |
| `tests/corpus.rs` | **a production bundle**, plus `hbcdump` as a second disassembler | `HBC_CORPUS_BUNDLE`, `HBC_CORPUS_LIMIT`, `HERMES_HBCDUMP_V96` | skips |
| `hbc-decomp-cli/tests/stdout_contract.rs` | **the process boundary** — real stdout, stderr, exit codes | — | no |
| `commit_image` (`serialize.rs`) | **the bytes themselves** — the model is re-derived from them | — | no, always on |
| `tests/debug_info_guard.rs`, `tests/debug_relocation.rs` | **the fixture's own source lines** — `locations.debug.js`, built `-g3`; relocation asserts each location still names the instruction it named before | — | no |
| `corpus_fixture_present` tests (`write/mod.rs`) | **`hermesc` output** — compiled corpus files under `examples/react-native/v<N>/`, generated, not tracked | `HBC_CORPUS_OPTIONAL` | **fails** when absent, unless opted out |
| `hbc-decomp-cli/tests/e2e_corpus.rs` | **a child process + `node`'s parser** — every corpus file decompiles in its own process, exits 0 inside 60 s, and parses; plus the tracked `tests/fixtures/jsx_cycle_v*.hbc` | `HBC_CORPUS_OPTIONAL` | corpus half fails when absent, unless opted out; tracked half always runs |
| `scripts/build/roundtrip.sh` | **`node` running the original snippet** — its stdout against our decompiled output's | `roundtrip_known_failures.tsv`, exact in both directions | local gate (`gates.sh`), not CI |
| `scripts/build/syntax_check.mjs` | **`node`'s module parser** over a whole decompiled bundle | per-bundle ceiling in `syntax_baseline.tsv` | local gate; a bundle not on the machine is skipped |

The read-side suites — `read_robustness.rs`, `read_diagnostics.rs`, `debug_locations.rs`,
`bytecode_options.rs` — are catalogued where their findings live: `../../01_read/RISKS.md` and
`../../01_read/unmodeled_regions/PLAN.md` (P1, P5). The last four rows test the decompiler as
much as the write path; they are here because this is the only harness catalogue.

`scripts/build/gates.sh` runs every local gate in one command — `cargo fmt --check`, `cargo
clippy --workspace --all-targets -D warnings`, `cargo test --workspace`, a release build, the
round trip, then the parse check (`--quick` skips the last). The corpus those rows read is built
from the tracked snippets by `scripts/build/fetch_hermesc.sh` then `build_corpus.sh`
(`build_hermesc_from_source.sh` for HBC ~40–58, which have no prebuilt `hermesc`).

```powershell
# One-time: build the VMs (and the fixtures and hbcdump they need).
# Each lands in its own worktree beside the clone: C:\src\hermes-v96, -v98, -v99.
96, 98, 99 | ForEach-Object {
    ./scripts/build_hermes_vm.ps1 -Version $_ -HermesRepo C:\src\hermes-src -Fixtures
}

$env:HERMES_VM_V96    = 'C:\src\hermes-v96\build\bin\Release\hvm.exe'
$env:HERMES_SRC_V96   = 'C:\src\hermes-v96'
$env:HERMES_SRC_V97   = 'C:\src\hermes-v97'   # source only; v97 never shipped, so no VM
$env:HERMES_HBCDUMP_V96 = 'C:\src\hermes-v96\build\bin\Release\hbcdump.exe'
$env:HBC_CORPUS_BUNDLE = 'C:\apks\...\index.android.bundle.backup'
$env:HBC_CORPUS_LIMIT = '0'      # sweep all 62,909 functions (~9s); default 2000
$env:HBC_REQUIRE_ORACLES = 'all' # absent oracle => failure, not [skip]; all|src|vm|hbcdump|corpus
cargo test
```

⚠️ **"Gated? skips" was the live weakness (R21), and is now declarable.** With no env vars
set, those three suites still pass while asserting almost nothing — deliberate, because a
checkout without a Hermes build has to stay testable. What changed is that a run can now say
which oracles it refuses to do without: `HBC_REQUIRE_ORACLES=src,vm,hbcdump,corpus` (or `all`)
makes each absent one a failure naming the variable to set, and a variable that is *set* but
does not point at what it claims is an error in every mode. An unknown token in that list is
itself a failure — a typo that quietly enforced nothing would be this same defect again, in
the one place nobody would look.

CI (`.github/workflows/test.yml`) runs the suite unconfigured, then provisions the four
upstream checkouts with `scripts/fetch_pinned_hermes.py` — ~4 MB and a few seconds, because it
is a blobless sparse fetch by the sha each table records — and re-runs `upstream_pin` under
`HBC_REQUIRE_ORACLES=src`. `vm_verify` and `corpus` stay opt-in there. `.github/workflows/build.yml`
is the other half: fmt and clippy as a lint job, then `cargo test --workspace --release` on
Linux, macOS and Windows under `HBC_CORPUS_OPTIONAL=1`, because the generated corpus is never on a
runner.

⚠️ **`test.yml`'s unconfigured step does not set `HBC_CORPUS_OPTIONAL`**, so since
`corpus_fixture_present` started failing on a missing fixture (`197b243`) that step fails on every
runner — 13 write-module tests panic with "corpus fixture missing" (run 37163482195, `main`
@ `81c4e2a`) — and the strict `upstream_pin` step after it never runs. Its own name ("the
unconfigured path stays green") is the claim it falsifies.

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
  also one of the few harnesses that need no external artifact (the debug suites and
  `e2e_corpus`'s tracked half are the others), so it is one that will still be running in five
  years.
- **`commit_image`** is not a test but a structural guarantee, which is stronger: I1 cannot
  be violated because the model is no longer independently maintained.
- **The generated corpus** (`corpus_fixture_present`, `e2e_corpus`, `roundtrip.sh`) is the only
  thing that covers every HBC version a prebuilt `hermesc` exists for (~59 up) with real
  compiler output — but as the
  decompiler's oracle, not the write path's: of the write ops, only the in-module tests that read
  a corpus file touch it, and none of it runs a patched image. Its failure mode is the inverse of
  the env-gated suites': absent, it fails rather than skips, so a checkout without it has to say
  `HBC_CORPUS_OPTIONAL` out loud.

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

> **Update (upstream v0.2.4 merge and the gates, `b4f2797`..`45d97df`).** Behaviourally the
> write path did not move. Every `patch/*.rs`, `serialize.rs` and `header_write.rs` change in
> that range is `cargo fmt` reflow or a clippy fix (`45d97df` says "No behaviour change"); the
> one substantive write-side change is the HASM assembler (`hasm/parse.rs`: `b26ea89` stopped
> it eating the first letter of a mnemonic, `8e985fe` added its tests). What did change is the
> test surface — `corpus_fixture_present` turned silent skips into failures (`197b243`) — see
> Test harnesses.

### Findings

> **Two F-series.** F1–F10 below are this file's *git-history* findings, cited elsewhere as
> "history F3", "history F9" and so on. They are not the read-hardening findings F1–F14 shared by
> the read/ir/pipeline/frontends registers (`../../README.md` § The finding index). Neither series
> is renumbered.

- **F1 — `7fa1bfc` "Fix Copilot review findings from PR #3" (four bugs in one commit).**
  Areas: `patch-operand` (`operands.rs`), `retarget-string` (`strings.rs`), CLI
  (`write_cmd.rs`).
  1. `patch_string_operand` wrote a string id without checking `new_string_id <
     string_count` → could produce invalid bytecode (now `operands.rs:103`).
  2. `patch_string_operand` `FunctionRelative` didn't bound-check `insn_offset <
     bytecode_size` → could patch outside the function body (now `operands.rs:122`).
  3. `retarget_string`'s overflow test had a dead branch `off == 0x800000` (unreachable
     after the 23-bit mask); the real sentinel is `len == 0xff` (now `strings.rs:257`).
  4. CLI `run_retarget_string` indexed `file.strings[fid]`/`[tid]` **before**
     `retarget_string` validated the ids → panic on a bad `--from-id`/`--to-id` (now
     reads after validation via `.get()`, `write_cmd.rs:246`).
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
  to emit the bare id on stdout, human text on stderr (`write_cmd.rs:305`). **Implies:** the
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
  `../RISKS.md` § High-risk areas (exception-handler relocation, modern large-header magic offsets,
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

- **F10 — a table claiming a provenance it did not have.** `Bytecode99.json` recorded
  `GitCommitHash: 913d31acd…`, and the opcodes it contains had already been deleted upstream
  at that commit. The pin was decorative: written once, never checked, and wrong.
  **Implies:** recorded provenance is worth nothing without something that verifies it. When
  adding a pin, add the check in the same change, or the pin becomes a claim that ages
  silently — see R19 and The v99 opcode drift.
