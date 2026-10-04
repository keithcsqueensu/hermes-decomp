# 03 — Analysis: read-only fact-gathering over the IR

> **Ownership.** *Owns* the analyses that derive *facts* from the IR without mutating it —
> who calls whom, what a register or closure slot really is, which Metro module a factory
> implements, where a loop or `if` lives in the CFG, where a string is referenced. *Delegates*
> the IR it reads to [`02_IR.md`](02_IR.md), and every *rewrite* that consumes these facts to
> [`04_TRANSFORMS_CODEGEN.md`](04_TRANSFORMS_CODEGEN.md). The decompiler's closure/env-slot
> *model* — and why closure names are not yet recovered from debug info — is owned by
> `../plan_guides/03_analysis/closure_model/PLAN.md`.

Files: `analysis/` — `closure/`, `dataflow/`, `ipa/`, `metro/`, `naming/`, `structure/`, plus
`liveness.rs`, `reaching.rs`, `loops.rs`, `xref.rs`.

---

## What it does

`analysis/` is the read-only phase between IR construction and the transform/codegen phases.
Almost every analysis is pure: it consumes IR (`Statement`/`Expression` trees and the
per-function `CFG`) and produces standalone result structs — `GlobalAnalysis`, `ClosureInfo`,
`MetroRegistry`, `StructureAnalysis`, `RegisterInfo` maps, `LivenessInfo` — that later phases
read to rename, restructure, and prune. Nothing here mutates the IR; the *application*
(`rename_registers`, `resolve_closures`) is a thin companion step the pipeline drives with
the facts these analyses produce. This is the "identity and payload are separate derivations"
principle in the small.

## The analyses

### closure/ — closure & environment-slot resolution
Hermes captures variables through a level/slot environment system; the IR carries
`Binding::ClosureVar { level, slot }`. This subsystem gives those slots real identifiers.
`closure/info/` builds the fact table: `ClosureInfo` (`info/types.rs:25`) holds
`slots: BTreeMap<u32, ClosureSlotValue>`, where `ClosureSlotValue` (`info/types.rs:10`) is
`Function | Constant | RegExp | Variable | Unknown`, keyed by level+slot packed via
`encode_level_slot` (`info/types.rs:5`). `store_slot` is **reuse-aware** — Hermes recycles
slots, so it refuses to let an ephemeral temp or a later regex overwrite a slot that already
has a stable name, avoiding TDZ-style mislabels. `closure/context/` (`ClosureContext`) walks
and merges captures across nested scopes; block environments a function builds on top of its
own get synthetic ids (`block_scopes`, `block_level`) so they take part in the parent chain.
Entry point `resolve_closures(stmts, &ClosureInfo)` (`closure/mod.rs:18`) rewrites every
`Binding::ClosureVar` to `Binding::Variable(get_slot_name(...))`, falling back to the
`closure_N` family for unresolved parent-env captures. `resolve_closures_recording` is the
same pass that also returns each baked name's `(level, slot)`; the pipeline keeps it as
`ClosureContext.baked_captures`, which the late inherit passes (`transforms/var_naming/
ancestor_inherit.rs`, `capture_sync.rs`) read to rename a `closure_L_S` once its owner has
named the slot.

### ipa/ — interprocedural analysis
Whole-program view to propagate parameter names and detect dead code.
`run_ipa(functions, metro_registry, func_name_index)` (`ipa/mod.rs:42`) returns
`GlobalAnalysis` (`ipa/structs.rs:16`: `param_names`, `param_links`, `graph: CallGraph`,
`dead_code`). Multi-pass: collect structural names + `ParamLink`s + call sites (`traversal/`,
which resolves each call against the definition *reaching* it via
`dataflow::reaching_bindings`, and `arg_hints.rs`, which chases a call argument through local
definitions to its source expression); infer names
from body usage and error strings; vote (`inference::vote_on_names`, rejecting generics);
then top-down, bottom-up and fixed-point propagation across `param_links` (capped at
`MAX_PARAM_LINK_ITERATIONS`); a typed fallback pass; and finally dead-code = all functions
minus those reachable from Metro-module roots via `CallGraph`. `FunctionNameIndex`
(`ipa/resolution.rs:9`) maps a name to `Vec<u32>` candidate ids (kept plural because bundles
duplicate names); `resolve_callee` uses it plus the Metro registry to resolve call edges. In
deep naming mode the pipeline re-runs `run_ipa` to a fixed point (`context/naming.rs`,
`MAX_DEEP_NAMING_ITERATIONS`), baking learned names into the bodies between runs.

