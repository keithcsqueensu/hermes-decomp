# Impl plan — A, scope and variable identity

Status: **proposed (2026-10-28).** Follows S3_PARKED_ENV_PLAN.md (S3.3, S3.4 and the
follow-ups recorded there) and K4_PLAN.md. Plan B (putting hermesc's inlined helpers back as
functions, INLINED_HELPERS_PLAN.md) depends on stage 3 of this plan.

## 1. Where things stand

Two things are not first-class in the pipeline, and most of the defects S3.3 and S3.4 fixed
were one or the other missing.

**A variable is its spelling.** `Var { name, slot }` compares, orders and hashes by `name`
alone (`ir/types.rs`). The `slot` stamp is optional, set at W6 for environment slots, and
passes drop it. Locals and registers have no identity past their name at all. So "is this
the same variable?" is answered by string equality, and it is wrong in both directions:

- **Two variables under one name.** A register holding a new value is named after the slot it
  is stored into, so `slot = value` printed `x = x` and `cleanup_noise` dropped the store
  (every build since S3.2, v96 too; `inlined_counter`). A capture and a body's own temporary
  share `obj` (`obj.easingX` beside `obj = { initialValues }`). An async wrapper's parameter
  and the slot it is copied into share `url`, and the slot's `let` redeclared the parameter.
  Discord's `_fireSurveyAction` names its response slot `action_type` after the key it is sent
  under, the parameter's name; once the copy `action_type = action_type` is dropped, nothing
  in the IR says which slot was the copy, so the module stays unparsable.
- **One variable under two names.** A shared function's copies needed explicit renaming per
  creator (`separate_copy_names`, `shared_body_variants`); a slot read under one name in the
  creator and another in a closure is I1.
- **Decisions keyed by name.** The declaration pass counts writes per name across nested
  bodies (`extra_writes_from_nested_bodies`), so a child's own local counts against a parent's
  variable of that spelling (`let X = X_mod` aliases on v96); a `const` is chosen before the
  copies that write it exist (`relax_written_consts` patches it afterwards).

**An environment access is a level, not a scope.** The builder lowers `LoadFromEnvironment`
and `StoreToEnvironment` to `ClosureVar { level, slot }`; the planner (E4, S3.4) remaps levels
so that a walk up `parent_function` (`ancestor_at`, `slot_owner`) lands on the scope the graph
resolved. The level is an indirection the graph no longer needs, and it leaks:

- **A fake scope keyed by slot number.** A register loaded out of a slot addresses
  `NESTED_ENV_LEVEL_BASE + slot` (`reg_source_slot`), one block scope per slot number in the
  function, shared by every environment parked in a slot of that number. Read by hand in S3.4,
  its answers collided with unrelated locals (Expensify's Sentry wrapper called the wrapped
  function where the bytecode reads `require(dependencyMap[10])`).
