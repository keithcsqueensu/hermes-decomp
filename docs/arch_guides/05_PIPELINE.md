# 05 — The pipeline: orchestration and the stage spine

> **Ownership.** *Owns* the orchestration that ties parse → IR → analysis → transforms →
> codegen together: the `Decompiler` façade, the **stage ordering** (F1–F26 per-function,
> W1–W17 whole-program), `PipelineContext`, batch/parallel rendering, module filtering, the
> on-disk cache, and `DecompileOptionsV2`. *Delegates* what each stage *does* to guides
> [`02`](02_IR.md)/[`03`](03_ANALYSIS.md)/[`04`](04_TRANSFORMS_CODEGEN.md). This is the guide
> to read first — the stage order is a load-bearing contract, not an implementation detail.

Files: `pipeline/` — `decompiler.rs`, `ir_gen.rs`, `stages.rs`, `mod.rs`, `batch.rs`,
`cache.rs`, `progress.rs`, `context/*` (incl. `context/naming/`, `context/rendering/`). The
proposal-artifact format and verifier that the `cascade` option feeds live outside, in
`crate::cascade`.

---

## What it does

The orchestration spine. It runs two coordinated flows: a **per-function** flow
(`generate_ir` → transforms → codegen) and a **whole-program** flow
(`PipelineContext::build_with_options`) that runs cross-function analysis *once*, then renders
every function cheaply against that shared context. It owns stage ordering, parallelism,
caching, module filtering and progress reporting.

## The `Decompiler` façade (`decompiler.rs`)

Thin public entry type. State: `file: BytecodeFile`, `format: BytecodeFormat`,
`closure_ctx: Option<ClosureContext>`.

| Method | Does |
|---|---|
| `Decompiler::new(bytes)` | `parse_auto` then `resolve_format` (diagnostic if opcode table substituted) |
| `from_parts(file, format)` | construct from already-parsed parts |
| `build_closure_context(&mut self)` | populate `closure_ctx` |
| `decompile_function(id, opts)` | single-function path → `decompile_function_v2_with_context` |
| `decompile_all(opts)` | whole-program path → `decompile_all_v2_with_closures` |
| `decompile_to_ir(id, opts)` | `generate_ir(...)`, returns `Vec<Statement>` not text |

Note the single-function path (`mod.rs:decompile_function_v2_with_context`) does **not** use
`PipelineContext`; it calls `generate_ir` then a reduced intra-function cleanup
(`strip_hermes_this`, `inline_named_variables`, `cleanup_noise`, `rename_reserved_words`,
`insert_declarations`) before codegen. Cross-function naming (IPA, Metro) is only available on
the whole-program path.

## The stage spine

Two nested sequences, documented authoritatively (and non-executably) in `stages.rs`.

`stages.rs` lists every stage in run order, and its test
(`stage_markers_are_listed_and_unique`) fails if a `// STAGE <id>:` marker in the pipeline is
missing from it or used twice. It had drifted badly before that test existed; see
`../plan_guides/05_pipeline/RISKS.md` § Stage labels. Stages inserted later carry sub-labels
(W4a, W11b, F5a, F25b) so existing numbers keep their meaning.

**Per-function — `ir_gen.rs:generate_ir`, F1–F26:**
- **F1** IR build (bytecode → CFG) via `IRBuilder::build_function`.
- **F2** `resolve_global_reads` then `transform_to_ssa`.
- **F3** copy/const propagation (`propagate_copies` cross-block, then `propagate`) — gated on
  `options.propagate`.
- **F4** `simplify_statements` per block — gated on `simplify`.
- **F5** structure recovery: `StructureAnalysis::analyze` → `to_statements` →
  `fold_builtin_guards` (HBC ≥ 97 `f.call === functionPrototypeCall` diamonds).
- **F5a** for-of (modern + `detect_legacy_for_of`)/for-in/iterator-destructuring detection,
  **before** inlining folds the iterator registers away.
