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

Exit: the numbers per bundle in this file; the fixture recorded.

**Status: done (2026-10-25).** The scope-graph census line (`RUST_LOG=slots=debug`) now splits
resolved accesses by kind and classes every parking store by the reads through what it parks.
The output is unchanged (Expensify byte-identical; Discord differs by 8 lines where two
`regExpN` names swap, an ordering nondeterminism seen before this change).

| | Expensify | Discord |
|---|---|---|
| slot-held accesses, resolved / unresolved | 6,936 / 18 | 28,558 / 39 |
| up from a slot, resolved / unresolved | 1,797 / 44 | 6,680 / 70 |
| closure accesses unresolved | 2,657 | 2,935 |
| unknown (paths disagree, `GetClosureEnvironment`) | 606 | 3,981 |
| resolved, not on the chain | 1,762 | 1,992 |
| parking stores, every read resolved | 987 | 3,666 |
| parking stores, a read unresolved | 12 | 15 |
| parking stores, never read | 6 | 41 |
| parking-like lines in the output (`closure_N = tmpN` / `= cN`) | 1,768 | 4,475 |

S3.1's condition holds for 99% of parking stores (3,666 of 3,722 on Discord). The 41 never read
can go too. Fixture parked_scopes (v98, v99) is listed known.

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

**Status: done (2026-10-26).** `EnvPlan.drop_stores` carries the parking stores whose every
read resolved, or that nothing reads; the builder emits nothing for them. A function whose plan
only drops stores is rebuilt with shift 0 and keeps its environment offset, so it takes the
unplanned build's path apart from the drops.

| | Expensify | Discord |
|---|---|---|
| undeclared | 25,061 → 24,830 | 18,788 → 17,938 |
| undeclared `tmp` / `tmp2` | 398 / 245 → 200 / 224 | 993 / 219 → 275 / 167 |
| I1 / I2 / I3 | 2,173 / 1,627 / 285 → 2,175 / 1,615 / 356 | 7,593 / 2,772 / 369 → 7,564 / 2,779 / 440 |
| string literals | 511,216 → 515,334 | 835,581 → 843,301 |
| unparsable modules | 0 → 0 | 3 → 4 |

v96 is byte-identical. The literal check found two real problems, both fixed: renumbered
temporaries made the inliner keep a component's `let tmp31 = require` (its keep set is by name,
and a nested raw machine now had a `tmp31` of its own), and the import hoister then hoisted
`tmp31(6286)` to the module's top level; it now hoists a call through a loader alias with the
loader's own name, which is also where the literal total's rise comes from (more calls become
imports). The modules left with fewer literals lose none: a duplicate `new Logger("…")` gone
(it was constructed twice), or a property store folded into a literal or a destructuring.

Left: I3 Unused rises (Expensify 119 → 191, Discord 89 → 160), bare `let`s of wrapper slots that
only ever held an environment; the loads that read one back still exist until dead-code removal.
Dropping those loads needs the facts to say which load offsets bring an environment into a
register. And the fourth unparsable Discord module (`streamKey`) is the pre-existing class of a
raw wrapper declaring a slot under its parameter's name (`action_type = undefined` clobbering the
parameter on the first step), not parking.

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

**Status: done differently; the exit above is not met.** The graph-only route does not work as
written: a block scope's slots are declared where the block is built, which for a machine's
scope is inside the step, the one place it must not be (it would restart every call). Making
the scope the wrapper's therefore still means stamping its slots as the wrapper's own, which is
what the `STEP_ENV_SLOTS` re-stamping does. It stays, with its gaps fixed:
- every body is re-stamped, in one pass over all raw machines, not only the step: a closure the
  step creates with the machine's scope read a placeholder nothing declared (`closure_2` in
  parked_scopes);
- `separate_step_env_names` checks every body that uses the slot, parameters included: the loop
  counter was named `value`, the step's own parameter, which shadowed it inside the step.

What remains is maintained code, not removed: the synthetic slot range, the wrapper's stamped
`let`s and the rename-on-clash step. Replacing them needs declaration placement to know a
parked scope outlives its block (a scope class "lives as long as its parking slot's owner"),
which is a change to `insert_body_declarations` and the identity checker, not to the graph.
Recorded as follow-up **S3.2b**; `COLLAPSED_SLOTS` is the same case.

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

