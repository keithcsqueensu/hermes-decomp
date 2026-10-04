# 04 — Transforms + codegen: IR → idiomatic JS text

> **Ownership.** *Owns* the staged rewrite passes that turn register-level IR into
> JS-shaped IR, and the codegen that renders it to text. *Delegates* the *order* those passes
> run in — the F/W stage contract — to [`05_PIPELINE.md`](05_PIPELINE.md) (`pipeline/stages.rs`
> + `ir_gen.rs` own it; this guide catalogs the passes themselves), the *facts* the passes
> consume to [`03_ANALYSIS.md`](03_ANALYSIS.md), and the IR node definitions to
> [`02_IR.md`](02_IR.md).

Files: `transforms/` — ~35.5K LOC (of which `codegen/` ~6.3K), the largest subsystem: 17
subdirs (incl. `codegen/`) + 13 top-level files (incl. `mod.rs`).

---

## What it does

Takes decoded, register-level IR (a CFG of `Statement`/`Expression`) and progressively
rewrites it into idiomatic-JavaScript-shaped IR, then renders that IR to text. It is a
**staged pass pipeline**, and the ordering is genuinely load-bearing (see the gotchas). The
canonical order lives in two places, both owned by guide 05:

- `pipeline/stages.rs` — a *non-executable documentation file* enumerating whole-program
  stages **W1–W17** and per-function stages **F1–F26** with explicit `REQUIRES`/`OUTPUT`
  dependencies, written specifically to prevent silent reordering bugs.
- `pipeline/ir_gen.rs::generate_ir` — the **executable** per-function order.

Many passes below also run again in the whole-program stages and at render time
(`pipeline/context/{transforms_phase,codegen,rendering}.rs`); `stages.rs` no longer lists all
of those — guide 05 § The stage spine has the executed order and the drift.

`transforms/mod.rs` is only a re-export facade; it defines no ordering.

## Transform catalog

**Data-flow / propagation**
- `ssa.rs::transform_to_ssa` — SSA form + live-range splitting (F2).
- `propagate/` — `propagate` (`mod.rs` + `substitute.rs`) and
  `reaching_passes.rs::{propagate_copies, resolve_global_reads}` — copy/const propagation
  (F3), global-read resolution (F2, before SSA).
- `data_flow/concat_propagate.rs::propagate_concatenation` — threads string-concat chains
  across temporaries (F9).
- `simplify.rs::{simplify_statements, simplify_expr}` — expression simplification, run at F4
  and again at F25.

**Statement optimization**
- `optimize/mod.rs::optimize_statements` — if-inversion (`invert.rs`), ternary detection
  (`ternary.rs`), dead-assignment elimination (`dead_assign.rs`), return merging
  (`merge_returns.rs`) (F6). The same dir holds passes the pipeline calls directly:
  `builtin_guard.rs::fold_builtin_guards` (HBC ≥ 97 `f.call === functionPrototypeCall`
  diamonds, F5), `eliminate_dead_stores`, `dead_bindings.rs::remove_dead_temp_bindings*`
  (late unread pure temps, keyed on a keep-set of names nested functions read), and
  `switch_clobber/::repair_switch_clobbers` (`kind = kind.kind` after renaming) — the last
  in the W16 tail and again at render time.
- `ternary_returns.rs::optimize_ternary_returns` — `if(c) return a; return b` →
  `return c?a:b` (F20).
- `logic_patterns.rs::transform_logic` — `&&`/`||`/`??` short-circuit reconstruction (F8).
- `logic_simplify/` — advanced boolean / De-Morgan simplification (F21).

