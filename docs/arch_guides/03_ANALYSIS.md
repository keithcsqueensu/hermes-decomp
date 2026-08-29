# 03 — Analysis: read-only fact-gathering over the IR

> **Ownership.** *Owns* the analyses that derive *facts* from the IR without mutating it —
> who calls whom, what a register or closure slot really is, which Metro module a factory
> implements, where a loop or `if` lives in the CFG, where a string is referenced. *Delegates*
> the IR it reads to [`02_IR.md`](02_IR.md), and every *rewrite* that consumes these facts to
> [`04_TRANSFORMS_CODEGEN.md`](04_TRANSFORMS_CODEGEN.md). The decompiler's closure/env-slot
> *model* — and why closure names are not yet recovered from debug info — is owned by
> `../plan_guides/03_analysis/closure_model/PLAN.md`.

Files: `analysis/` — `closure/`, `ipa/`, `metro/`, `naming/`, `structure/`, plus
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
`Value::ClosureVar { level, slot }`. This subsystem gives those slots real identifiers.
`closure/info/` builds the fact table: `ClosureInfo` (`info/types.rs:22`) holds
`slots: BTreeMap<u32, ClosureSlotValue>`, where `ClosureSlotValue` (`info/types.rs:10`) is
`Function | Constant | RegExp | Variable | Unknown`, keyed by level+slot packed via
`encode_level_slot` (`info/types.rs:5`). `store_slot` is **reuse-aware** — Hermes recycles
slots, so it refuses to let an ephemeral temp or a later regex overwrite a slot that already
has a stable name, avoiding TDZ-style mislabels. `closure/context/` (`ClosureContext`) walks
and merges captures across nested scopes. Entry point `resolve_closures(stmts, &ClosureInfo)`
(`closure/mod.rs:17`) rewrites every `ClosureVar` to `Value::Variable(get_slot_name(...))`,
falling back to the `closure_N` family for unresolved parent-env captures.

### ipa/ — interprocedural analysis
Whole-program view to propagate parameter names and detect dead code.
`run_ipa(functions, metro_registry, func_name_index)` (`ipa/mod.rs:41`) returns
`GlobalAnalysis` (`ipa/structs.rs:16`: `param_names`, `param_links`, `graph: CallGraph`,
`dead_code`). Multi-pass: collect structural names + `ParamLink`s + call sites; infer names
from body usage and error strings; vote (`inference::vote_on_names`, rejecting generics);
then top-down, bottom-up and fixed-point propagation across `param_links` (capped at
`MAX_PARAM_LINK_ITERATIONS`); a typed fallback pass; and finally dead-code = all functions
minus those reachable from Metro-module roots via `CallGraph`. `FunctionNameIndex`
(`ipa/resolution.rs:9`) maps a name to `Vec<u32>` candidate ids (kept plural because bundles
duplicate names); `resolve_callee` uses it plus the Metro registry to resolve call edges.

### metro/ — Metro bundler module model
Recovers the module graph from the flat Hermes function soup. `MetroDetector`
(`metro/detection.rs:5`) scans for `__d(factory, id, deps)` registrations; `MetroRegistry`
(`metro/registry.rs:206`) holds `MetroModule`s keyed by module id, each tying a
`function_id` to its `FactoryRoles`. `FactoryRoles` (`registry.rs:21`) is the
version-independent trick: Metro's factory **arity** (4/5/6/7 declared params) *encodes* the
calling convention `(global, require, [importDefault, importAll,] module, exports[, deps])`,
derived by `from_param_count`. Entry point `MetroRegistry::analyze(statements)`
(`metro/mod.rs:181`). `propagation/` names modules from their exports and rewrites
`dependencyMap[i]` indices into named requires. `graph.rs` builds `DependencyGraph` /
`DependencyTree`. `mod.rs` also holds the large shared generic-name rejection lists so a
hoisted Babel helper never becomes a module's name.

