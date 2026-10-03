# 04 — Transforms + codegen: IR → idiomatic JS text

> **Ownership.** *Owns* the staged rewrite passes that turn register-level IR into
> JS-shaped IR, and the codegen that renders it to text. *Delegates* the *order* those passes
> run in — the F/W stage contract — to [`05_PIPELINE.md`](05_PIPELINE.md) (`pipeline/stages.rs`
> + `ir_gen.rs` own it; this guide catalogs the passes themselves), the *facts* the passes
> consume to [`03_ANALYSIS.md`](03_ANALYSIS.md), and the IR node definitions to
> [`02_IR.md`](02_IR.md).

Files: `transforms/` — ~23.6K LOC, the largest subsystem: 15 subdirs + 8 top-level files,
plus `transforms/codegen/`.

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

`transforms/mod.rs` is only a re-export facade; it defines no ordering.

## Transform catalog

**Data-flow / propagation**
- `ssa.rs::transform_to_ssa` — SSA form + live-range splitting (F2).
- `propagate.rs::{propagate, propagate_copies, resolve_global_reads}` — copy/const
  propagation, global-read resolution (F3).
- `data_flow/concat_propagate.rs::propagate_concatenation` — threads string-concat chains
  across temporaries (F9).
- `simplify.rs::{simplify_statements, simplify_expr}` — expression simplification, run at F4
  and again at F25.

**Statement optimization**
- `optimize/mod.rs::optimize_statements` — if-inversion (`invert.rs`), ternary detection
  (`ternary.rs`), dead-assignment elimination (`dead_assign.rs`), return merging
  (`merge_returns.rs`) (F6).
- `ternary_returns.rs::optimize_ternary_returns` — `if(c) return a; return b` →
  `return c?a:b` (F20).
- `logic_patterns.rs::transform_logic` — `&&`/`||`/`??` short-circuit reconstruction (F8).
- `logic_simplify/` — advanced boolean / De-Morgan simplification (F21).

**Structural / pattern recovery**
- `patterns/` — `detect_patterns` (concat, nullish, optional chaining), `jsx.rs` JSX
  reconstruction, and `patterns/loops/` (`while_true`, `for_of`, `for_in`, `for_loop`,
  `guarded_dowhile`) for loop recovery (F10; loops also pre-detected before generators).
- `class_patterns/` — `detect_class_patterns` drives `analyzer/ClassAnalyzer::analyze` +
  `analyzer/emit.rs` to rebuild ES6 `class` syntax from prototype/helper idioms (F11).
- `generator/` — `detect_generator_patterns`, `state_machine.rs` / `state_machine_v98.rs` /
  `simplify_state_machine`, `transform.rs`; reconstructs generator/async state machines (F16).
- `destructuring/` — `detect_destructuring`, `iterator.rs`, `v98.rs` (F15).

**JS-idiom reconstruction**
- `objects.rs::{transform_object_literals, fold_slot_index_fills}` — object literals from
  slot fills (F12).
- `arrays.rs::transform_array_literals` — array literals (F12).
- `default_params.rs::transform_default_params` — `x === undefined ? d : x` → default params
  (F13).
- `spread_rest.rs::transform_spread_rest` — spread/rest (F14).
- `chain_access/::optimize_chain_access` — collapse chained member/`.call` access (F19).

**Module / export / naming**
- `exports/` — `infer_commonjs_names`, `rename_param_registers` (F22).
- `module_hoist/` — `hoist_module_loaders` (`detect`, `hoist`, `lazy`, `names`) —
  whole-program require/module hoisting (W5/W16e).
- `name_inference.rs::infer_names` — heuristic local names (F22).
- `var_naming/` — `infer_variable_names` plus the closure-naming family
  (`closure_inference`, `closure_definitions`, `closure_usage`, `closure_def_naming`,
  `renaming`, `suggestions`) driving whole-program W10/W11 and F24.

**Inlining / cleanup**
- `inline/` — `inline_expressions` (single-use temp elimination, F7),
  `declarations::insert_declarations*`, `folding`, `inline_named`,
  `strip_this::strip_hermes_this` (W12), `arguments`, `esm_cleanup`, `reserved_words`.
- `cleanup/` — `cleanup_statements` + `advanced::cleanup_advanced` (dead loops, empty blocks,
  redundant/undefined removal, `ensure_return`) (F18).