- **F6–F25** (all gated on `simplify`): statement optimize → expression inline → logic
  transform → concat propagation → pattern detect → class detect → object/array literals →
  default params → spread/rest → destructuring → generator/async detect → yield-to-await →
  cleanup (basic + advanced) → chain access → ternary return → advanced logic simplify →
  CommonJS export / name inference → register naming (`apply_register_naming`, merges
  debug-info names) → semantic variable naming → `fold_slot_index_fills` → final
  `simplify_statements` (F25) → **F25b** `convert_while_true_loops` / `fold_guarded_loops`.
- **F26** closure resolution: `resolve_closures` if `perform_resolve`, a `ClosureContext` is
  provided, and the function has slots. The whole-program path passes `perform_resolve =
  false` and resolves later (W6/W9).

**Whole-program — `context/mod.rs:build_with_options`, W1–W17:**
- **W1** closure context; **W2** Metro detection (`build_metro_registry`, on **raw** IR);
  **W3–W4** optimized IR generation (parallel) + closure analyze/insert
  (`generate_all_optimized_ir`).
- **W4a** — only when `options.cascade` is set: `apply_cascade_artifact`
  loads a proposal artifact, verifies it against the bytecode (`crate::cascade::verify`) and
  applies the confirmed names to `all_ir` before naming, so they flow into IPA like any name
  the bytecode gave. A read/parse failure warns and names nothing.