**Investigation (2026-10-26, Expensify, `HD_ENV_DUMP`).** The census now names the first missing
link of each unresolved closure access (`unresolved closure: …`): creators that reach different
scopes 2,274, a function no fact creates 149, a created scope whose parent is unresolved 145,
created with too many scopes 89 (of 2,657). 2,198 of them are level 1, spread over 1,079
functions (the top 10 hold 223), so this is a shape, not a few hot functions. By the shape of
the function's creation sites:

| level-1 accesses | functions | creation sites |
|---|---|---|
| 1,745 (79%) | 749 | several creator functions, each with its own scope (F64569 by F13681 and F13682, adjacent copies) |
| 339 (15%) | 158 | one creator, sibling block scopes on one parent (hermesc emitted the block twice: F0 creates F64502 at @1337, @1365 and @1393, all on @897) |
| 84 (4%) | 39 | the creator's own environment unresolved (a knock-on) |
| 26 (1%) | 17 | one creator, its own scope and a block scope |

Consequences for the items above (corrected 2026-10-27, after trying the first):
- **The "sibling copies" are not one scope downstream.** Each copy gets its own builder level
  (F0's are 131-135), so the closure context does not already merge them. And they are not
  `finally` copies: hermesc *inlined* a small factory at several calls (F0: `stub(m)` for
  `console.error`, `info`, `log`, ...; React's `escape(key)` in `mapIntoArray`; Sentry's MCP
  `wrapMethodHandler` in module 967). A compiled function created on two block scopes of one
  function does prove the two are copies of one source scope, but the copies are **live at
  once**, each with its own values. So this bucket is the same problem as "several creators":
  one shared body, several live environments.
- **Tried and not landed: merging the copies into one scope** (graph union-find over sites
  that create one compiled function, saved outside the tree). On Expensify it took unresolved
  closure accesses 2,657 -> 2,259 and I2 1,615 -> 1,240, but one scope means one variable:
  where the copies share a block, the later copy assigns the earlier copies' binding. F0's six
  console wrappers all read one `let trace = obj2.trace`, so every console method logs at the
  trace level; before, they read an undeclared `error3` and failed loudly. A silent wrong
  answer for a loud one is not a trade to take.
- **Landed: W16g never hoists a function created on a block environment.** Its capture check
  walks only the functions between a site and the declaring scope, never the blocks inside one,
  so it declared the shared closure once above every copy's binding: `escape`'s arrow, F0's
  console wrappers (`error: error3, info: error3, ...`), and, with the merge, module 967's
  wrapper (22 strings and 187 properties lost). `ClosureContext::block_created` holds every
  function some site creates on a block environment; those stay at their sites.
- **What remains is the naming constraint**, for both buckets: one name for slot k across all
  of a shared function's creation scopes, and each copy keeping its own binding (a block per
  copy, or the factory call put back). Fixture `inlined_copies` (KNOWN) is the reference: the
  first inlined copy reuses the function's own environment and the others build block scopes,
  so the inner function reads `closure_1_0`; the object literal is also folded above the stores
  that fill the copies (both pre-S3.3).
