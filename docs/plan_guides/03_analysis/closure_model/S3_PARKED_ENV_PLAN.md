# Impl plan — S3, environments parked in slots (finishing it)

Status: **proposed (2026-10-25).** Follows V98_ENV_PLAN.md (S3 in §1; E2–E4 in §4) and
OUTPUT_DEFECTS_PLAN.md §9.

## 1. Where S3 stands

S3 is the v98 shape where a function stores an environment in a slot and a later run loads it
back. A raw generator machine does it on its first step:

```
GetParentEnvironment r5, 1          ; the enclosing function's scope (`this`, parameters)
StoreToEnvironment   r1, 1, r5      ; parked in the wrapper's slot 1
CreateTopLevelEnvironment r8, 1     ; the machine's own variables
StoreToEnvironment   r1, 0, r8      ; parked in the wrapper's slot 0
…later steps…
LoadFromEnvironment  r5, r1, 0      ; the machine's scope, back out
StoreToEnvironment   r5, 0, r0      ; one of its variables
```

**Resolving accesses is done.** E2 records the parking as facts, E3's scope graph resolves a
load through a parked environment to the scope that was parked, and E4 addresses each resolved
access by the level that reaches that scope. On Discord the graph resolves 28,562 of 28,601
slot-held accesses and 6,714 of 6,784 up-from-a-slot accesses (V98_ENV_PLAN §4 E3).

The `slot-held env accesses` figure in the identity log (Discord 36,693) counts every access
through a slot-held environment *before* the plan remaps it. It measures how common the shape
is, not how many accesses fail. OUTPUT_DEFECTS_PLAN §9 read it as "unresolved"; it is not.

**What is left is in the output.**
- **The parking is printed.** An environment has no JavaScript form: the register holding one is
  never defined, so `StoreToEnvironment r1, 0, r8` prints as `closure_0$2 = tmp`. Discord: 3,522
  such lines (`closure_N = tmpN` 1,500, `closure_N = tmp` 682, `closure_N$N = tmpN` 681,
  `closure_N$N = tmp` 659), plus 556 `closure_N = cN` where the register held the parent scope.
  They are most of the 780 undeclared `tmp`/`tmp2` references inside raw machines, and each
  declares a pointless variable in the wrapper.
- **The machine's own scope is named and declared by a patch.** `keep_raw_machines` re-stamps the
  machine's variables into a synthetic slot range of the wrapper (`STEP_ENV_SLOTS`), inserts
  stamped `let`s for them, and `separate_step_env_names` renames them off clashes afterwards.
  3 Discord modules still do not parse because of it (a wrapper slot named like its parameter).
- **A small unresolved remainder,** and the level model as fallback for it: 39 slot-held and 70
  up-from-a-slot accesses, 2,935 closure accesses whose creators reach different scopes (or more
  than 16 of them, `TooMany`), and 3,981 `unknown` (paths that disagree, `GetClosureEnvironment`).
  A fallback access still names a parked scope `NESTED_ENV_LEVEL_BASE + slot`, a fake block scope
  keyed by slot number alone.

**History to respect.** Suppressing the parking store in the builder was tried before E3 and
removed 64 string literals and 629 property accesses from the reference bundle: later passes
read those accesses through the parked slot's fake block scope. Today the graph addresses them
directly, so the store is no longer anyone's link, but only where the graph resolved every
access through the parked environment. Stage 1 is built around that condition.

## 2. Stages

Each stage is measured like a K4 stage: v96 byte-identical, v98 I1/I2/I3, undeclared references
per module (with the `worse:` names classified), E1, gates.

### S3.0 — Measure what is left (small)

- Split the census: slot-held and up-from-a-slot accesses **resolved by the plan** against those
  that **fell back** to the level model, per bundle, in the identity log. The fallback count is
  what S3.3 must bring down.