### metro/ — Metro bundler module model
Recovers the module graph from the flat Hermes function soup. `MetroDetector`
(`metro/detection.rs:5`) scans for `__d(factory, id, deps)` registrations; `MetroRegistry`
(`metro/registry.rs:272`) holds `MetroModule`s keyed by module id, each tying a
`function_id` to its `FactoryRoles`. `FactoryRoles` (`registry.rs:21`) is the
version-independent trick: Metro's factory **arity** (4/5/6/7 declared params) *encodes* the
calling convention `(global, require, [importDefault, importAll,] module, exports[, deps])`,
derived by `from_param_count`. Entry point `MetroRegistry::analyze(statements)`
(`metro/mod.rs:587`). `propagation/` names modules from their exports and rewrites
`dependencyMap[i]` indices into named requires; a name comes back as an `InferredName`
(`propagation/inference.rs`) carrying `from_default_export`, stored on the module as
`MetroModule.name_from_default_export` so the copies of one Babel helper may all keep its
name. `known_libraries.rs` names a library module (`react`, `react-native`, `AssetRegistry`) only when its export
map contains every distinctive key of that library. `graph.rs` builds `DependencyGraph` /
`DependencyTree`. `mod.rs` also holds the large shared generic-name rejection lists so a
hoisted Babel helper never becomes a module's name.

### naming/ — register naming / role inference
`analyze_registers(stmts)` (`naming/registers.rs:37`) returns `BTreeMap<u32, RegisterInfo>`,
inferring a `RegisterRole` (`registers.rs:20`: Array/Object/Function/String/Promise/This/…)
plus accessed props, called methods and provenance. `generate_name(info, used_names)`
(`naming/generation.rs:100`) turns that into an identifier — destructuring key first, then
property-signature fingerprints (`{latitude,longitude}` → `location`; `{dispatch,getState}` →
`store`), then role fallback. `rename_registers(stmts, names)` (`naming/renaming.rs:4`)
applies the map.

### structure/ — control-structure recovery
Turns a `CFG` back into structured JS. `Structure` (`structure/mod.rs:19`) is the recovered
tree (If/While/DoWhile/For/Switch/TryCatch/Break/Continue/Label).
`StructureAnalysis::analyze(cfg)` (`structure/mod.rs:63`) delegates to `recovery::analyze`,
using `loops::LoopInfo` and exception handlers; `conversion.rs` lowers `Structure` back to
`Statement`s.

### liveness.rs / reaching.rs — CFG dataflow
`LivenessInfo::analyze(cfg)` (`liveness.rs:16`) is the standard backward fixed-point
(`live_in`/`live_out` register sets); its comment names DCE as the use case, but **nothing
outside the module calls it**. `ReachingDefs::analyze(cfg)` (`reaching.rs:23`) is the forward
dual over registers (`DefSite`s), and *is* consumed: by `transforms/ssa.rs` (F2 live-range
splitting) and by `resolve_global_reads` / `propagate_copies`
(`transforms/propagate/reaching_passes.rs`), all run in `pipeline/ir_gen.rs`.

### dataflow/ — dataflow over the structured IR
Structure recovery drops the CFG, yet the analyses that want a flow-sensitive answer run
after it. `dataflow/mod.rs` is a small engine over the *structured* tree that rebuilds the
joins control flow implies (`if` arms meet, loop bodies iterate to a fixed point bounded by
`MAX_LOOP_ITERATIONS`, a catch is reachable from anywhere in its try, a returning path
contributes nothing). A client supplies a `Fact` (join semilattice) and an `Analysis`
(`transfer`, `declare`); `solve` returns the fact after a body, `solve_observed` also reports
the fact on entry to each statement — unreachable statements included, with the fact frozen.
`dataflow/reaching_bindings.rs` is the one client: reaching definitions keyed by binding name,
whose `Reach` lattice collapses two different definitions to `Many` so a caller gets an
answer only when exactly one reaches. Consumed by `ipa/traversal/`.

### loops.rs — loop detection
`detect_loops(cfg)` (`loops.rs:21`) finds back-edges via `compute_dominators` (`loops.rs:110`)
and builds `LoopInfo` (header/body/exit/back_edges/`is_do_while`). It seeds exception catch
blocks as extra dominator roots so a `catch → return` edge is not misread as a back-edge.

### xref.rs — cross-reference / search
Operates on the raw `BytecodeFile`/`Instruction` (not IR). `find_string_xrefs`
(`xref.rs:11`) and `find_function_refs` (`xref.rs:56`) scan every function's decoded
instructions for string-id or function-id operands, returning
`XrefResult { function_id, offset, opcode }`.

## How analyses feed the pipeline

**Pipeline-consumed** (drive transforms/codegen): `ipa` (wired through `pipeline/stages.rs`,
`context/naming.rs`, cached in `pipeline/cache.rs`), `metro` (feeds IPA roots, module naming,
dependency rewriting), `closure` (`resolve_closures`, `resolve_closures_recording`), `naming`
(`rename_registers`), `structure` + `loops` (in `ir_gen.rs`; `loops` via
`structure/recovery.rs`), `reaching` (SSA and copy propagation in `ir_gen.rs`), `dataflow`
(via IPA collection).
**Standalone:** `xref` is a CLI/search facility over the raw file, not part of the transform
chain; `liveness.rs` has no caller outside its own module.