- `var_kind.rs::promote_const_bindings` — `let`→`const`.
- `worklet_source.rs::collect_worklet_sources` — Reanimated worklet source capture.

## Codegen (`transforms/codegen/`)

Turns transformed IR into JS text. Entry type `codegen/mod.rs::Codegen` with
`CodegenOptions` (indent string, `include_labels`). `Codegen::generate_statements(&[Statement])
-> String` is the top entry, recursing via `stmt_gen.rs::generate_stmt`,
`expr_gen.rs::generate_expr`/`generate_expr_with_parens` (precedence-aware parenthesization),
and `control_flow.rs::generate_{if,while,do_while,for,try_catch}`. Rendering is
**string-concatenation**, indentation-driven by `indent_level`; a `DepthGuard` bounds
recursion.

Injected context uses a **`with_*` builder pattern** on `Codegen`:
- `with_imports(import_map: BTreeMap<u32,String>)` — require-id → module-name annotations.
- `with_esm_mode(dep_names)` + `with_esm_module_meta(dep_ids)` — ESM emission with stable
  `/* N */` module-id comments.
- `with_inline_bodies(Arc<BTreeMap<u32,String>>)` — pre-rendered nested-function bodies from
  whole-program stage W17, shared cheaply via `Arc`.

ESM output is a large sub-system on its own: `esm_gen`, `esm_imports`, `esm_classify`
(`EsmClassification`: Import/Export/ImportAndExport/Skip/Body), `esm_patterns`,
`esm_descriptors` (`Object.defineProperty` getter/value → `export`), `esm_boilerplate`.
Helpers `sanitize_import_name`, `sanitize_loop_var`, `replace_whole_word`, `indent_multiline`
handle identifier hygiene.

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
`visit_statement_list`); ~44 impls exist across the tree, so passes are a mix of
hand-recursion and visitor impls rather than one uniform framework.

## Notable design decisions / gotchas

- **Ordering is a contract.** `pipeline/stages.rs` exists solely to pin dependencies. E.g.
  loop `for-of`/`for-in` detection runs *before* `detect_patterns` — otherwise the
  `iter = src[Symbol.iterator]()` shape is destroyed. `while_true` / `fold_guarded_loops` run
  **last** (F25+), on fully-named statements, because they need clean output.
- **Metro/closure detection reads RAW IR** (W2) before optimization, to avoid pattern
  destruction; naming is a multi-pass whole-program dance (W5–W11) that must precede
  `strip_hermes_this` (W12) and inlining (W13).
- **Identity vs. payload split** — variable naming / closure resolution re-runs after IPA
  (W8→W9), so a single pass never both discovers and consumes a name.
- **Inline-body rendering (W17) is leaf-first** and cached in an `Arc` map handed to
  `Codegen`, decoupling nested-function text from the parent's codegen pass.

## File map

| Dir / file | Purpose |
|---|---|
| `ssa.rs`, `propagate.rs`, `simplify.rs`, `data_flow/` | SSA, copy/const & concat propagation, expr simplification |
| `optimize/` | if-inversion, ternary, dead-assign, return-merge |
| `logic_simplify/`, `logic_patterns.rs`, `ternary_returns.rs` | boolean / short-circuit / ternary reconstruction |
| `patterns/` (+`loops/`, `jsx.rs`) | concat/nullish/optional-chain, loop & JSX recovery |
| `class_patterns/` (+`analyzer/`) | ES6 class reconstruction |
| `generator/` | generator/async state-machine recovery |
| `destructuring/` | object/array/iterator destructuring |
| `objects.rs`, `arrays.rs`, `default_params.rs`, `spread_rest.rs`, `chain_access/` | object/array literals, default params, spread/rest, chained access |
| `exports/`, `module_hoist/`, `name_inference.rs`, `var_naming/` | CommonJS/ESM export inference, module hoisting, local & closure naming |
| `inline/`, `cleanup/`, `var_kind.rs`, `worklet_source.rs` | temp inlining, dead-code cleanup, const promotion, worklet capture |
| `codegen/` (`mod`, `expr_gen`, `stmt_gen`, `control_flow`, `format`, `esm_*`) | IR→JS text; `Codegen`/`CodegenOptions`, `with_*` context, ESM emission |

Key symbols: per-function driver `pipeline/ir_gen.rs::generate_ir`; codegen entry
`codegen/mod.rs::Codegen::generate_statements`; facade `transforms/mod.rs`.