**Structural / pattern recovery**
- `patterns/` — `detect_patterns` (short-circuit, Babel for-of, nullish, optional chaining,
  concat, an in-place JSX pass), `logic_short_circuit.rs::detect_short_circuit_logic` (re-run
  in W16d on named bindings), `jsx.rs` + `jsx/{props,fold}.rs` (`reconstruct_jsx`, re-run
  twice in W16 once props objects are complete), and `patterns/loops/` (`while_true`,
  `for_of/{modern,legacy}`, `for_in`, `for_loop`, `guarded_dowhile`) for loop recovery (F10;
  for-of/for-in also pre-detected right after F5; `while_true`/`fold_guarded_loops` last in
  F25 and again in W16a2).
- `class_patterns/` — `detect_class_patterns` drives `analyzer/ClassAnalyzer::analyze` +
  `analyzer/emit.rs` to rebuild ES6 `class` syntax from prototype/helper idioms (F11).
- `generator/` — `detect_generator_patterns`, `state_machine.rs` / `simplify_state_machine`,
  `transform.rs`; reconstructs generator/async state machines (F16).
  `state_machine_v98.rs::try_reconstruct_generator_v98` lifts the HBC ≥ 97 desugared
  `function*` switch back to flat `yield`s (W16c); it returns `None` on any shape mismatch and
  the caller then keeps the body exactly as decoded.
- `destructuring/` — `detect_destructuring` (F15), `iterator.rs::detect_iterator_destructuring`
  (right after F5), `v98.rs::reconstruct_v98_array_destructuring` and
  `babel.rs::reconstruct_babel_array_destructuring` (`_slicedToArray(src, n)` + indexed reads →
  `[a, b] = src`) (both W16d).

**JS-idiom reconstruction**
- `objects/` — `transform_object_literals` (`mod.rs`, with `inline_literals.rs` folding
  single-use literals) (F12), and `slot_fills.rs::fold_slot_index_fills` — shape-table
  `obj[N] = val` placeholder fills, run after F24 and again in W16/W16c because naming turns
  the register into a variable.
- `arrays.rs::transform_array_literals` — array literals (F12).
- `default_params.rs::transform_default_params` — `x === undefined ? d : x` → default params
  (F13).
- `spread_rest.rs::transform_spread_rest` (+`spread_rest/apply.rs`, `HermesBuiltin.apply` →
  spread) — spread/rest (F14).
- `chain_access/::optimize_chain_access` — collapse chained member/`.call` access (F19).

**Module / export / naming**
- `exports/` — `infer_commonjs_names`, `rename_param_registers` (F22).
- `module_hoist/` — `hoist_module_loaders` (`detect`, `hoist`, `kinds`, `lazy`, `names`) —
  whole-program require/module hoisting (W16e in `context/mod.rs`).
- `hoist_closures.rs::hoist_repeated_closures` — a non-escaping function hermesc re-creates
  at every call site gets one declaration in the sites' common scope (end of W16).
- `collapse_registry.rs::collapse_metro_registry` — folds the entry function's run of ≥ 16
  `__d(factory, id, deps)` registrations into one comment (render time).
- `name_inference.rs::infer_names` — heuristic local names (F22).
- `var_naming/` — `infer_variable_names` plus the closure-naming family
  (`closure_inference`, `closure_definitions`, `closure_usage`, `closure_def_naming`,
  `renaming`, `suggestions`) driving whole-program W10/W11 and F24, and
  `ancestor_inherit.rs::inherit_ancestor_closure_names` / `capture_sync.rs::sync_capture_names`
  (baked `closure_{level}_{slot}` captures take the ancestor's / owner's current name;
  `naming.rs` "W13"/"W13c", W16f).

**Inlining / cleanup**
- `inline/` — `inline_expressions` (single-use temp elimination, F7),
  `declarations/::insert_declarations*` (`mod.rs` + `scan`, `patterns`, `writes`), `folding`,
  `inline_named` (`inline_named_variables[_keeping]` W13, `eliminate_immutable_aliases` for
  `deep`), `captured.rs::names_used_by_descendants` (the keep-set: names nested functions
  still need bound), `strip_this::strip_hermes_this` (W12), `arguments`, `esm_cleanup`,
  `reserved_words`, `cleanup::cleanup_noise`.