- **The chain is also the tree.** To answer reads two levels below a shared function, S3.4
  installs a representative parent in `scope_parents`, which `add_child` turns into
  `parent_function`, which also decides where named functions are declared and what the
  closure hoister may move. Hence the exceptions: shared functions never hoisted, a function
  that creates itself keeps no parent (ts-pattern's `optional`).
- **Raw machines are re-stamped by hand.** `keep_raw_machines` moves a machine's scope into a
  synthetic slot range of the wrapper (`STEP_ENV_SLOTS`), inserts stamped `let`s, and
  `separate_step_env_names` renames them off clashes. It cannot tell one variable reached
  through two identities from two variables under one name, so it refuses to split either.

**Patches this pays for.** Each of these exists because identity is missing and goes away
when it is not: `separate_copy_names`, the rename half of `shared_body_variants`,
`copy_writes` / `relax_written_consts`, `stamp_merged` in `cleanup_noise`,
`split_capture_and_local`, `written_first_vars` in the outer set, `drop_slot_lets_of_params`,
`separate_step_env_names`, the `STEP_ENV_SLOTS` re-stamping, `NESTED_ENV_LEVEL_BASE + slot`,
`reg_source_slot`, the shared-parent install and its self-creating exception, and the
hoister's `stay_at_sites`.

**Size.** `ClosureVar` appears in 49 places in 22 files, `NESTED_ENV_LEVEL_BASE` in 23 in 8,
`.slot` stamps in 78 places, `Binding::Variable` in 135 files.

## 2. Goal and non-goals

Goal: every environment access names its scope, and every variable has an identity from the
moment it exists to codegen; names are chosen once, at the end, from identities.

Not in this plan: putting inlined helpers back (Plan B); output defects with other causes
(structuring, class reconstruction, the KNOWN `other:` fixtures); performance work beyond
keeping run time flat.

## 3. Prerequisite: an oracle that sees wrong values

Every stage below changes output across whole bundles, and the checks that would catch a
silent wrong value are the env fixtures only; undeclared counts and parse checks do not see a
value that is wrong but defined (`const_assign.mjs` had to be written to see one class). Before
stage 1:

- **Ratchets for what is now measured by hand:** `const_assign.mjs` (consts written) and the
  per-bundle parse count, beside `slot_identity_baseline.tsv`, failing on the way up.
- **One env fixture per defect class S3.3 and S3.4 met,** compiled with hermesc and run against
  hvm: shared bodies (`shared_creators`, `inlined_copies`, `inlined_nested_reads`,
  `inlined_counter`, `inlined_recursive` exist), plus a parameter copied into a same-named
  slot, a response slot named like a parameter, a capture beside a same-named local, a
  closure on a block environment writing its owner's variable.
- **Scope to settle at the start:** whether selected bundle modules with no native
  dependencies can run under node against the same module under hvm. Worth a day's look; not
  a blocker.

**Status (2026-10-28).** The first two are in place:
- `scripts/build/output_ratchets.sh` with ceilings in `output_baseline.tsv`, run by `gates.sh`
  after the parse check: the `quick` row (equinox, about 80 s) every run, the `large` rows
  (Expensify, Discord) with `--bundles`. Ceilings at S3.4's close: equinox 0 unparsable / 1
  const written, Expensify 0 / 1, Discord 1 / 0.
- Fixtures `capture_local`, `block_writer`, `param_slot` (pass: they guard the S3.4 fixes) and
  `loop_reassign` (KNOWN `other:`: a `for ... of` whose body reassigns its variable is not
  recovered, the loop never advances).

The third is built: `scripts/measure/module_oracle.py` (2026-10-29). hvm runs one file, so the
bundle is made to run one module: the global function's closing `__r(startup)` /
`__r(entry)` calls are replaced, through `hermes-decomp asm`, by `m = __r(N); throw typeof m +
"|" + Object.keys(m).join(",")` (thrown, because the bundle's globals leave neither `print` nor
a working `JSON.stringify`; property reads by `GetByVal`, since a reused property-cache index
answers with another property's slot). The decompiled side is `hermes-decomp extract`, each
import pointed at its module's file (by `/* id */`, else by the module's name), run under node
with `module`-less ES semantics; a module without a default export gets its own namespace as
one, Metro's `require` giving the exports. The snapshot is the type of the exports and their
keys; values are not compared yet (serializing them calls the getters Babel's interop defines
exports with, and one that threw broke the snapshot).

Getting it to run found two defects outside S3, both fixed with it:
- **Exception tables of large legacy functions were dropped.** An overflowed v96 header's
  `info_offset` points at the large header itself and the FunctionInfo follows it; the parser
  read the large header's fields as the table, got a count in the millions and dropped it
  silently, so a shipped v96 bundle's global function lost its handler (and its debug-info
  offset), and `asm` refused it. `FunctionHeader::info_start`, used by the parser and the
  writer.
- **Babel's `interopRequireDefault` module printed empty.** The ESM classifier skipped any
  assignment of a function named like `*interop*`, meant for a module's local helper, also
  when it was the module's export (`module.exports = function _interopRequireDefault`).
  Nearly every module calls it (`module_1(_mod368)`) where the import side keeps the call, so
  the output called a function it never defined. An export is never skipped now.

