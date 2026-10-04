# Transforms + codegen — risk register

> **Ownership.** *Owns* the risk that the transform/codegen phase mis-rewrites the IR or emits
> wrong JavaScript. *Delegates* the phase's *description* — the F/W stage catalogue, the pass
> families, the `Codegen` `with_*` context — to `../../arch_guides/04_TRANSFORMS_CODEGEN.md`.

Status: **no open defects; one watch item** (re-audited against HEAD 81c4e2a). No robustness
finding from the read hardening pass landed on this stage, and no version-drift hazard of the
write path's kind lives here. This register exists as the stage's vertebra on the spine; the
notes below are the standing hazards to respect when adding a pass, plus one structural
weakness — the ESM rendered-line repair — that is not a known bug but is where the next one is
most likely. The ordering file these hazards point at (`pipeline/stages.rs`) had drifted from
the executed order; it is back in step and test-guarded — `../05_pipeline/RISKS.md`
§ Stage labels.

---

## Standing hazards (respect when adding a pass)

These are properties the transforms rely on rather than bugs. They are documented in the arch
guide and pinned in `pipeline/stages.rs`; restated here as the "where a transform change goes
wrong" checklist:

- **Pass ordering is a contract.** `pipeline/stages.rs` exists solely to pin F/W stage
  dependencies and prevent silent reordering. The sharp edges: loop `for-of`/`for-in` detection
  must run *before* `detect_patterns` (else the `iter = src[Symbol.iterator]()` shape is
  destroyed); `while_true`/`fold_guarded_loops` must run **last** (F25+), on fully-named
  statements; Metro/closure detection reads **raw** IR (W2) before optimization. A reordering
  that violates one of these produces plausible-but-wrong JavaScript — the same failure class
  the read register calls out, one layer up.
- **Identity and payload stay separate.** Variable naming / closure resolution re-runs after IPA
  (W8→W9) so a single pass never both discovers and consumes a name. A new naming pass that
  folds discovery and use together reintroduces the coupling the split exists to avoid.
- **Rebuild vs. in-place is not uniform.** Most structural passes take `Vec<Statement>` by value
  and return a fresh `Vec`; a second group mutates via `&mut`. A pass that assumes the wrong one
  either drops edits or double-applies them. See `../../arch_guides/04_TRANSFORMS_CODEGEN.md`
  § Interaction with the IR.
- **Render recursion is bounded, but only at render time.** Codegen routes through the same
  `DepthGuard` the IR uses (F9, `../02_ir/RISKS.md`); a new recursive emitter that bypasses it
  reopens the stack-overflow hole.
- **Passes run more than once, on differently-spelled input.** JSX, short-circuit, slot-fill
  folding, inlining and loop folding each run per-function *and* again in the whole-program
  W16 stages, after naming has turned registers into named bindings. A matcher that only
  recognises the register spelling silently does nothing on the later runs.
- **Lifts must fail closed.** The HBC ≥ 97 generator lift (`try_reconstruct_generator_v98`)
  and the v98/Babel destructuring matchers return the body unchanged on any shape mismatch.
  That is load-bearing: the dead-store/dead-temp cleanups that follow cannot follow data flow
  through a raw resume machine's label/status slots, and once deleted live code (the
  `piloteAuthHeaders` header build) when handed one — see the comment at the lift call in
  `pipeline/context/transforms_phase.rs`. A new lift that returns a partial rewrite reopens it.
- **Dead-code removal needs the descendant keep-set.** After closure resolution a captured
  slot is a plain name in each body, so a single-body pass sees a store nobody reads.
  Removal passes take `names_used_by_descendants` / `captured_by_descendants` as a keep-set
  (`*_keeping` variants); a new one that skips it drops stores nested functions read.

## Watch item — ESM repair works on rendered text

Not a known defect; registered because it is the stage's least-structured surface.
`codegen/esm_gen.rs` and `esm_imports.rs` grew (net ~+1.4K lines since 2026-08-28) a suite of
passes that keep a module parseable — import consolidation/dedupe, export/import/declaration
collision resolution, alias and `let X;` hoist de-duplication, `undeclared_assignments` — and
they operate on **rendered lines** (`Vec<String>`), recognising `import`/`export`/`let`/
`function` heads and identifiers by string matching (`parse_import_line`, `declared_name`,
`replace_word`), not on IR. Inlined descendant bodies arrive only as strings too, which is why
`with_nested_writes` exists. The failure mode is the usual one for this stage —
plausible-but-wrong JS, or a rename applied inside a string literal or a nested scope — and it
is guarded only by unit tests on hand-written lines (`esm_gen.rs` tests, `codegen/tests.rs`).
If it bites, the structural fix is to make the collision/dedupe decisions on IR before
`generate_esm_module` renders, leaving text passes for formatting only.

## Related open work elsewhere

The transform that would put recovered debug names into codegen output
(`../01_read/unmodeled_regions/PLAN.md` P1b) is blocked in the **analysis** stage's closure
model, not here: `Codegen` already carries injected context via the `with_*` builder pattern, so
the hook exists — what is missing is a `ClosureVar` still intact at print time to key it on. See
`../03_analysis/closure_model/PLAN.md` K4 (carry `ClosureVar` to codegen).
