# Impl plan — stamps baked before W6

Status: **done (2026-10-30): E0, E2 and E3 landed; E1 was not needed.** Found in
IDENTITY_PLAN.md A2c1. §1 is the diagnosis as first written; §3 is what E0 found, which
reverses it: the early stamps were right, and the renumbering made them wrong.

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

## 3. What E0 found, and what landed

**The stamps were right; renumbering made them wrong.** E0's census
(`stamps::class_body_stamps`, logged with every stamp census under `slots=debug`) checks each
stamped reference in a class method body against the method's chain and against the stamps
the method's own body (baked at W6 in its own view) gives the same slot. On Discord with
stable ids: 46,261 references, 0 off the chain, 0 against the method's own body (v96 8,486,
Expensify 7,866, both 0).

The probe on lodash settled it. With stable ids, `runInContext`'s classes read slot 36 as
`(F18926@3, 36)`, which is on every method's chain and is the astral regex: right. With
renumbering, the same stamp kept the id it was baked with at F26, and after W6's rescan that
number belonged to `F8682@837`, a block of an unrelated function. The class references were
silently pointing into another function's scope; the gate passed because nothing else
referenced that slot. The `closure_6` read as "lodash's `TypeError`" in §1 is
`runInContext`'s own slot 6, correctly stamped (the factory's slot 6 was never involved).

So §1's cause was wrong, and E1 (re-resolving class bodies in the method's view) has nothing
to fix: `fetch_body` already resolves a method with the method's own id.

**What was really refusing `regex4`** is E3's gate. `owner_binding_if_follows` counted every
class-statement reference as one no rename reaches. But codegen prints a method from its
function's whole-program body (`inline_bodies`), renamed with that function, not from the copy
inside the class; the copy is printed only when that body is missing. Its references follow.

**Landed:**
- E2: `reanalyze_all` keeps block ids (no renumbering, no parent edges dropped and restored).
- E3: `class_method_refs` leaves out a method whose function has a body of its own, so its
  copy no longer blocks the owner's rename.
- E0's census stays as a diagnostic.

**Results** (against A2c1, `b3914ba`):

| | lines | undeclared | consts written | strings lost |
|---|---|---|---|---|
| v96 | 213 | 1,485 (=) | 1 (=) | 0 |
| Expensify | 178 | 12,052 -> 12,039 | 1 (=) | 0 |
| Discord | 3,475 | 6,553 -> 6,567 | 0 (=) | 0 |

Discord's identity check falls: I1 2,525 -> 2,503 at the end of naming and 7,524 -> 7,374 at
codegen, I2 8,167 -> 7,962 and 2,407 -> 2,368; I3's undeclared row is unchanged (364).
`regex4` and `regex5` are named. The v96 and Expensify diffs are names the gate used to refuse:
XHR's `const OPENED = 1` where `let c12 = 1` sat beside an undeclared `OPENED`, Sentry's
`allowTouches`, Reanimated's `ERROR_MESSAGES`; dead `let closure_0 = x` aliases go.

**Open: Discord's +14 is one name.** `fn7_2` +38, against 25 better across 11 names. It is
moment.js's inlined `addRegexToken` inside a function with more than 127 blocks (the builder's
level cap, IDENTITY_PLAN.md A2c1: its slots print at level 255). The arrow
reads `fn7_2` and `re1_12` where the block binds `re1_11`. Before, it read `fn146` and `c1_9`
against a binding of `closure_255_0_40`/`closure_255_1_39`: also wrong, but the wrong names were
declared elsewhere. So the region was misread before and is visibly misread now. Its fix is
the capped levels' (the arrow's reads and the block's stores do not reach the same block),
not stamps: A2c2's follow-up.

The plan's E2 exit asked for 6,520 or below, a number measured with renumbering kept on top
of site keys. That exit is not met by 47, and the 47 are the `fn7_2` case above.

**Correction (2026-10-30, A2c2):** `fn7_2` is not the level cap. A repro with no capped level
shows the same shape: a closure reads a parameter's copy under the name the context derived
from the parameter, which the owner's binding refused because the parameter holds it (I1).
It is IDENTITY_PLAN.md A3 step 5a's.

## 4. Not in scope

Shared functions that create themselves (SELF_CREATING_SHARED_PLAN.md); the level cap (A2c).
