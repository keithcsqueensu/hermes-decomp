# Impl plan — stamps baked before W6

Status: **proposed (2026-10-30).** Found in IDENTITY_PLAN.md A2c1. Blocks A2c's stable block
ids, and so A2d and A3: an identity that changes number on every rescan is not an identity.

## 1. What was found

A2c1 keys block scopes by creation site, which makes their ids stable: nothing needs renumbering
on a rescan any more. Keeping them stable lost names on Discord (undeclared 6,558 -> 6,651: lodash's
`regex4` and `regex5`, Discord module 12, and `fn7_2`), and renumbering them on every rescan, as
before, restored them (6,520, below A2b's baseline). Stable ids were not wrong. They exposed stamps
that are.

Traced on lodash (Discord F18926, the factory; F81437, `runInContext`):

- F18926's own environment is a block (site @3, builder level 129). Slot 6 holds
  `RegExp(reEscapedHtml.source)` and slot 36 the astral regex; W10 and W11b name them `regex` and
  `regex4` from their readers.
- The owner's binding of a block slot is renamed by name, only when every stamped reference to
  the slot follows (`SlotFollow::owner_binding_if_follows`, the `verified` gate in W10 and W11b).
  A reference inside a class statement never follows (no per-function rename reaches it).
- With stable ids, the class statements inside F81437 hold 90 references stamped
  `(F18926@3, 6)` and `(F18926@3, 36)`: 4 and 86. They print as `closure_6`, and the one checked
  is `new closure_6("Expected a function")`, which is lodash's `TypeError` (F81437's own slot 6),
  not the regex. The stamp is wrong. So the gate refuses, and the factory keeps
  `let closure_36 = RegExp(...)` while its readers say `regex4`.
- Before, the same stamps carried block ids from before W6's rescan, which renumbered every block,
  so they matched no block, and the gate passed by accident.

Where they come from, most likely: F26 (`ir_gen`, closure resolution at IR generation) resolves
a function's whole statement list with that function's closure view, class statements included.
A class built at F11 holds bodies whose levels count from the method, not from the creator, so a
method's level 1 (its creator) read in the creator's view is the creator's level 1 (its parent:
F18926's block). To confirm in E0.

## 2. Stages

- **E0 — census.** Count, per bundle, references whose stamp names a block that a fresh W6 bake
  of the same reference would not: by where the reference sits (class method or constructor
  bodies, a top-level statement, a nested function's inlined body) and by stage (F26, W6). Confirm
  the F26 class-body cause on F81437.
- **E1 — class bodies in their own view.** F26 resolves a class's constructor and method bodies
  with the method's closure view (its function id is on the method), or leaves them unresolved
  for W6, never in the creator's view. Exit: E0's class rows at 0; undeclared not rising.
- **E2 — stable ids.** `reanalyze_all` stops renumbering blocks (the A2c1 code path that keeps
  them: children keep their block; recorded-only blocks survive). Exit: Discord undeclared at or
  below the renumbering run (6,520), Expensify and v96 not rising; `regex4` and `regex5` named.
- **E3 — the gate on stamps.** With stamps right, recheck the class-statement refusal in
  `owner_binding_if_follows`: a class reference that names the slot and is renamed with it should
  follow, not block.

## 3. Not in scope

Shared functions that create themselves (SELF_CREATING_SHARED_PLAN.md); the level cap (A2c).