- `cleanup/` — `cleanup_statements` + `advanced::cleanup_advanced` (dead loops, empty blocks,
  redundant/undefined removal, `ensure_return`) (F18).
- `var_kind.rs::promote_const_bindings` — `let`→`const`. Exported but **not wired** into the
  pipeline, deliberately: env slots are shared across closures, and promotion caused
  const-reassign / TDZ parse failures (the NOTE after W16 in `transforms_phase.rs`).
- `worklet_source.rs::collect_worklet_sources` — Reanimated worklet source capture.

## Codegen (`transforms/codegen/`)

Turns transformed IR into JS text. Entry type `codegen/mod.rs::Codegen` with
`CodegenOptions` (indent string, `include_labels`). `Codegen::generate_statements(&[Statement])
-> String` is the top entry, recursing via `stmt_gen.rs::generate_stmt`,
`expr_gen.rs::generate_expr`/`generate_expr_with_parens` (precedence-aware parenthesization),
and `control_flow.rs::generate_{if,while,do_while,for,try_catch}`. Rendering is
**string-concatenation**, indentation-driven by `indent_level`; a `DepthGuard` bounds
statement recursion and an `expr_depth` counter elides past the cap on deep expression
chains. A function body W17 could not render prints as `body_hole(id)` (`BODY_HOLE`
marker), which the inline-body passes count to know when to stop.

Injected context uses a **`with_*` builder pattern** on `Codegen`:
- `with_imports(import_map: BTreeMap<u32,String>)` — require-id → module-name annotations.
- `with_esm_mode(dep_names)` + `with_esm_module_meta(dep_ids)` — ESM emission with stable
  `/* N */` module-id comments.
- `with_inline_bodies(Arc<BTreeMap<u32,String>>)` — pre-rendered nested-function bodies from
  whole-program stage W17, shared cheaply via `Arc`.
- `with_nested_writes(BTreeMap<String,usize>)` — names written inside the inlined descendant
  bodies (which exist here only as strings), so a write buried in one still invalidates an
  import binding.

ESM output is a large sub-system on its own, entered at `esm_gen.rs::generate_esm_module`:
`esm_gen`, `esm_imports`, `esm_classify` (`EsmClassification`:
Import/Export/ImportAndExport/ImportAndBody/Skip/Body — `ImportAndBody` is an import plus a
`let X = X_mod` body line, for a module that reassigns the name), `esm_patterns`,
`esm_descriptors` (`Object.defineProperty` getter/value → `export`), `esm_boilerplate`.
After classification, `esm_gen` and `esm_imports` run a suite of passes over the **rendered
lines** to keep the module parseable: import consolidation and dedupe
(`consolidate_imports`, `fold_redundant_imports`, `make_default_imports_distinct`,
`dedupe_import_bindings`), export/declaration collision resolution
(`dedupe_function_export_collisions`, `resolve_import_declaration_collisions`, the
`export const X` private-binding rewrite), alias/hoist de-duplication (`alias_line_once`,
`demote_alias_lines_shadowed_by_declarations`, `drop_hoists_shadowed_by_declarations`), and
`undeclared_assignments` (declare names written but never bound). Helpers
`sanitize_import_name`, `sanitize_loop_var`, `replace_whole_word`, `indent_multiline` handle
identifier hygiene.

## Interaction with the IR

Two coexisting styles:
- **Rebuild-style** (most structural/recovery passes): take `Vec<Statement>` by value, return
  a fresh `Vec` — `optimize_statements`, `detect_destructuring`, `inline_expressions`,
  `cleanup_statements`, `infer_variable_names`, `optimize_chain_access`,
  `detect_generator_patterns`.
- **In-place** via `&mut` — `transform_object_literals`, `transform_array_literals`,
  `transform_default_params`, `transform_logic`, `infer_names`, `fold_slot_index_fills`,
  `infer_commonjs_names`.