- **The naming constraint, as built: a copy of the body per creator, not one name per slot.**
  Forcing one name for slot k across every creator collides wherever a creator already uses
  that name, and copies that share a block would still share one binding. Instead, at render
  preparation (`pipeline/context/shared_bodies.rs`), each creator other than the one the body
  was named against gets a copy of the body, and of the nested functions that read the same
  scope, with the stamped slot references renamed to that creator's names, under a synthetic
  function id (`FIRST_VARIANT_ID`); its creation site points at the copy. Each creator keeps
  its own names and declarations. The creators come from the facts (`shared_creations`: the
  function whose body creates it, and the scope there), never from the scope's owner. A
  creator is skipped, and counted, when it names a slot twice, when its name is a different
  variable in the body, or when the body's name for a slot it never mentions is a different
  variable in the creator. A creator with two scopes (the inlined copies above) is not one
  reading and is left for later. Fixture `shared_creators` passes at 98/99 (it read an
  undeclared `sum` in `left`).
  Measured against the hoister commit: Expensify undeclared 25,795 -> 25,185 (315 creators
  copied), Discord 18,306 -> 17,663 (359 copied; below S3.2's 17,938); no string literal or
  property lost; Discord's four unparsable modules unchanged. Read by hand: Discord's
  `stringifySafe` `replacer` (created by `createStringifySafeWithLimits` and by its inlined
  copy in module 48) now reads module 48's `c1 = 100` where it read an undeclared
  `POSITIVE_INFINITY2`. Left: 341 / 728 creators whose name for a slot they never mention is
  a different variable there (a fresh name would still be undeclared), 359 / 443 with the
  same names already.
- **Copies in one creator (item 1), as built.** A closure now records the environment it was
  created on (`Expression::Function::env`, the builder level: 0 own, past 128 a block it built),
  so each creation site names its scope, and `shared_creations` keeps every scope per creator.
  Before anything reads names, `separate_copy_names` renames creation-scope slots apart: from
  each other within one creator (copies are live at once; one name is one binding), and from
  the shared body's own locals (F0's `warn` copy read the wrapper's own `let warn`). The scope
  the body was named against keeps its names. Then each (creator, scope) gets its copy. Where a
  copy cannot take the creator's name, the slot gets a fresh, undeclared name: loud, where the
  shared body's name would read another scope's binding. A name the creator uses *unstamped* is
  no evidence of a clash: in F66192 the slot's own store had lost its stamp (`const fn = ...`),
  and treating it as another variable made 740 / 901 good reads undeclared; only a name the
  creator stamps as another slot counts (78 / 92). Fixture `inlined_copies` passes at 98/99.
  F0's six console wrappers each read their own level (`error2`, `info2`, `info`, `warn2`,
  `trace2`, `trace`). Against the previous commit: undeclared Expensify 25,185 -> 24,455,
  Discord 17,663 -> 17,048 (both below S3.2); I2 Expensify 1,637 -> 1,346, Discord
  2,786 -> 2,556; no string literal or property lost; Discord's four unparsable modules
  unchanged. I3 Unused rose (192 -> 209, 161 -> 180): read below, the checker's view.