First run, Equinox (v96), 200 modules at random (seed 2): 92 not runnable under hvm (they reach
native modules), 108 runnable. Of those, 53 match. Findings: 20 still CommonJS (`exports`,
`module.exports`, `require(n)` left in ES output), 9 do not parse, 5 import a module by a name
the extract does not carry, 1 exports something else (module 1556: a function under hvm, `{}`
decompiled), and the rest throw at load: a method called without its receiver (`const
forEach = items.forEach; forEach(cb)`, modules 930, 1008, 1214), a module variable declared
and never assigned (930's `obj` for `originalConsoleMethods`), a local interop helper skipped
while its call stays (`_interopRequireDefault is not defined`, 1867), and undefined values
(`cache.create`, `.EOF`). Modules with JSX (`Unexpected token '<'`) are counted apart: node
runs them only through a transform the harness does not have yet. Next for the harness: a JSX
transform, value snapshots that read property descriptors instead of calling getters, and
v98 bundles (the same patch, `hvm` v98).

Second pass (2026-10-29), three findings fixed:
- **A method read apart from its call kept its receiver only when the receiver was the read's
  object itself.** Read once above a branch, the receiver is a copy (`tmp = items; forEach =
  items.forEach; forEach(tmp, cb)`), and the call printed without it, as did Babel's private
  field helpers (`set(self, obj)` for `weakMap.set`). `keep_method_receivers` looks through
  once-assigned copies: `forEach.call(items, cb)`, `set.call(weakMap, self, obj)`.
- **A slot named from an object key was renamed in its owner only** (W10,
  `apply_object_key_literals`): `exports.originalConsoleMethods = obj` made the owner's
  binding `originalConsoleMethods` while the closures reading the slot kept `obj`, declared by
  nobody but `let obj;`. Each reader is renamed with it, by stamp where the reader has a local
  of the same name; when one cannot follow (a class method, a reader that reads the name
  unstamped without writing it, or the key already used on the way) the slot keeps its name
  everywhere. Equinox: 76 refusals by a name in use, 15 by class methods.
- **A local interop helper was dropped while a call to it stayed** (graphql-js's
  `_interopRequireDefault(devAssert2)`, the import already taken apart): the definition is kept
  when the module still names it. Undeclared on Equinox 3,361 -> 1,827.

Run on the same sample: decompiled throws 7 -> 3. The four modules fixed now stop in a
dependency, on defects found by going further: a class whose `prototype` is assigned
(`TokenStream.prototype = ...`, read-only on a class, module 4940), and Babel's `toPrimitive`
calling `obj.default` on an interop import (module 12). Also seen, in both versions:
`startTransition`'s `ReactSharedInternals.T = currentTransition` folded into the new object's
literal (`obj2 = { T: obj2 }`).

## 4. Stages

Each stage is measured like an S3 stage: v96 byte-identical unless the stage says otherwise
(and then every changed class read), v98 I1/I2/I3 and the census, undeclared and placeholder
counts, consts written, unparsable modules, string literals and properties per module (no
module loses one without a reason read), env fixtures, gates, run time.

### A0 — A checker that can be trusted (small)

The identity checker is the ratchet every later stage is judged by, and it has known false
positives: a named function expression's own name counts as a local beside the capture
(`ClosureContext::declared_function_names` includes expressions), which is 1,740 of v96's 2,284
I2 rows (`_getRequireWildcardCache`, `_isNativeReflectConstruct`). Only declarations bind in
the enclosing scope. Fix it, re-baseline the ceilings, and add the two ratchets from §3.

Exit: ceilings re-recorded with each change explained; no output change.

### A1 — The builder addresses scopes (medium)

The plan already knows the scope of every resolved access (`expected` in `plan_env_levels`)
and verifies it (`verify_env_plans`). Carry that scope to the builder instead of a level:
`EnvPlan` maps an access to a `ScopeRef` (or to unknown), and the builder emits a slot binding
that names the scope (`Binding::Slot { scope, slot }`, or `ClosureVar` with an absolute scope
id). W6 stamps it directly; no walk. Moving a statement into another body no longer shifts
levels for such a binding. Accesses with no plan (no closure context: single-function runs,
the CLI's `--function`) keep the level path as it is.

Exit: the same answers as today, so v96 and v98 byte-identical; the census counts accesses
addressed by scope against accesses still by level (only the planless path).

**Sized at the start (2026-10-29): larger than "medium".** A `ClosureVar`'s name does not come
from its scope: the closure context builds a `ClosureInfo` per function keyed by
`encode_level_slot(level, slot)` (W1, `context/walk.rs`, `info/analyze.rs`), the resolver names
an access from that table and only then asks `slot_owner(func, level)` for the stamp, and nine
passes call `slot_owner` on whatever `ClosureVar` they meet (`var_naming/*`, metro propagation,
`ir_gen`). Today every planned access already resolves to its planned scope through the
installed chain (`verify_env_plans` checks each one), so the indirection costs nothing in
output; what A1 buys is structural (statements move between bodies without level shifts, and
A2 can drop the level machinery). Split it into steps that each leave the output
byte-identical, so a step that changes one byte has found something:
- **A1a1, the plan records scopes.** `ClosureContext::access_scopes` keeps the scope id of each
  access the plan places (resolved and chain-answered; unknown stays unknown), by function and
  offset. Not in `EnvPlan`: a plan makes its function rebuild, and most placed accesses keep
  their level. Data only; the census counts each against what its level reaches through
  `slot_owner`.
- **A1a2, the builder emits the scope.** `Binding::ClosureVar` gains `scope: Option<u32>`, set
  for a planned access (the level kept for naming), `None` on the planless path. A field rather
  than a second variant: patterns written with `..` keep working, and only the dozen
  exhaustive ones change.
- **A1a3, W6 stamps from the scope.** The resolver stamps a scoped `ClosureVar` directly and
  counts every disagreement with `slot_owner(func, level)`; the count must be 0 on v96 and v98
  before the walk's answer is dropped for planned accesses.
- **A1b1, names by scope.** `function_closures` is already keyed by scope (a function, or a
  block scope's id); the level-keyed table is only the view `get_closure_info_for` builds per
  reader. So no second table: W6 computes each scoped access's name from its scope's own
  entry and the census compares it with the level view's.
- **A1b2, naming per scope.** Names are taken from the scope's entry for planned accesses,
  once A1b1 shows no difference.
- **A1b3, the `slot_owner` callers read the stamp,** one pass per commit (`capture_sync`,
  `ancestor_inherit` and `resolve_baked` through a recorded owner per baked name; metro
  propagation and `closure_definitions` parse placeholders and stay on the planless path).

Done (2026-10-29), A1a1 to A1b1:
- `access_scopes` holds a `ScopeRef`, not an id. Block-scope ids are handed out again on each
  reanalysis (`reanalyze_all` clears `block_scopes`), so an id taken at plan time named the
  neighbouring block by W6: 122,992 stamps disagreed on Expensify until the tag carried
  (function, builder level) and W6 looked the id up.
- `ScopeTag` compares, hashes and orders as equal, so a tagged and an untagged access to one
  slot stay one binding to every pass that keys on bindings.
- Census, W6 stamps by scope: Equinox 406,015 of 406,015 agree with the walk; Expensify
  758,462 agree, 20 disagree (2,178 unplaced); Discord 1,306,402 agree, 92
  disagree (9,050 unplaced). Names by scope: Equinox none differ, Expensify 15, Discord 85. Every disagreement is the chain changing after the plan:
  a body left with no parent (Expensify F98051), a block scope that a generator's collapse
  rehomed, a parent relinked (F107281, scope 69709 against the walk's 1238). The walk wins
  where they disagree, so the output is byte-identical on all three; those rows are A2's to
  remove, and A1b2 waits for them.
- Found on the way: v98 output is not deterministic. Two runs of one binary differ on
  Expensify inside its largest `styles` function (the numbering of its object literals,
  `obj670` against `obj285`, and the order of a few statements), and on Discord in which of
  two locals of one function gets which number (`items12`/`items13`, elsewhere
  `regExp12`/`regExp13`), a different function each run. Fixed the same day: the cause was
  `separate_locals_from_captures`, which handed out fresh names to a body's clashing locals
  in hash-map order, and two of them can want one name (`obj` tries `obj285` as `obj2` tries
  `obj2`+`85`). In name order now; three runs of each bundle agree byte for byte, and each
  function that used to flip settles on one of its two forms. v96 unchanged.

A1b3 (2026-10-29): the bake records, per baked name, the owner W6 stamped it with
(`baked_owners`); `resolve_baked`, `capture_sync` and `ancestor_inherit` read it through
`ClosureContext::baked_owner`, which counts it against the walk at the time of reading. Equinox
180,628 agree, 0 disagree; Expensify 346,155 and 112; Discord 620,766 and 559. Those
disagreements are the chain moving after W6 (a generator's collapse relinking its step's
scopes): read from the record, they kept placeholders where the late walk found the name
(`closure_6` for `byteLength`), so the walk keeps them too until A2, and the output is
byte-identical (Expensify up to the `styles` flip above, since fixed).

Found tracing those rows (2026-10-29): most W6 stamps the walk could not place were in
closures whose creating code the output had lost. A loop whose header is a switch (a token
loop over v98's `StringSwitchImm`, an async state machine dispatching on its state, React's
rotated child walk) was structured as its header block alone, and everything only the switch
reached was dropped (graphql's `parseNumberSkeleton`: 2 statements of 464 instructions).
Such a header is now `label: while (true) { header; switch }`, its exit taken from the latch;
and every switch's cases leave the block they converge on to after the switch, where the case
recovered first used to absorb it and the others ended without it, or carry a copy. Expensify
+26,893 lines, Discord +8,984, strings +2,844 and +902; no module loses a string literal
(Orphan Functions aside) and the property drops read are copies folded into one tail.

A third, the same way: class reconstruction analyzed nested statement lists with the state of
the enclosing list, which it keys by statement index. A nested statement whose index the
enclosing list's class had consumed was dropped, and that class declared again in its place
(lodash's `runInContext` lost its `lodash.prototype[name] = function` installs to `class
Hash`, `ListCache` and `MapCache` inside the loop that makes them; Discord's lodash module
held 36 `class lodash`, moment's 354 `class hooks`). Each nested list is analyzed on its own
now, and a class whose constructor the list does not define is not made (it only declared the
name again, `lodash.range = function` becoming `class lodash { static range }` in a block).
Discord -403,687 lines, Expensify -9,688, v96 -12,557, every one a copy: no module loses a
distinct string literal.

Not fixed yet, found the same way: a loop whose body holds a `try/catch` that rejoins it is
not found at all (React's `while (child !== null) { switch (child.tag) { ... try ... } }`
prints as an `if`, event-target-shim's listener walk loses its loop). `compute_dominators`
makes every catch block a root, so the catch is a way into the latch around the header and
the back edge is never seen. Giving a catch its protected blocks as predecessors finds those
loops (v96 +34 `while (true)`, Discord +1,074, Discord's last unparsable module parses), but
v98 generator reconstruction expects the machine's dispatch not to be a loop and gives up on
the new ones (raw machines: Expensify 536 -> 581, Discord 1,738 -> 1,919), and inside them the
cleanup is weaker (results kept, loader temporaries not inlined, so imports are not lifted).
It lands with `state_machine_v98` taught the loop form, with a test that a loop whose catch
rejoins it is found (header, protected block, catch, latch).

What A1 leaves for A2, by count: W6 stamps the walk overrides (Expensify 20, Discord 92),
names by scope that differ (15, 85), recorded owners the late walk overrides (112, 559), and
placed accesses whose block scope was never built (4, 12). A2 is done when all four are 0 and
the walk is no longer consulted for a placed access.

Exit of A1: `encode_level_slot` and `slot_owner(func, level)` used only by the planless path.

### A2 — Retire the level machinery (medium to large)

With scopes addressed directly, remove what only existed to make levels land:

- `NESTED_ENV_LEVEL_BASE + slot` and `reg_source_slot`: a slot-held access the graph places is
  addressed by its scope; one it cannot is unknown (as since S3.4).
- `block_scopes` keyed by `(function, builder level)` becomes keyed by creation site, the key
  the graph already uses.
- The shared-parent install: the chain is per access in the plan, so `scope_parents` holds
  only the graph's single answers and `parent_function` is the lexical tree again. The
  hoister's shared exception and the self-creating exception go with it (re-check the closure
  hoister on block-created functions; that exception has its own reason).
- Raw machines: the machine's scope is a scope in its own right (the step's created scope),
  declared in the wrapper because it outlives the step's block. This is S3.2b: declaration
  placement learns that a parked scope lives as long as the owner of the slot it is parked in.
  `STEP_ENV_SLOTS` and the stamped `let`s go.

Split into stages (2026-10-29), each with its own counts; any v96 change is read line by
line (A2a's first fix changed 18: names gained, two `let`s dropped whose name the enclosing
scope now declares, and one hook callback's capture now read as its dependency list already
named it, `str` for `totalRequired`, a slot whose name is wrong in both):

- **A2a — the four counts at 0.** Each row traced to its cause, not overridden: so far every
  one has been lost code (the switch and class fixes above) or a context change after W6.
  Recorded owners: generator collapse moves a block scope from the step to its wrapper
  (`rehome_block_scopes`), and the walk from the step at that builder level found nothing
  while the record held the right scope. The context keeps where each rehomed scope came
  from (`rehomed_block_scopes`) and the walk reads it. Naming then reached those slots by
  their scope and refused `byteLength`: the clash check walked up from the reader to the
  owner, and a block scope is never on the reader's chain, so the walk ran on to the module
  root. It now stops at the scope's function too. Recorded owners the walk overrides: 0 on
  all three bundles (Expensify 112, Discord 559 before). Names gained where the clash had
  refused them (`refs`, `regex`, `error`, `_Error`; Expensify 2,140 changed lines, Discord
  5,536, v96 18). Undeclared: v96 1,807 unchanged, Discord 6,725 -> 6,658, Expensify 12,283
  -> 12,286, the rise in raw machines whose parked slots the wrapper still binds as `c0`
  where the step's reads now take the usage name (`attachmentID`): wrong before as well
  (the step's parameter stood in), and A2e's to fix. Consts written unchanged (1, 1, 0).
  Next, lost code again: a `catch` and a `finally` on one body start at the same block, and
  structure recovery kept one handler per start, so the `finally` won and the `catch` body
  was never printed (Sentry's `sentryWrapped` lost `withScope(...); throw ex`, and the
  function it creates had no parent; React's `startTransition` lost its `reportGlobalError`).
  Handlers sharing a start now open outermost first (the handler more blocks throw to) and
  each nests the next. Expensify stamps 5 -> 2; strings lost 0 on all three; undeclared v96
  1,807, Expensify 12,286 -> 12,283, Discord 6,658 -> 6,551; consts written unchanged.
  Then a context change after W6: `reanalyze_all` gives block scope ids again in a new
  order, and a function whose creator the rescan no longer sees kept its parent by the old
  number, now another function's block (Expensify F109623 read F103111's slots). Such a
  parent is kept by builder and level and renumbered after the scan. Stamps Expensify
  2 -> 0, Discord 44 -> 12. The last twelve were lost code: a loop whose header jumps into
  its body (two nested counted loops under `hermesc -O`) printed the header's statements
  before `while (true)` and never printed the loop's exit, so the inner counter's reset and
  everything after the loops were gone (Discord's `crc32` table module lost its export,
  `ConjureDitherField` its worklets and exports; nested loops are in the round-trip corpus
  now). W6 stamps the walk overrides and names by scope that differ: 0 on all three.
  Undeclared Expensify 12,283 -> 12,272; strings lost 0; consts unchanged.
  Found on the way: the IPA parameter rename (`rename_param_registers`) did not check the
  body's names, so a local the namer called `arr` and a parameter IPA calls `arr` merged,
  the local's `let` went and its writes clobbered the parameter (a `-O` loop that reuses
  the parameter's register shows it; so does `hide(arg0, message)` opening with
  `message = undefined`). W11f now drops a parameter name a local of the same body holds,
  once both are final; the parameter prints as `argN`. Dropped: v96 4,720, Expensify 9,557,
  Discord 15,630. Undeclared v96 1,807 -> 1,485, Expensify 12,272 -> 12,052, Discord
  6,550 -> 6,558: the clobbered writes had left module-level `let`s (`let sku_id;`) that
  hid reads already wrong, such as a timer callback reading `guild_id` for its loop
  variable. Those are capture naming, A3's. Strings lost 0; consts written unchanged.
  A parameter the body only reassigns (`str = str.substring(0, 1)`) loses its name too;
  telling that apart from a clash needs identities (A3).
  The one block scope never built (Expensify F71548 level 130) is A2c's, not a stray
  row: jQuery's `inspectPrefiltersOrTransports` with `inspect` compiled inline twice, each
  copy in its own block holding its `selected`, and the `each` callback F107458 created in
  both blocks and in its own. Its store `selected = ...` has three owners; the context
  keeps one creation scope per function, so the first block is the only one built, the
  second copy's read prints as the undeclared `closure_130_0` and the callback writes
  `closure_0$2`. Shared-body copies (S3.3) would split it but skip functions that create
  themselves. It closes with A2c (block scopes by creation site) and the copies, and is
  re-checked there.
- **A2b — the walk no longer consulted for a placed access**, then A1b2 (naming by scope).
  Done (2026-10-30): W6's `bake` stamps a placed access with its recorded scope and names
  it from that scope's table, and `baked_owner` returns the recorded owner; a disagreeing
  walk is still counted and traced, no longer followed. Output byte-identical on all three
  bundles, as A2a's zeros predicted. The walk now answers only accesses with no record:
  W6 stamps v96 0 of 406,289, Expensify 2,168 of 762,149, Discord 9,075 of 1,318,481;
  baked owners v96 0, Expensify 551, Discord 481. Those are A2c's to place or call
  unknown (the never-built block, slot-held accesses the graph cannot place).
- **A2c — `block_scopes` keyed by creation site**, not `(function, builder level)`.
  `NESTED_ENV_LEVEL_BASE`, `reg_source_slot` and `rehomed_block_scopes` go with it.
  Exit includes Expensify F71548 (a function created in two blocks of one creator and in
  itself, A2a above): each block built, the callback's store reaching its own block.
- **A2d — the shared-parent install goes.** `parent_function` is the lexical tree again; the
  hoister's shared and self-creating exceptions are re-checked on block-created functions.
- **A2e — raw machines.** S3.2b already parks a machine's scope in its wrapper
  (`parked_scopes`; `STEP_ENV_SLOTS` is gone). What is left is that the machine prints
  correctly while it stays raw, and that the catch-loop fix can land. Census (Expensify,
  2026-10-29): the lifter gives up 492 times, 419 on a case it cannot parse (a case whose
  `yield` sits under an `if` or a loop: it lifts straight `yield` chains only), 69 on two
  terminal cases with content, 4 otherwise. Stages:
  - **A2e1 — one name per parked slot.** The wrapper's binding, its destructuring and every
    read in the step take one name, decided per slot (`SlotRef`), not per spelling: today the
    wrapper binds `c0` while the step's reads take the usage name (`attachmentID`), +3
    undeclared on Expensify. Counted: parked slots whose references print under more than
    one name, to 0.
  - **A2e2 — `separate_step_env_names` keyed by slot.** Its clash test reads names; with A2e1
    it compares slots, so a slot reached two ways is not split and two slots under one
    name are. Undeclared and unparsable on both v98 bundles must not rise.
  - **A2e3 — the dispatch may be a loop.** `state_machine_v98` and `keep_raw_machines` accept
    a machine whose dispatch sits in a loop (what the catch-loop dominator change makes of
    one whose catch rejoins the dispatch), with a fixture of each shape.
  - **A2e4 — the catch-loop fix lands**: catch blocks get their protected blocks as
    predecessors in `compute_dominators`, with the test from the finding above, and raw
    machines not rising (536 and 1,738 before; the change alone made them 581 and 1,919).

  Lifting the machines that stay raw (a case whose `yield` is under control flow, two
  terminals) is structuring, not identity: `../../04_transforms/GENERATOR_LIFT_PLAN.md`,
  which lifts on the CFG before structuring. Its G2 removes the loop-shaped dispatch, so A2e3
  is re-read once G2 lands and may reduce to A2e4.

Exit: no `NESTED_ENV_LEVEL_BASE` slot identity and no `rehomed_block_scopes`; raw machines
and `parked_scopes` measured; v96 changes read line by line, v98 changes class by class.
(`STEP_ENV_SLOTS`, `inlined_nested_reads` and `inlined_recursive`, named here before, went
with S3.2b's parked scopes.)

### A3 — An identity for every variable (large; staged by pass family)

`Var` gains an identity: an environment slot's is its `(scope, slot)` from A1; a local's is
assigned where registers become variables (F23 register naming) and kept by every pass. The
name becomes a hint. Equality and hashing move to the identity.

Done pass family by pass family, each its own step with the stage's measurements, in the order
the defects above point to:

1. cleanup (`cleanup_noise` self-assignment; `stamp_merged` goes),
2. declarations (write counting per identity; `written_first_vars`, `split_capture_and_local`,
   `drop_slot_lets_of_params` go),
3. inlining and object folding (`inline_named_variables`, `slot_fills`, `hoist_closures`),
4. the generator and async machinery (`keep_raw_machines`, `separate_step_env_names`),
5. naming (`var_naming`, W7–W11) and shared bodies (`separate_copy_names`; a copy is a
   renaming of identities per creator scope, not of names).

Before step 1, **A3.0 — the guard**: a debug check that counts, per pass, each `Var` made
without an identity (178 construction sites outside tests, most in `transforms/inline`,
`analysis/metro`, `var_naming`, `patterns` and `codegen`), logged with the census. It changes
no output; its counts are what each step below must bring to zero for its family.

Steps 4 and 5 are each split (2026-10-29):

- **4a — moves keep identities.** `keep_raw_machines` (the `createGenerator(step)` rewrite and
  the rehome to the wrapper), W16b's collapse (the step's body moved into the wrapper) and
  W15's unwrap carry every stamp they move. Measured by the dropped-stamp census taken
  before and after each, which must not move.
- **4b — the async parameter and its slot copy are two identities.** W15's `url` case: the
  wrapper's parameter and the slot it is copied into are told apart by identity, so the
  slot's `let` cannot redeclare the parameter. Measured by the I2 rows on async wrappers.
- **4c — W16c reconstruction keeps identities.** `state_machine_v98` flattens cases into one
  body; the variables of each case keep their identity through the flattening, census before
  and after W16c.
- **4d — `separate_step_env_names` goes.** With 4a–4c, a parked slot's clash is two identities
  under one name, and the general clash rule (step 5) renames it. Undeclared and unparsable
  counts must not rise on either v98 bundle.
- **5a — renaming by identity.** `rename_variables_in_stmts` gains an identity-keyed form, and
  the naming passes move to it one at a time, each its own commit with the counts: W10
  `closure_definitions`, W11 definition-site naming, W11b `ancestor_inherit`, W11c
  `capture_sync`, then the IPA and metro renames. A pass converted renames the slot, never
  the spelling, so a body's own local of the same name stays as it was.
- **5b — shared bodies by identity.** A copy of a shared body maps identities per creator
  scope; `separate_copy_names` and the rename half of `shared_body_variants` go. I1 at the
  shared-body floor.
- **5c — the leftovers.** Every pass still renaming by spelling, from the guard's counts, each
  converted or shown to rename only locals it created.

During the transition a debug check reports every `Var` made without an identity, per pass;
the count must reach zero before A4.

Exit: the dropped-stamp census (`stamps::report`) at zero; I1 at the shared-body floor; each
deleted patch's defect class covered by a passing fixture; v96 changes only where a name-based
merge was wrong, each read.

### A4 — Names at codegen (large)

Naming passes stop renaming text and record candidate names per identity, with the evidence
rank they already have (`NameSource`). One allocator assigns final names per scope, after the
shared-body copies exist: an identity visible in a nested scope reserves its name there, so a
local never shadows a capture it sits beside, and a parameter's copy and a different slot get
different names because they are different identities. Declarations and their kinds come from
writes per identity across every body that renders it, copies included.

Staged (2026-10-29) so that no step changes names before it has been measured in the dark:

- **A4a — shadow allocator.** Naming passes record each candidate per identity alongside the
  rename they still make; the allocator runs after the shared-body copies and its names are
  compared with the printed ones, not applied. The counts are the differences by class
  (a capture shadowed, two identities under one name, a weaker name chosen), read before
  anything is switched.
- **A4b — slots named by the allocator.** Environment slots take the allocator's names; locals
  keep theirs. Unparsable and undeclared counts must not rise on any bundle.
- **A4c — locals named by the allocator.** The register-born identities (F23) too; the
  naming passes stop renaming text.
- **A4d — declarations by identity.** Writes counted per identity across every body that
  renders it, copies included, decide `let` or `const` and where: `copy_writes` /
  `relax_written_consts`, `extra_writes_from_nested_bodies`, `split_capture_and_local`,
  `drop_slot_lets_of_params` and the outer `written_first_vars` go, each with the const and
  undeclared counts.
- **A4e — the rest of §1's patches**: `stamp_merged`, the hoister's `stay_at_sites` (with
  A2d's re-check), each deleted with the fixture of its defect class passing.

Exit: the patches listed in §1 deleted; consts written 0 and unparsable modules 0 on both
bundles (Discord's `_fireSurveyAction` included); I1 and I2 at their floors with the
remaining rows explained; v96 and v98 measured.

## 5. Order and size

§3 → A0 → A1 → A2 → A3 → A4. A0 and the §3 ratchets are small and unblock everything. A1 is
a re-plumbing with identical output and is the foundation; A2 depends on it. A3 is the large
one and is safe only in steps, each behind the ratchets. A4 depends on A3 being complete for
the passes that name. Plan B can start once A3's step 5 (shared bodies) is done.

## 6. Risks

- **Churn.** A2–A4 change names across whole bundles; line diffs stop being readable. Judge by
  the counts and the oracle, and read changes by class, as in S3.
- **Half-converted passes.** Between A3 steps some passes compare identities and others names;
  a pass that builds a `Var` by name only reintroduces the bug. The debug check is the guard.
- **v96.** Several fixes in S3.4 changed v96 output for the better (the lost store). Expect
  the same: "byte-identical" holds for A0, A1 and A2; from A3 on, every v96 change is a read
  class with a reason.
- **The cache.** Identities change the snapshot layout: bump `CACHE_VERSION` at each step
  that changes `Var` or `ClosureContext`.
- **Run time.** Identities add a field to every variable and the allocator is whole-program;
  keep the Discord run within today's (about 23 minutes) or record why not.
- **Planless paths.** Single-function decompilation has no closure context; it keeps levels
  until A3 gives it identities of its own, and must keep working throughout.