- Count parking statements: IR stores whose value is an environment (from E2's `parked` facts),
  and loads that bring one into a register, both at codegen.
- Add an E1 fixture, `parked_scopes` (v98, v99): an async method that stays raw (an `await` in a
  loop), whose later state creates a closure reading both the method's `this` and a loop
  variable, and a generator doing the same. It records the case end to end; listed known until
  S3.1–S3.2 make it pass.

Exit: the four numbers per bundle in this file (resolved, fell back, parking stores, parking
loads); the fixture recorded.

### S3.1 — Stop printing the parking (medium; the main win)

- In the builder: `StoreToEnvironment E, k, r` where `r` holds an environment value emits
  nothing, and `LoadFromEnvironment r, E, k` that loads an environment only sets `r`'s
  environment, both **when every access through that parked environment is resolved by the
  plan**. Otherwise keep today's emission, so a fallback access keeps its link.
- `drop_stores_from_undefined_sources` (W16b) is the after-the-fact version of the same thing
  for lifted machines. Keep it until the measurement shows it no longer fires, then remove it.

Measure: Discord's 3,522 + 556 parking lines and the 780 undeclared `tmp` inside raw machines
→ near 0; **string literals and property accesses per module must not fall** (the earlier
attempt's failure, now a check); I3; the wrappers' pointless declarations gone.

Exit: parking gone wherever the graph resolved it, no literal or access lost, gates.

### S3.2 — The machine's scope from the graph (medium)

- The scope a machine creates and parks is a node of the scope graph. Make its function the
  **wrapper** (the scope lives as long as the generator, which the wrapper creates), so its
  slots are the wrapper's variables through ordinary stamps, naming and declarations.
- Then remove `keep_raw_machines`' re-stamping (`STEP_ENV_SLOTS`), its stamped `let`s, and
  `separate_step_env_names`. The collapsed-machine re-stamping (`COLLAPSED_SLOTS`) gets the same
  look: a collapsed machine's function scope belongs to the wrapper too.
- The 3 unparsable Discord modules are this case; they must parse.

Measure: raw_generators, raw_async and parked_scopes pass; unparsable 0 on all bundles; I1–I3
on Expensify and Discord (the machine variables' naming moves to the normal path).

Exit: no synthetic slot ranges left for machines; gates.

### S3.3 — Resolve the remainder (medium, open-ended; do by size)

From V98_ENV_PLAN §4 E4's follow-ups and S3.0's split:
1. **`TooMany`** (more than 16 creation scopes): collapse a creation set whose parents agree
   (`ScopeGraph::up_all`) instead of giving up on its size. Expensify's F72596 (32 copies of
   one object-literal factory, all with the same parent) is the reference case.
2. **Creators that disagree** (Discord 2,480): resolve an access when every creator's chain
   reaches the same scope *at that access's level*, even when the chains differ below it.
3. **`unknown`** (3,981): split `GetClosureEnvironment` from disagreeing paths; merge paths
   whose values agree on the scope actually accessed.
4. The 39 + 70 unresolved slot-held and up-from-a-slot accesses: read by hand and classify.

Measure each: the fallback count from S3.0, I3 Undeclared, undeclared placeholders (Discord
3,780).

Exit: every remaining fallback counted and explained; the threshold agreed before this stage.

### S3.4 — Retire the fallback identity (small, last)

Once S3.3 leaves only counted exceptions, remove the `NESTED_ENV_LEVEL_BASE + slot` identity and
`reg_source_slot` from `EnvRegMap`: an access the graph cannot resolve becomes an explicit
`Unknown` (a counted placeholder), not a fake block scope. The census reports by graph result
only.

Exit: v96 byte-identical, v98 measured, gates.

## 3. Order and size

S3.0 → S3.1 → S3.2 → S3.3 → S3.4. S3.1 carries most of the visible output gain (the parking
lines and the raw-machine `tmp`s) and is safe only because of the resolved-only condition. S3.2
replaces code written in D3.1 as a patch with the graph's own answer. S3.3 is the open-ended
one; take its items by measured size and stop where the remainder is small and understood.
S3.4 depends on S3.3.

## 4. Risks

- **The resolved-only condition is the whole safety argument of S3.1.** If it is too loose,
  the earlier regression (strings and accesses lost) returns. That is why S3.1 measures them.
- **Shared functions.** One compiled function created from several scopes (981 on Expensify,
  1,326 on Discord) can park different environments per creator. E3 found 0 slot conflicts,
  but S3.1 should re-check that per parked slot before dropping its store.
- **S3.2 changes naming for every machine variable** on v98 bundles. Expect a large diff; judge
  it with I1–I3 and the raw fixtures, not by line count.
