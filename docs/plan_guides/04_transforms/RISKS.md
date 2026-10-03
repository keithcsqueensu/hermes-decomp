# Transforms + codegen — risk register

> **Ownership.** *Owns* the risk that the transform/codegen phase mis-rewrites the IR or emits
> wrong JavaScript. *Delegates* the phase's *description* — the F/W stage catalogue, the pass
> families, the `Codegen` `with_*` context — to `../../arch_guides/04_TRANSFORMS_CODEGEN.md`.

Status: **no open findings.** No robustness finding from the read hardening pass landed on this
stage, and no version-drift hazard of the write path's kind lives here. This register exists as
the stage's vertebra on the spine; the notes below are the standing hazards to respect when
adding a pass, not open defects.

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

## Related open work elsewhere

The transform that would put recovered debug names into codegen output
(`../01_read/unmodeled_regions/PLAN.md` P1b) is blocked in the **analysis** stage's closure
model, not here: `Codegen` already carries injected context via the `with_*` builder pattern, so
the hook exists — what is missing is a `ClosureVar` still intact at print time to key it on. See
`../03_analysis/closure_model/PLAN.md` K4 (carry `ClosureVar` to codegen).