- **W4b–W11e** naming/IPA/closures (`run_naming_pipeline`): two ground-truth module-naming
  passes (**W4b** `fileFinishedImporting("…")` paths, **W4c** source files baked into function
  names), module-name propagation (W5a–f), closure resolution, Metro export analysis, then
  **W7b** (single-export and function-table module names, `naming/module_table_names.rs`) and
  **W7c** (`finalize_module_specifiers`), 6-phase IPA
  (with `deep`: export keys fed into the callee index; factory params named from Metro
  roles), IPA re-resolve, closure property/definition naming. Then the naming tail:
  **W11a** late `dependencyMap` rewrite, **W11b**/**W11c** ancestor-name inherit + capture
  sync, **W11d** fixed-point IPA/naming loop (`deep` only), **W11e** content-hashed module
  names (`stable` only).
- **W12–W16** transform pipeline (`run_transform_pipeline`): strip-this, inlining (parallel,
  keeping names nested functions read), async detect + yield-to-await, async-wrapper
  unwrap, post-IPA transforms; then `transforms_phase.rs` W16a2 loop folds, W16a3 second
  JSX pass, W16b generator-wrapper collapse — which itself runs W16c HBC ≥ 97 generator
  lift (fail-closed: an unlifted machine is kept as decoded), an async re-scan, W16d v98 +
  Babel array destructuring and a second short-circuit pass, W16e a third JSX pass,
  re-inlining and `repair_switch_clobbers` — then W16f object-key closure naming and W16g
  `hoist_repeated_closures`.
- Worklet source recovery; parent→children inversion; **W16h** import hoisting
  (`hoist_module_loaders`).
- **W17** inline-body rendering (`build_all_inline_bodies`): pass 1 renders every body,
  later passes re-render only parents whose referenced bodies changed, until body holes stop
  decreasing (capped at 64 passes and a byte budget); bodies referenced but missing from
  `all_ir` are rebuilt from bytecode (`rendering/missing.rs`). Result is `Arc`-cached.

Render time (`context/codegen.rs:generate_function_code`) applies one more per-function
pass list — IPA param names, `cleanup_noise`, dead-store/dead-temp removal,
`collapse_metro_registry`, reserved words, `make_sanitized_names_distinct`,
`repair_switch_clobbers`, declaration insertion — and takes a `cascade_names` header name
over the string table's.

## `PipelineContext` (`context/mod.rs`)

The build-once/render-many shared context threaded through the whole-program path. Fields:
`all_ir: BTreeMap<u32, Vec<Statement>>`, `registry: MetroRegistry`,
`closure_ctx: Option<ClosureContext>`, `global_analysis: GlobalAnalysis` (IPA results),
`inline_bodies: Arc<BTreeMap<u32,String>>`, `child_functions: BTreeMap<u32,Vec<u32>>`
(parent→children, inverted once), `ancestor_env_slots: BTreeMap<u32,HashSet<String>>`
(precomputed top-down), `captured_by_descendants: BTreeMap<u32,HashSet<String>>` (names a
function's nested functions read/write — the "keep" set for dead-store removal),
`worklet_sources: BTreeMap<String,String>`, `cascade_names: BTreeMap<u32,String>` (confirmed
proposal names; empty unless `cascade` was set). `build_with_options` constructs it;
`context/codegen.rs:generate_function_code` renders one function against it.
Submodules split the phases: `ir_build.rs` (W2–W4), `naming.rs` (+`naming/module_table_names.rs`;
W4a–W11 and its own tail), `transforms_phase.rs` (W12–W16f), `rendering.rs`
(+`rendering/missing.rs`; W17), `codegen.rs`, plus `closures.rs`, `async_detection.rs`,
`generator_wrapper.rs`.

## Batch & parallelism (`batch.rs`)

`decompile_all_v2_with_closures` → `decompile_filtered_v2` builds a `PipelineContext` once,
then `render_bundle`. **Two-pass**: global analysis (the whole `PipelineContext` build) first,
then per-function rendering iterates `0..function_count`, grouping functions by Metro module
(climbing `closure_ctx.parent_function` via `get_root` to attribute child closures to their
module), emitting `// === Module N: name ===` blocks in dependency order
(`topological_module_order`: deps before dependents, id order among independents,
cycle-safe) then `// === Orphan Functions ===`. `ModuleFilter` (id_ranges,
name_globs, exclude_globs, a `from`+`depth` dependency subtree via BFS, case-insensitive
`glob_match`) selects modules; orphans are dropped when a filter is active. **Rayon**
parallelism lives in `ir_gen.rs:build_closure_context_from_file` (order-preserving
`into_par_iter`) and `generate_all_optimized_ir`; `configure_thread_pool()` runs at build
start. `analyze_module` is a standalone IPA-only path (raw IR, no `PipelineContext`) that
backs the CLI dead-code report and `callgraph`, `inspect.rs`, and the MCP analyze tools.

## Caching (`cache.rs`)

`PipelineContext::build_cached` serializes the built context to `<input>.hdcache`
(MessagePack via `rmp_serde`). The `CacheHeader` key = MAGIC `HDC1` + `CACHE_VERSION`
(currently **5**, bumped only on schema change) + SHA-256 of the bytecode bytes +
`binary_fingerprint()` (SHA-256 of the compile-time `DECOMP_BUILD_FINGERPRINT` — auto-
invalidates on any rebuild) + `options_key` (hashes the **whole** `DecompileOptionsV2`, so no
field can silently desync — regression-tested by
`cache.rs:every_option_field_changes_the_cache_key`). Any mismatch or read/write failure
falls back to a rebuild: **the cache is an optimization, never correctness.** Saves go
temp-file-then-rename with a pid-tagged temp name (`<input>.hdcache.<pid>.tmp`) to avoid
concurrent-writer corruption, serialising through a borrowed `PipelineSnapshotRef` rather
than a clone. Loads run on `crate::run_with_large_stack` (a 64 MiB thread) because the
decode recurses per IR level. The snapshot stores six fields (`all_ir`, `registry`,
`closure_ctx`, `global_analysis`, `inline_bodies`, `worklet_sources`); `child_functions`,
`ancestor_env_slots` and `captured_by_descendants` are recomputed on load, and
`cascade_names` is **not** stored — a cache hit always has none. The `options_key` does hash
the `cascade` *path* but not the artifact's contents, so the CLI forces `--no-cache` whenever
`--cascade` is given; a library caller of `build_cached` gets no such guard (see
`../plan_guides/05_pipeline/RISKS.md` § cascade).

## Progress (`progress.rs`)

Stderr progress gated by a global `AtomicBool` — off by default (library/tests silent),
enabled by the CLI. `status(msg)` prints a bullet; `Phase::start/finish` announce a labeled
timed phase and print elapsed seconds on drop.

## `DecompileOptionsV2` (`mod.rs`)

`#[derive(Hash)]` — load-bearing for the cache key.

| Field | Gates |
|---|---|
| `resolve_strings` | resolve string operands to literals |
| `include_offsets` | annotate bytecode offsets |
| `propagate` | F3 propagation |
| `simplify` | F4 and the whole F6–F25 block, plus single-function cleanup |
| `recover_structures` | F5 CFG→control-flow (else raw block dump) |
| `assembly_mode` | forces absolute offsets + `include_offsets` |
| `deep` | whole-program only: export keys in the IPA callee index + the fixed-point naming loop (`naming.rs` "W14") |
| `stable` | whole-program only: content-hashed names for still-unnamed modules (`naming.rs` "W15") |
| `cascade: Option<PathBuf>` | whole-program only: proposal artifact applied at W4a (`crate::cascade`) |

`build_with_options` honours only `assembly_mode`, `include_offsets`, `deep`, `stable` and
`cascade` from the caller and forces the rest to `optimized()`. Presets: `optimized()` (all
on except offsets/assembly; `deep`/`stable` off, no `cascade`), `debug()` (offsets on,
propagate/simplify off).

## File map

| File | Role |
|---|---|
| `mod.rs` | re-exports; `DecompileOptionsV2`; single-function path; `apply_register_naming` |
| `decompiler.rs` | `Decompiler` façade |
| `ir_gen.rs` | `generate_ir` (F1–F26); `build_closure_context_from_file` (parallel); yield→await |
| `stages.rs` | non-executable authoritative doc of W1–W17 / F1–F26, with sub-labels; a test keeps it in step with the `// STAGE` markers |
| `batch.rs` | `decompile_all/filtered_v2[_cached]`, `render_bundle`, `ModuleFilter`, `analyze_module` |
| `cache.rs` | `build_cached`, `CACHE_VERSION`, `default_cache_path`, `binary_fingerprint`, snapshot serde |
| `progress.rs` | stderr `status` / `Phase` |
| `context/mod.rs` | `PipelineContext` + `build_with_options` (W1–W17 driver); `apply_cascade_artifact` (W4a) |
| `context/ir_build.rs` | `build_metro_registry`, `generate_all_optimized_ir` (W2–W4) |
| `context/naming.rs` | `run_naming_pipeline` (W4a–W11 + its own "W12"–"W15" tail) |
| `context/naming/module_table_names.rs` | name still-unnamed modules from the function table (W7b) |
| `context/transforms_phase.rs` | `run_transform_pipeline` (W12–W16f), `collapse_generator_wrappers` |
| `context/rendering.rs` | `build_all_inline_bodies`, ancestor env-slot / captured-by-descendants precompute (W17) |
| `context/rendering/missing.rs` | rebuild bodies referenced but missing from `all_ir` |
| `context/codegen.rs` | `generate_function_code` (per-function render + render-time passes) |
| `context/{closures,async_detection,generator_wrapper}.rs` | closure/async/generator support |

Cross-cutting invariants to flag: the cache key hashes the entire options struct (but not
the contents of a `cascade` artifact);
debug-info variable names are linked via `function_scopes` (read from
`DebugOffsets.scopeDescData`), **not** the location stream's zero-valued `scopeAddress` (P1's
correction 1 — see `../plan_guides/01_read/unmodeled_regions/PLAN.md` § P1); and F5's
for-of/iterator detection must run before inlining folds iterator registers away.