Shared traversal infra is `ir/visitor.rs` (`Visitor` read / `MutVisitor` write, with
`visit_statement_list`); ~90 impls exist across the crate (~66 under `transforms/`), so passes are a mix of
hand-recursion and visitor impls rather than one uniform framework.

## Notable design decisions / gotchas

- **Ordering is a contract.** `pipeline/stages.rs` exists solely to pin dependencies. E.g.
  loop `for-of`/`for-in` detection runs *before* `detect_patterns` — otherwise the
  `iter = src[Symbol.iterator]()` shape is destroyed. `while_true` / `fold_guarded_loops` run
  **last** (F25+), on fully-named statements, because they need clean output — and again in
  W16a2 once whole-program inlining has cleaned the latch.
- **Second passes are the norm, not a smell.** JSX, short-circuit folding, slot-fill folding,
  inlining and loop folding each re-run in W16 because naming/inlining re-creates their input
  shape on named bindings. A pass that assumes its input is register-spelled will miss the
  later runs.
- **Fail closed on unrecognised shapes.** The HBC ≥ 97 generator lift and the v98/Babel
  destructuring matchers return the body unchanged on any mismatch; the cleanups that follow
  cannot follow data flow through raw resume-machine label/status slots and have deleted live
  code when fed one.
- **Metro/closure detection reads RAW IR** (W2) before optimization, to avoid pattern
  destruction; naming is a multi-pass whole-program dance (W5–W11) that must precede
  `strip_hermes_this` (W12) and inlining (W13).
- **Identity vs. payload split** — variable naming / closure resolution re-runs after IPA
  (W8→W9), so a single pass never both discovers and consumes a name.
- **Inline-body rendering (W17) is leaf-first** (multi-pass to a fixed point; guide 05) and cached in an `Arc` map handed to
  `Codegen`, decoupling nested-function text from the parent's codegen pass.

## File map

| Dir / file | Purpose |
|---|---|
| `ssa.rs`, `propagate/`, `simplify.rs`, `data_flow/` | SSA, copy/const & concat propagation, global reads, expr simplification |
| `optimize/` (+`switch_clobber/`) | if-inversion, ternary, dead-assign, return-merge; builtin-guard fold, dead stores/temps, switch-clobber repair |
| `logic_simplify/`, `logic_patterns.rs`, `ternary_returns.rs` | boolean / short-circuit / ternary reconstruction |
| `patterns/` (+`loops/` incl. `loops/for_of/`, `jsx.rs` + `jsx/`) | short-circuit/concat/nullish/optional-chain, loop & JSX recovery |
| `class_patterns/` (+`analyzer/`) | ES6 class reconstruction |
| `generator/` (+`state_machine_v98.rs`) | generator/async state-machine recovery, HBC ≥ 97 lift |
| `destructuring/` (+`babel.rs`, `v98.rs`) | object/array/iterator destructuring |
| `objects/`, `arrays.rs`, `default_params.rs`, `spread_rest.rs` (+`spread_rest/`), `chain_access/` | object/array literals & slot fills, default params, spread/rest, chained access |
| `exports/`, `module_hoist/`, `hoist_closures.rs`, `collapse_registry.rs`, `name_inference.rs`, `var_naming/` | CommonJS/ESM export inference, module & closure hoisting, Metro registry collapse, local & closure naming |
| `inline/` (+`declarations/`), `cleanup/`, `var_kind.rs`, `worklet_source.rs` | temp inlining, declarations, dead-code cleanup, const promotion (unwired), worklet capture |
| `codegen/` (`mod`, `expr_gen`, `stmt_gen`, `control_flow`, `format`, `esm_*`, `tests`) | IR→JS text; `Codegen`/`CodegenOptions`, `with_*` context, ESM emission + rendered-line repair |

Key symbols: per-function driver `pipeline/ir_gen.rs::generate_ir`; codegen entry
`codegen/mod.rs::Codegen::generate_statements`; facade `transforms/mod.rs`.
