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
- **A1b1, slots collected per scope.** The closure walk (W1) builds a scope-keyed table beside
  the level-keyed `ClosureInfo`; naming reads the level table still and the census compares
  the two (0 disagreements).
- **A1b2, naming per scope.** Names are allocated from the scope table for planned accesses.
- **A1b3, the `slot_owner` callers read the stamp,** one pass per commit (`var_naming/*`, metro
  propagation, `ir_gen`, `capture_sync`, `closure_definitions`), each byte-identical.

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

Exit: no `NESTED_ENV_LEVEL_BASE` slot identity, no `STEP_ENV_SLOTS`; `parked_scopes`,
`inlined_nested_reads`, `inlined_recursive` measured; v96 identical; v98 changes read class by
class.

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