- **Follow-ups to item 1 (2026-10-28).**
  - *A copy's writes and the creator's `const`.* A creator renamed apart for a copy declared
    the slot `const` while its copy assigned it (`const c12_2 = 0` in Expensify's inlined
    `parallel`, whose callback does `c12_2 = 0`): a TypeError when the callback runs, invisible
    to the undeclared count and the parse check. `prepare_render_bodies` declares every body
    before `shared_body_variants` makes the copies, so the creator never saw them.
    `scripts/measure/const_assign.mjs` now counts `const`s the program writes. The copies'
    writes per creator (`Variants::writes`, cached as `copy_writes`) relax the creator's
    `const`s after the copies exist, and feed a module factory's declarations. Consts written:
    Expensify 59 (`c0b19ea`) -> 76 (`bf1cf9c`) -> 36, Discord 264 -> 297 -> 219; every case
    `bf1cf9c` added is gone. The 219 left are older; the largest group (`obj`, 208 writes on
    Discord) is a method's own temporary printed without a declaration, so it assigns a
    module `const` of that name (`obj = { isLooping }` in `timingImpl`): not an S3 cause.
  - *A copy stamps the scope it reads.* `RenameSlots` restamps a renamed reference as the
    copy's creation scope, not the scope the body was named against.
  - *Same creator, unnamed slot.* "A slot the creator never names keeps the body's name" is
    wrong when the creator also holds the scope the body was named against: the body's name
    is that copy's binding in the same function, and a stamp lost by a fold (`const items1 =
    [run, run]`) hid it. Such a slot now gets a fresh, undeclared name (Expensify 128 slots,
    Discord 129): undeclared 24,455 -> 24,612 and 17,048 -> 17,124, each a silent wrong read
    made loud.
  - *The I3 rise was the checker's view.* The identity check saw the bodies before the copies,
    so a creator's declaration read only by its copy counted Unused. It now sees the copies
    (`RenderedBody::copy_of`), each below the body that creates it, with every creator of a
    shared function as its parent; the Shadowed walk ends at the function that builds a block
    scope. On original functions: Unused Expensify 177, Discord 154 (below `c0b19ea`'s 192 and
    161), Shadowed 81 and 738 as before. The copies add their own rows (Shadowed 143 / 204,
    Unused 23 / 157, Undeclared 14 / 5): a copy repeats what its original prints, and the
    output there is unchanged since `c0b19ea` (Discord's `function regex` prints its
    `let closure_2_12;` in every build).
  - *The object literal above its fills (pre-S3.3).* `fold_slot_index_fills` hoisted closures
    into the literal at `NewObjectWithBuffer`, above the stores to the slots they capture. A
    literal holding a closure folded past an environment store now moves down to its last
    fill, when nothing in between is more than a binding the values do not read. `inlined_copies`
    prints the three stores, then the literal. v96 identical.
  - No string literal or property lost (Expensify's module 602 drops a dead duplicate of a
    literal the `return` repeats); Discord's four unparsable modules unchanged.
- **The remainder, counted (S3.3's exit).** Every unresolved closure access by where it sits
  (census `unresolved closure in …`), Expensify / Discord:

  | | in a shared body | elsewhere |
  |---|---|---|
  | creators that reach different scopes | 2,272 / 2,478 | 2 / 2 |
  | a created scope whose parent is unresolved | 114 / 154 | 31 / 14 |
  | a function no fact creates | 0 / 0 | 149 / 189 |
  | created with too many scopes (`TooMany`) | 0 / 0 | 89 / 98 |

  "In a shared body": the scope read is a creation scope of a shared function the access sits
  in. Level-1 reads there are served by the per-creator copies. **Reads two levels up are
  not** (the 114 / 154): the graph cannot give `start`'s scope one parent, so the level model
  stamps the read with an unrelated scope and nothing renames it. Fixture
  `inlined_nested_reads` (KNOWN) records it: the callback reads `global`'s `sum` and `items`
  instead of the copy's `done` and `arr`, a silent wrong answer, and the `done` store is lost
  (the same loss shows in a `shortOut`-shaped case, every build since S3.2). The fix is S3.4's
  explicit unknown (loud), then a copy-aware chain for those reads. `TooMany`, "no fact
  creates" and the 2 elsewhere are unchanged and are S3.3 items 1 and 3.

Exit: every remaining fallback counted and explained; the threshold agreed before this stage.

### S3.4 — Retire the fallback identity (small, last)

Once S3.3 leaves only counted exceptions, remove the `NESTED_ENV_LEVEL_BASE + slot` identity and
`reg_source_slot` from `EnvRegMap`: an access the graph cannot resolve becomes an explicit
`Unknown` (a counted placeholder), not a fake block scope. The census reports by graph result
only.

Exit: v96 byte-identical, v98 measured, gates.

**Status (2026-10-28): the closure part done, the slot-held part not started.** What was built
covers the closure accesses S3.3 counted; `NESTED_ENV_LEVEL_BASE + slot` and `reg_source_slot`
(slot-held and up-from-a-slot accesses: 18 / 39 and 44 / 70 unresolved) remain.

- **A chain answer for unresolved closure accesses.** A shared function gets its first
  creation scope as its parent in `scope_parents`, so the installed chain is complete through
  it. An access the graph left unresolved is then read on that chain where the reading holds
  for every creator: the shared function's creation scope itself is renamed per copy (S3.3),
  and a scope past it must be the same from every creation scope. This is what the two-level
  reads in `inlined_nested_reads` needed (the callback read `global`'s own slots through
  `add_child`'s link): it passes now. Expensify / Discord: 2,146 / 2,385 accesses read on the
  chain, all verified by `verify_env_plans`.
- **An explicit unknown only on evidence.** Where the creation scopes disagree past the shared
  function, the access gets `UNKNOWN_ENV_LEVEL` and prints `closure_unknown_{slot}`
  (undeclared, a placeholder to `undeclared.mjs`): 54 / 92. Where the chain merely breaks (a
  link the graph lacks: "no fact creates", "parent unresolved"), the level model stays: 457 /
  458. Making those unknown too was tried: undeclared rose by 2,000 / 5,700, since the lexical
  parent the level model walks is usually right.
- **A shared function that creates itself keeps no parent** (29 / 53; ts-pattern's `optional`,
  whose inlined `chainable` creates it again). It is declared once per creator, and one parent
  moved every declaration to the representative's scope (Discord module 5745 printed one
  `function optional()` for 32 creators). Fixture `inlined_recursive` (KNOWN) records that a
  single declaration cannot read each creation's `pattern`.
- **Shared functions stay at their sites** (`hoist_repeated_closures`, with the block-created
  ones): with a parent installed, the hoister declared `GameSearchSession`'s methods once at
  module level, reading the constructor's locals out of scope.
- **The lost store into a slot** (every build since S3.2, v96 too). The register holding a new
  value is named after the slot it is stored into, so `slot = value` printed `x = x` and
  `cleanup_noise` dropped it; a later pass then took the remaining unstamped write for a
  local (`const sum2 = sum + 1`) and the store was gone. Now, when the target is the slot and
  the value an unstamped local the body writes, the drop stamps that local's references as the
  slot (`stamp_merged`): one variable, as printed. A value the body only reads (a captured
  parameter) is dropped as before. Fixture `inlined_counter` passes (it printed `a1 a2 a3`).
  On v96 this restores Babel's self-replacing helpers (`exports = getPrototypeOf.bind()` where
  a separate `exports2` was written): 7,448 changed lines, so the v96 exit is not met by
  design; undeclared 6,169 -> 6,159.
- **A capture and a local under one name in one body** (`split_capture_and_local`): the
  unstamped references of a name the body reads by stamp from an ancestor, writes first and
  never writes by stamp take a fresh name. And a name the body writes first is never "outer"
  by name alone (`written_first_vars`).
- Against `d15c5fa`: undeclared Expensify 24,612 -> 24,065, Discord 17,124 -> 16,129;
  `const`s written 36 -> 34, 219 -> 195 (most of Discord's are still `obj`: a temporary
  undeclared in a raw state machine that the split does not reach). String literals and
  properties up in total (Expensify 519,088 -> 519,083 / 931,990 -> 932,010; Discord 844,953
  -> 845,009 / 1,280,125 -> 1,280,618); the modules with fewer drop dead temporaries
  (`let str6 = "0";` in CFB, `let str = "complete";` in `TTFBThresholds`) or a literal printed
  twice (module 407's `tmp = {...}; let obj = {...}`). I2 CaptureAndLocal Expensify 310 ->
  2,981: the merged stamps on Babel's `_interopRequireWildcard`, whose output is unchanged.
  Discord's unparsable modules 4 -> 5: module 7149 joins the known class (a raw wrapper's slot
  named like its parameter, `let orderId;`); before, the slot was renamed `orderId5` and never
  assigned.

**`TooMany` (S3.3 item 1), done by raising the cap (2026-10-28).** A creation set over 16
scopes was not followed, so the function fell to the level model: one parent link, one
creator's names in every rendering. hermesc inlines small helpers into every caller, so the
sets are large: Discord's `dispatchOrResetSocket` callback has 187 creators, and every
rendering read `obj.type` (another creator's slot 0, here the module's `const obj =
Dispatcher`) and assigned that `const` (171 of the 178 `obj` writes `const_assign.mjs`
counted). `up_all` already collapses a set whose parents agree, so `MAX_SET` is now 1024 and
such a function is an ordinary shared function, one copy per creator. The bucket is gone on
both bundles (89 / 98 accesses). Against the S3.4 commit: undeclared Expensify 24,065 ->
22,832, Discord 16,129 -> 15,688; `const`s written Discord 195 -> 41 (`obj` 178 -> 7);
equinox unchanged; run time unchanged (715 s / 1,386 s); one dead duplicate string dropped.
Expensify's remaining 113 `sum` writes were there at `d15c5fa` (React DevTools' `getID`
counter in module 122).

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
