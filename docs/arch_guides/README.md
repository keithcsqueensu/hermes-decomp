# Architecture guides — the map of the crate

These guides describe **how `hbc-decomp` is built** — the layers, the types that cross
between them, and the one data-flow spine everything hangs off. They are the structural
companion to `../plan_guides/`, which own *open work and design decisions* on the write
path and the unmodeled regions. Rule of thumb:

- **How does X work today / where does X live?** → here (`arch_guides`).
- **Why is X unfinished / what would finishing it take?** → `../plan_guides`.
- **How do I run the tool?** → `../USAGE.md` (CLI) and `../LIBRARY.md` (API).

Every guide states its own **Ownership** near the top — what it covers and what it hands
off — the same contract the plan guides use. This file is the index over those blocks.

---

## The workspace

Three crates, one library and two thin frontends over it:

| Crate | Binary | LOC | Role |
|---|---|---|---|
| `hbc-decomp` | — (lib) | ~54K | Everything: parse, decompile, write. The subject of guides 01–06. |
| `hbc-decomp-cli` | `hermes-decomp` | ~7.4K | clap command surface + ratatui TUI over the library (guide 07). |
| `hbc-decomp-mcp` | `hermes-mcp` | ~1.5K | MCP server exposing the library to AI assistants (guide 07). |

Supports **HBC bytecode versions 40–99**. The per-version opcode/builtin tables are JSON
under `crates/hbc-decomp/resources/bytecode/` and are embedded at compile time by
`build.rs` (which also emits a build fingerprint used to invalidate the on-disk cache).

## The spine

There is exactly one data-flow spine, and reading it top to bottom is the fastest way to
understand the crate. Each stage is a guide:

```
 .hbc bytes
    │  parse_auto ──────────────────────────────────► 01_READ_LAYER
    ▼
 BytecodeFile  (typed sections, diagnostics, raw_bytes)
    │  IRBuilder::build_function (per function)
    ▼
 CFG of Statement/Expression  (registers still live) ► 02_IR
    │  analyses derive facts (closures, Metro, IPA…)
    ▼
 GlobalAnalysis / MetroRegistry / ClosureInfo … ─────► 03_ANALYSIS
    │  staged rewrite passes (F1–F26 / W1–W17)
    ▼
 JS-shaped IR ──────────────────────────────────────► 04_TRANSFORMS_CODEGEN
    │  Codegen::generate_statements
    ▼
 JavaScript text

 the orchestrator that runs all of the above ───────► 05_PIPELINE  (Decompiler, PipelineContext)
 a separate outbound path: edit/assemble/serialize ─► 06_WRITE_PATH  (see ../plan_guides)
 the two frontends that call the library ───────────► 07_FRONTENDS  (CLI + MCP)
```

The **pipeline guide (05)** is the one to read first if you only read one — it owns the
stage ordering (`pipeline/stages.rs` documents F1–F26 per-function and W1–W17
whole-program), and the ordering is a load-bearing contract, not an implementation detail.

## The guides

| Guide | Owns | Key entry points |
|---|---|---|
| [`01_READ_LAYER.md`](01_READ_LAYER.md) | binary → `BytecodeFile`; opcode tables; disasm | `BytecodeFile::parse_auto`, `disassemble_function` |
| [`02_IR.md`](02_IR.md) | the per-function IR: `Expression`/`Statement`/`CFG`, its construction and traversal | `IRBuilder::build_function` |
| [`03_ANALYSIS.md`](03_ANALYSIS.md) | read-only fact-gathering: closures, Metro, IPA, naming, structure, dataflow, xref | `resolve_closures`, `run_ipa`, `MetroRegistry::analyze` |
| [`04_TRANSFORMS_CODEGEN.md`](04_TRANSFORMS_CODEGEN.md) | the staged rewrite passes and JS text emission | `generate_ir`, `Codegen::generate_statements` |
| [`05_PIPELINE.md`](05_PIPELINE.md) | orchestration: the `Decompiler` façade, stage order, `PipelineContext`, batch/parallel, cache | `Decompiler`, `decompile_all_v2_with_closures` |
| [`06_WRITE_PATH.md`](06_WRITE_PATH.md) | encode/HASM/patch/serialize — summary + pointer to `../plan_guides` | `write/` module |
| [`07_FRONTENDS.md`](07_FRONTENDS.md) | the CLI command surface + TUI, and the MCP tool surface | `hermes-decomp`, `hermes-mcp` |

## Cross-cutting principles

Four ideas recur in every layer; they are the crate's design DNA.

1. **Degrade loudly, never crash.** The read layer records a structured `Diagnostic`
   (footer mismatch, layout fallback, opcode-table substitution, unreadable debug info)
   rather than failing; unknown opcodes become `Expression::Unknown`; the renderer emits a
   `TOO_DEEP` comment instead of overflowing the stack. A total decompile is the goal.
2. **Refuse rather than approximate.** An unknown *version* is an error, not a best guess —
   `ModernLayout::for_version` and `DebugLayout::for_version` allow-list what they support
   and hard-error the rest, because a wrong guess emits a VM-misread file. Contrast (1):
   *malformed input* degrades; an *unsupported format* refuses.
3. **Identity and payload are separate derivations.** Naming/closure resolution re-runs
   after IPA so a single pass never both discovers a name and consumes it (W8→W9). The same
   split is the core doctrine of the write path (`../plan_guides`).
4. **Ordering is a contract.** `pipeline/stages.rs` is a non-executable file that exists
   only to pin F/W stage dependencies and prevent silent reordering bugs.

## Cross-cutting infrastructure

- **Recursion & stack.** The IR is a tree of boxed nodes; every walker (Display, visitor,
  transforms) recurses. `configure_thread_pool()` (lib.rs) gives Rayon workers a 64 MiB
  stack and the CLI's `main` runs on a 64 MiB worker thread — both because the default
  ~1–2 MiB stack overflows on real bundles. `ir/depth.rs` bounds *render* recursion.
- **Parallelism.** Whole-program work fans out over Rayon (`build_closure_context_from_file`,
  `generate_all_optimized_ir`), order-preserving.
- **Caching.** `PipelineContext` serializes to `<input>.hdcache` (MessagePack), keyed on
  bytecode SHA-256 + build fingerprint + the whole options struct; any mismatch rebuilds.
- **Error model.** One `error::Error` enum (`Io`, `Parse`, `UnsupportedVersion`,
  `MissingFormat`, `Write`) with `Result<T>` throughout.

## Relationship to `plan_guides`

The write path (guide 06) is deliberately thin here: its invariants, hazards, and open work
have their own risk register and mitigation plans under `../plan_guides/06_write/`, one stage on
a plan-guides spine that mirrors these arch guides stage-for-stage and is indexed in
`../plan_guides/README.md`. Guide 06 is a structural map with pointers, not a second copy — same
one-home-per-fact rule those guides enforce among themselves.