### naming/ — register naming / role inference
`analyze_registers(stmts)` (`naming/registers.rs:37`) returns `BTreeMap<u32, RegisterInfo>`,
inferring a `RegisterRole` (`registers.rs:20`: Array/Object/Function/String/Promise/This/…)
plus accessed props, called methods and provenance. `generate_name(info, used_names)`
(`naming/generation.rs:33`) turns that into an identifier — destructuring key first, then
property-signature fingerprints (`{latitude,longitude}` → `location`; `{dispatch,getState}` →
`store`), then role fallback. `rename_registers(stmts, names)` (`naming/renaming.rs:4`)
applies the map.

### structure/ — control-structure recovery
Turns a `CFG` back into structured JS. `Structure` (`structure/mod.rs:19`) is the recovered
tree (If/While/DoWhile/For/Switch/TryCatch/Break/Continue/Label).
`StructureAnalysis::analyze(cfg)` (`structure/mod.rs:63`) delegates to `recovery::analyze`,
using `loops::LoopInfo` and exception handlers; `conversion.rs` lowers `Structure` back to
`Statement`s.

### liveness.rs / reaching.rs — dataflow
`LivenessInfo::analyze(cfg)` (`liveness.rs:16`) is the standard backward fixed-point
(`live_in`/`live_out` register sets) for DCE. `ReachingDefs::analyze(cfg)` (`reaching.rs:25`)
is the forward dual — its header comment marks it **"Reserved for future… not yet used in
the pipeline."**

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
dependency rewriting), `closure` (`resolve_closures`), `naming` (`rename_registers`),
`structure` + `loops` (in `ir_gen.rs` and the loop transforms), `liveness` (DCE).
**Standalone:** `xref` is a CLI/search facility over the raw file, not part of the transform
chain; `reaching.rs` is declared-unused infrastructure.

## Notable design decisions / gotchas

- **`closure_N` naming is load-bearing.** Both `resolve_closures` branches
  (`closure/mod.rs:149,227`) and `metro/mod.rs`'s `GENERIC_NAME_PREFIXES` treat unresolved
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
| `closure/mod.rs` | `resolve_closures` — rewrites `ClosureVar`→identifier |
| `closure/info/{types,naming,value,analyze}.rs` | `ClosureInfo`, `ClosureSlotValue`, slot-name/merge logic |
| `closure/context/*` | `ClosureContext` — cross-scope capture walk/merge |
| `ipa/mod.rs` | `run_ipa` — multi-pass param-name propagation + dead code |
| `ipa/structs.rs` | `GlobalAnalysis`, `ParamLink` |
| `ipa/graph.rs` | `CallGraph`, reachability / post-order |
| `ipa/resolution.rs` | `FunctionNameIndex`, `resolve_callee` |
| `ipa/{traversal,inference,body_hints,error_string_hints,property_accesses,hints_tables}.rs` | fact collection, name voting, hints |
| `metro/registry.rs` | `MetroRegistry`, `MetroModule`, `FactoryRoles` |
| `metro/detection.rs` | `MetroDetector` — `__d(...)` scanning |
| `metro/graph.rs` | `DependencyGraph`, `DependencyTree` |
| `metro/exports.rs` | module-name inference from exports |
| `metro/propagation/*` | `propagate_module_names`, dependency-map rewrite |
| `metro/mod.rs` | generic-name lists, `is_obviously_generic`, `MetroRegistry::analyze` |
| `naming/registers.rs` | `analyze_registers`, `RegisterInfo`, `RegisterRole` |
| `naming/generation.rs` | `generate_name` (property-signature fingerprints) |
| `naming/renaming.rs` | `rename_registers` |
| `structure/{mod,recovery,conversion,loops,exceptions}.rs` | `Structure`, `StructureAnalysis`, CFG→structured tree |
| `liveness.rs` | `LivenessInfo` (backward dataflow, DCE) |
| `reaching.rs` | `ReachingDefs` (forward dataflow, **unused**) |
| `loops.rs` | `detect_loops`, `LoopInfo`, `compute_dominators` |
| `xref.rs` | `find_string_xrefs`, `find_function_refs` (CLI search) |
