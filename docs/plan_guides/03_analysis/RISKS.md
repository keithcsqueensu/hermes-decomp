# Analysis — risk register

> **Ownership.** *Owns* the risk that the analysis phase (closures, Metro, IPA, naming,
> structure) produces facts that are silently wrong. *Delegates* the analysis phase's
> *description* to `../../arch_guides/03_ANALYSIS.md`, and the one substantial open body of
> work — the decompiler's closure / env-slot model — to [`closure_model/PLAN.md`](closure_model/PLAN.md).

Status: **no open robustness findings landed on this stage** in the read hardening pass. The
analysis layer's fixed-point iterations are all bounded (`MAX_PARAM_LINK_ITERATIONS`,
`MAX_MODULE_NAME_ITERATIONS`, `MAX_REEXPORT_ITERATIONS`, `MAX_PARENT_CHAIN_DEPTH`,
`MAX_WRAPPER_CHAIN_DEPTH`, `MAX_INLINE_BODY_PASSES`, `MAX_ASYNC_PROPAGATION_ITERATIONS`) — the
read register (`../01_read/RISKS.md` § What is fine) retired the "is the analysis layer bounded"
worry by measurement; it is only the IR tree-walk that was not, which is F9 in `../02_ir/RISKS.md`.

---

## Open work

The one open item is not a *robustness* risk but a *model* one, and it is fully scoped in its
own plan:

- **The closure / env-slot model** → [`closure_model/PLAN.md`](closure_model/PLAN.md). Findings
  K1–K4 (specified, none shipped). In one sentence: the slot→name map is consumed as a
  register→name map (C1), two spellings of the same slot exist (C2), tens of thousands of
  placeholders are excluded from naming by their spelling (C3), and `ClosureSlotValue` has no
  rung for a better source of truth (C6) — so a name Hermes itself recorded cannot outrank one
  inferred from a store. This is what `../01_read/unmodeled_regions/PLAN.md` P1b (put recovered
  debug names in the decompiler) is blocked on. See the plan for the measurements and the fix.

## The load-bearing spelling

One design fact worth stating here because it couples analysis to naming and to Metro: the
`closure_N` / `c{slot}` placeholder spelling is **load-bearing**. `resolve_closures` and Metro's
generic-name rejection both key on that family, and name-voting deliberately rejects it as
generic. Break the prefix convention and placeholders leak into module/param names. Restructuring
that coupling is exactly what `closure_model/PLAN.md` K3 (`SlotName { text, source }`) exists to
do — see `../../arch_guides/03_ANALYSIS.md` § Notable design decisions.