## Notable design decisions / gotchas

- **`closure_N` naming is load-bearing.** Both `resolve_closures` branches
  (`closure/mod.rs:197,280`) and `metro/mod.rs`'s `GENERIC_NAME_PREFIXES` treat unresolved
  parent-env captures as the same `closure_`/`c{slot}` family, and name-voting deliberately
  *rejects* that family as generic. Break the prefix and placeholders leak into module/param
  names. (This coupling is exactly what `../plan_guides/03_analysis/closure_model/PLAN.md` exists to
  restructure.)
- **Slot reuse vs. flow-insensitivity.** `store_slot`'s merge rules exist because Hermes
  recycles env slots; a naive last-write produces `sum = sum + 1` (TDZ) or mislabels a reused
  regex slot. Only an *exclusively* regex slot becomes `re{N}`.
- **Metro roles come from arity, not offsets** (`from_param_count`) — chosen to be
  version-independent. But `apply_metro_param_roles` must only run on *real* factory
  functions, or innocent `arg1` captures get renamed to `require`.
- **Generic-name rejection is centralized and large** (`metro/mod.rs`): transpiler helpers
  (`_callSuper`, `__awaiter`), framework keys, and numeric-suffix laundering (`keys1`) are
  filtered, while real names ending in digits (`Base64`, `Sha256`) survive by base-name check.
- **Duplicate names are expected**: `FunctionNameIndex` keeps `Vec<u32>` candidates; IPA
  resolves only when unique.
- **`xref.rs::has_function_operand` is heuristic** — it matches any UInt16/32 operand
  numerically because `BytecodeFormat` doesn't distinguish FunctionID from other indices, so
  false positives are possible (the comment flags it; the user verifies).
- **`MAX_PARAM_SLOTS = 1<<16`** guards against a corrupt param index driving gigabyte
  allocations in the propagation vectors.

## File map

| Path | Role |
|---|---|
| `analysis/mod.rs` | public re-exports of every analysis entry point |
| `closure/mod.rs` | `resolve_closures` / `resolve_closures_recording` — rewrites `ClosureVar`→identifier |
| `closure/info/{types,naming,value,analyze}.rs` | `ClosureInfo`, `ClosureSlotValue`, slot-name/merge logic |
| `closure/context/{mod,analyze,walk,merge,helpers,async_prop}.rs` | `ClosureContext` — cross-scope capture walk/merge, async propagation |
| `dataflow/mod.rs` | structured-IR dataflow engine: `Fact`, `Analysis`, `solve`, `solve_observed` |
| `dataflow/reaching_bindings.rs` | `ReachingDefinitions`, `Defs`, `Reach` — per-binding reaching definitions |
| `ipa/mod.rs` | `run_ipa` — multi-pass param-name propagation + dead code |
| `ipa/structs.rs` | `GlobalAnalysis`, `ParamLink` |
| `ipa/graph.rs` | `CallGraph`, reachability / post-order |
| `ipa/resolution.rs` | `FunctionNameIndex`, `resolve_callee` |
| `ipa/traversal/{mod,visitor}.rs` | per-function collection: flow-sensitive definition lookup, call-graph linking, call-site hints |
| `ipa/{inference,arg_hints,body_hints,error_string_hints,property_accesses,hints_tables}.rs` | name voting, hints |
| `metro/registry.rs` | `MetroRegistry`, `MetroModule`, `FactoryRoles` |
| `metro/detection.rs` | `MetroDetector` — `__d(...)` scanning |
| `metro/graph.rs` | `DependencyGraph`, `DependencyTree` |
| `metro/exports.rs` | module-name inference from exports |
| `metro/known_libraries.rs` | name a library module by its complete export surface |
| `metro/propagation/*` | `propagate_module_names`, `InferredName`, dependency-map rewrite (`depmap_rewrite{.rs,/}`) |
| `metro/mod.rs` | generic-name lists, `is_obviously_generic`, `MetroRegistry::analyze` |
| `naming/registers.rs` | `analyze_registers`, `RegisterInfo`, `RegisterRole` |
| `naming/generation.rs` | `generate_name` (property-signature fingerprints) |
| `naming/renaming.rs` | `rename_registers` |
| `structure/{mod,recovery,conversion,loops,exceptions}.rs` | `Structure`, `StructureAnalysis`, CFG→structured tree |
| `liveness.rs` | `LivenessInfo` (backward dataflow, **no caller**) |
| `reaching.rs` | `ReachingDefs` (forward CFG dataflow; SSA + copy propagation) |
| `loops.rs` | `detect_loops`, `LoopInfo`, `compute_dominators` |
| `xref.rs` | `find_string_xrefs`, `find_function_refs` (CLI search) |
