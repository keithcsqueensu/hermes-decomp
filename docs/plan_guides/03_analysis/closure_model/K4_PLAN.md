# K4, staged — captures keep their identity to codegen

Expands phase K4 of `PLAN.md` ("carry `ClosureVar` to codegen") into separately shippable
stages, after K1–K3 shipped and K4-small did not (`PLAN.md` C9, C10). Conventions as in
`PLAN.md`: **[code]** is `file:line` when written, re-derive it; **[measured]** is a number from
running the tree on Equinox 11.43.0 (`index.android.bundle.backup`, v96).

---

## 1. Why K4, and why not as first written

**The problem, as measured.** After W6 a captured variable's identity — *slot `S` of scope
`O`* — exists only as text. Every later stage that needs it re-derives it from the spelling or
from `ClosureContext.baked_captures`, a side table keyed by that same text, which goes stale as
soon as any pass renames a body. The context also keeps its own name for the slot, which later
stages (IPA, Metro, allocation) change without touching the IR. Three consequences, all seen on
Equinox:

- two variables printed as one (C9: `genMethod` passing its `type` argument as the native
  bridge) — fixed for the paths found, by allocation;
- one variable printed as two (C10: `resolveThenable`'s callbacks reading `status`, a parameter
  the owner reuses for a string, while the owner binds `closure_0$2`) — avoided only by keeping
  the late renamers away from C9's names;
- context and IR disagreeing about a slot's name, which codegen turns into declarations of
  names the body does not use: 1,091 bare `let closure_…;` lines remain in the K2/K3 output.

**Why no local repair works.** K4-small tried to keep context and IR in agreement at one
point in the pipeline (`PLAN.md` C10, table). Even the most conservative variant — never
rename IR, only set the context back to the IR's name, once, after every naming pass — changed
1,868 hunks and introduced undeclared names, because codegen declares slots from the context
(`pipeline/context/rendering.rs:201` `own_env_slot_names`, `:434`
`ancestor_env_slot_names`) and today's output depends on the drift. Agreement needs one name
to agree with, which means the IR must know which slot a name is.

**Why not the K4 first written.** "Stop lowering in `resolve_closures`" keeps
`Binding::ClosureVar { level, slot }` alive past W6. Every pass after W6 matches
`Binding::Variable` — **[measured]** 682 sites in 126 files, by area: `transforms/patterns`
85, `transforms/inline` 82, `analysis/metro` 78, `transforms/var_naming` 62,
`transforms/codegen` 49, `pipeline/context` 47, `transforms/optimize` 39,
`transforms/destructuring` 37, the rest under 25 each. A node those passes do not recognise is
a node they silently skip: every one would have to be taught, and each lesson is a behaviour
change. And `level` is relative to the function whose body holds the node, so a statement
moved into another body (inlining) would change meaning.

## 2. The design

Keep the name; attach the identity to it.

```rust
// ir/types.rs
pub enum Binding {
    Register(u32),
    Variable(Var),
    ClosureVar { level: u32, slot: u32 },     // until W6, as now
}

pub struct Var {
    pub name: String,
    pub slot: Option<SlotRef>,                // Some for a baked capture or own-slot binding
}

pub struct SlotRef { pub owner: u32, pub slot: u32 }   // owner: function or block-scope id
```

Three properties make this the right shape:

1. **Renames carry it for free.** `rename_variables_in_stmts` assigns the new name in place
   (`analysis/naming/renaming.rs:333`, `*name = new.clone()`), so a pass that renames by text
   renames the `name` field and leaves `slot` alone. No pass has to learn anything for the
   identity to survive it.
2. **Passes that are not taught keep working.** They read `var.name` where they read the
   `String` before. **[measured]** roughly 214 sites construct a variable and ~400 only match
   one; the change at each is mechanical, and compiler-driven.
3. **The identity is absolute.** `SlotRef.owner` is a scope id, not a level, so it means the
   same thing in any body — inlined, hoisted, or moved by codegen.

With it, the invariants the C9/C10 bugs broke become checkable facts instead of hopes:

- **I1** — every `Var` with the same `SlotRef` has the same `name`, in every body.
- **I2** — within one body, two different `SlotRef`s (or a `SlotRef` and a local) never share a
  name, counting every scope the body can see.
- **I3** — a function declares exactly the slots it owns that its IR uses, by `SlotRef`.

## 3. The harness, and the gates every stage passes

`scripts/measure/` (this commit):

| tool | measures |
|---|---|
| `undeclared.mjs` | references no scope declares, per module (`acorn` + `eslint-scope`); `--dump` the per-name counts, `--show` a name's lines |
| `substitutions.py` | every identifier substitution in a diff, classed placeholder→name, name→placeholder, name→name |
| `compare.sh base.js new.js` | both of the above, plus `worse:` — every name with more undeclared references than in the base |

**[measured] baseline** (K3 output, `f61e191`): 42,458,760 B / 981,444 lines, ~65 s release; undeclared 3,124
(placeholders 220, distinct names 656); 1,653 module chunks do not parse as ES modules
(374,505 lines) and are not counted — compare two runs only when that number agrees.

Every stage is one of two kinds, and says which:

- **Representation stages** must be **byte-identical** on Equinox (and on CoachRx 2.1.223 as a
  second bundle), with the full test suite, the VM oracles (`HERMES_VM_V96/98/99`), the corpus
  tests (`HBC_CORPUS_BUNDLE`), the round trip and `gates.sh`.
- **Behaviour stages** pass all of the same except identity, and in addition: `worse:` is empty
  or every name in it is explained in the commit; the `name -> name` substitutions are read and
  each class is explained; and the I1/I2 violation count (from K4.3) does not rise. The round
  trip gains a snippet for every bug the stage fixes.

K4-small showed why `worse:` comes first: its best variants cut the total by ~170 while
breaking three names, and only the per-name diff showed it.

## 4. Stages

```
K4.0 harness ──► K4.1 Var ──► K4.2 stamp ──► K4.3 checker ──┬─► K4.4a/b/c declare by identity
     ✅             ✅            ✅              ✅          ├─► K4.5 resolvers by identity ──► K4.6 rename_slot ──► K4.6b late renamers ──► K4.7 enforce I1/I2
                                                              ├─► K4.R relocation checks (hoisting, inlining, module loaders)
                                                              └─────────────────────────────────────────────────────────────────────────────► K4.8 retire
```

K4.4 onward was split further after K4.3 measured what is actually wrong (§7): the classes
are distinct enough that each wants its own commit and its own count to fall.

### K4.0 — harness and baseline ✅ (this commit)
`scripts/measure/`, the baseline above, and this plan.

### K4.1 — `Var { name, slot }` — representation, byte-identical
Introduce `Var` and `SlotRef`; `Binding::Variable(String)` becomes `Binding::Variable(Var)`,
`slot` always `None`. Give `Var` `From<String>`/`From<&str>`, `Display`, `PartialEq<str>`,
and a `name()` accessor so most sites change by one token; let the compiler find them.
`MutVisitor` hands renaming passes `&mut var.name`. `Binding::name()` (`ir/types.rs`) returns
`var.name`. Serialised IR changes shape: bump `CACHE_VERSION`.

*Acceptance:* byte-identical; tests unchanged except for construction syntax. *Size:* the
largest diff of the plan and the least risky — mechanical, ~600 sites, no decision anywhere.
Land it in a few commits by directory so each is reviewable.

**✅ Shipped** (`ef4b771`), as one commit: the type change does not compile in parts. Two
decisions the plan left open: `Var` compares, orders and hashes by `name` alone (a pass that
rebuilds a `Var` from a string must still match the stamped original), and derefs to its name.
rustc's suggested fix at eight rename sites was `*var = new.into()`, which replaces the whole
`Var` and drops the slot; those assign `var.name` instead. Byte-identical on Equinox and
CoachRx; peak RSS 1,725 MB vs 1,719 MB.

### K4.2 — stamp at the bake — representation, byte-identical
`resolve_closures` (`analysis/closure/mod.rs:187,270`) stamps `SlotRef { owner:
slot_owner(f, level), slot }` on every `Var` it produces from a `ClosureVar`, own slots
included. Nothing reads it yet.

Then measure **stamp survival**: a debug counter (behind `RUST_LOG=slots=debug`) of stamped
reads and writes per function after W6, after each later stage, and at codegen. A pass that
rebuilds a `Var` from a `String` (`Binding::Variable(format!(…).into())`, hoisted `let x;`,
destructuring rewrites) drops the stamp; each such site is found by the counter falling across
it, and fixed to carry `slot` through.

*Acceptance:* byte-identical; at codegen, ≥ 99 % of the reads that were stamped at W6 still
are, every remaining loss listed in the commit by site.

**✅ Shipped** (`d3e5a9f`). What the plan did not foresee:

- *A census by spelling does not work.* Locals have no declaration before codegen, so a child's
  own `items` and an ancestor's slot `items` look alike; a name-based count reported 25k
  "losses" at W6 itself. `analysis/closure/stamps.rs` counts *transitions* instead —
  references that go stamped → unstamped under one name in one function between checkpoints.
  That is an upper bound (a pass that removes a stamped self-store and copies an unrelated
  unstamped reference of the same name counts too), so each residue was checked by hand.
- *`Statement::Let.name` had to become a `Var` too.* Declaration insertion folds a slot's first
  write into `let x = …`; with a `String` name, 45k of 58k own-slot stamps died one step
  before codegen. K4.4 depends on them.
- The other drop sites were W13's inliner (re-emitting a candidate from its name; flushing a
  deferred value from a `String`-keyed map). Upper-bound survival at codegen ≥ 99.9 %; no
  single render step drops a stamp.

### K4.3 — the identity checker — diagnostic, byte-identical
`check_slot_identity(all_ir, ctx) -> Violations { i1, i2, i3 }`, run at the end of naming
when `RUST_LOG=slots=debug`, and in a corpus test that fails when the count on
`HBC_CORPUS_BUNDLE` exceeds a recorded ceiling (the pattern `syntax_baseline.tsv` uses). It
reports, for each violation, the `SlotRef`, the bodies and the names.

This replaces guessing with a count: the number of C9/C10-class defects left in the output,
exactly. Every later stage is judged by it falling. *Acceptance:* byte-identical; the
ceilings recorded; the known cases (`genMethod`, `resolveThenable`,
`useAnimatedPropsLifecycleWithPrevNodeRef`) each show up, or are explained as not violations.

**✅ Shipped.** `analysis/closure/identity.rs`; `PipelineContext::check_slot_identity` runs it
on every body *as codegen renders it* (nested bodies from `prepare_render_bodies`, factories
from the new `prepare_factory_body`) — parameters named, declarations inserted — which is
where §5 says I2 has to be judged. Logged at the end of naming (I1/I2 only) and at codegen
under `RUST_LOG=slots=debug`; `HD_SLOTS_DUMP=<prefix>` writes every violation as TSV. The
corpus test is `tests/slot_identity.rs`, ceilings in `tests/slot_identity_baseline.tsv`
keyed by bundle SHA-256. Counts and the known cases are in §7.

### K4.4 — declare by identity — behaviour, three commits
`own_env_slot_names` / `ancestor_env_slot_names` (`pipeline/context/rendering.rs`) compute
from the stamped IR — the names the body actually uses for slots it owns, and the names its
ancestors' slots carry — instead of from the context. This is the step K4-small could not
take: there is no second name to disagree with. Split by the I3 class each removes:

- **K4.4a — declare every own slot the body uses** (I3 `Undeclared`, 1,270 on Equinox:
  e.g. `prompt` reads `cancelButtonKey` that nothing declares). **✅ Shipped**, see below.
- **K4.4b — never declare a capture** (I3 `DeclaresCapture`, 306: the child's
  `let closure_3 = String(index)` that should have written `prompt`'s `cancelButtonKey`).
  Declaration insertion must skip a `Let` whose slot another scope owns; the write stays an
  assignment. **✅ Shipped**, see below.
- **K4.4c — drop declarations nothing reads as this binding** (I3 `Unused`, 217). The
  1,091 bare `let closure_…;` in §1 is *not* this number: most of those are hoisted
  declarations of destructuring targets the body does assign (`let closure_3; … [tmp4,
  closure_3] = …`). The checker counts a bare `let` as unused only when no reference in the
  body, and none below that resolves to this function's slot or is unstamped, reads it.
  **✅ Shipped**, see below.

*Expected:* I3 to zero. *Acceptance:* the behaviour gates.

**K4.4a ✅ shipped.** What the 1,270 actually were (Equinox):

| class | count | what it was |
|---|---|---|
| module factories | 1,180 | not a defect: a slot named `react-native` is written `x = require(n)`, which codegen turns into `import react_native`, and anything else the module writes but never declares gets a module-level `let`. The checker now counts a factory's slot as declared when the module writes it (`RenderedBody::module`). |
| read before written | 76 in these two rows | the declaration pass took a slot the body reads first (its closures write it) for a capture: `free_captured_vars`. |
| spelled like an ancestor's slot | (62 spelled so, overlapping) | `outer_names` kept it out; the body's writes went into the ancestor's variable (`useScrubController` set the module's `closure_7 = { maxDuration: 1 }` to a state setter on every render; `l = closure_66` overwrote the enclosing `function l`). |
| both, one name for two slots | 14 | I2 `TwoSlots` in the same body: no declaration is right; K4.7 renames one side. Left alone (`stamps::own_slot_names` excludes them). |

The fix: `stamps::own_slot_names(func, body)` reads the names the body itself uses for its
own slots (block scopes excluded) off the stamps; `insert_declarations_with_slots` takes
them as `own_used`, removes them from `outer_names` and hoists any the body reads first.
Both render paths go through one `PipelineContext::insert_body_declarations`.

Discord's `worse:` list found the placement half. A slot belongs to the whole function, but
the declaration pass put its `let` at the first write, and when that write sits in a branch
a closure created outside the branch cannot see it. Before, the module-level `let` covered
for it. Examples: a generator's branch stores `force_survey_id` that a helper declared at
the top of the function reads, and `mapAsyncIterator`'s `abruptClose` is a function
declared in an `if` while `mapResult` above it reads it, which is block-scoped and unbound
in a strict module. The escape scan (`names_assigned_in_nested_bodies_and_used_outside`)
only sees direct reads, so `own_slots_escaping_their_block` also counts creating a closure
as a read of every own slot that closure, or a function below it, reads by stamp
(`read_by`, from `PipelineContext::own_slots_read_below`). An own slot that escapes its
block is hoisted. Hoisting every own slot written in a block instead tripled the diff
(1,966 hunks) for a smaller gain, because it also turned block-local helpers into
`let f; … f = function f…`.

One more change came out of the gates. The module-level `let` was hiding a second class:
lodash's `cloneRegExp` destructures into a local `source` it never declared, because an
ancestor's slot is called `source` *in the context* but nowhere in the IR, and the stale name
sat in `outer_names`. With `template` now declaring its own `source`, nothing bound the
local any more (CoachRx `worse: source`). `precompute_ancestor_env_slot_names` therefore
drops a context name that none of the slot's stamped references use. It only drops names.
Adding the names the IR does use for ancestors' slots was tried. It halved
`DeclaresCapture`, but it also suppressed real locals that share a spelling used elsewhere
(CoachRx undeclared +34, `containerInfo` +22), so it is K4.4b's to do with its own
measurement.

**[measured]** at codegen, K4.3 → K4.4a:

| | Equinox | CoachRx |
|---|---|---|
| I3 `Undeclared` | 1,270 → 14 | 889 → 23 |
| I2 `Shadowed` | 167 → 175 | 124 → 139 |
| I1, other I2, other I3 | unchanged | unchanged |
| undeclared references (harness) | 3,124 → 3,000 | 5,046 → 4,990 |
| hunks | 671 | 423 |

Every changed line is a declaration (`x = …` → `const x = …`, a hoisted `let x;`, a name
leaving a module-level `let` list). No `name -> name` substitutions. The +23 `Shadowed` are
each a scope that now declares its own slot under a name a child reads for an ancestor's
slot. Before, that scope wrote into the ancestor's variable and the checker could not see
it. Now the child reads the wrong variable and the checker counts it. Both outputs are
wrong, and K4.7's rename is what fixes it. `worse:` on Equinox is `React` 0 → 14: the
harness counts a top-level declaration anywhere in the file as covering a reference in any
module. The module-level `let React` was `SessionDetails`' own slot, which it now declares
itself, and the 14 references are in another module where `React` was never bound.

Discord 350.8 (v98): I3 `Undeclared` 5,517 → 337, undeclared references 50,514 → 49,673. Its
`worse:` lists 7 names (`collapsed`, `parentId`, `adContentId`, `force_survey_id`, `c42`,
`_false`, `onConfirmActivityLaunchChecksAlertOpen`). Every one has an I1 violation in both
versions: a body reads a slot under a name its owner does not use. In K4.3 each of those
reads resolved to an unrelated module-level `let` of the same spelling, the catch-all for
another function's undeclared write (F91991 reads slot 34861:1 as `collapsed`, while the owner
calls it `closure_1` and the module `let collapsed` belonged to an unrelated
`collapsed = tmp4[0]`). That function now declares its own variable, so the read is
unbound instead of reading the wrong variable. Giving the slot one name is K4.6.

Round trip: `capture_vs_local` added (a capture beside a same-named local, I2), recorded as
a known failure on v96 for K4.7. K4.4a's own shape did not reproduce in a small snippet: on
the bundles it comes from the late per-body renamers (K4.6b).

**K4.4b ✅ shipped.** The rule as planned is one line. Making it safe took most of the work,
because a store that stays an assignment needs a binding to land in, and many bindings
existed only by accident: codegen's module-level `let`. That `let` is a text scan, and it
counts a `let x` *anywhere* in the module as binding `x` everywhere. Each piece below was
found by the gates' `worse:` list going up on one bundle or another.

- **The rule, per body.** `stamps::captured_slot_names` gives the names this body uses, by
  stamp, for an ancestor's slot. They make up `outer_names` for this body, together with the
  ancestors' slot names (as the context has them) that a function nested in this body uses,
  so a local declared here never hides the ancestor's variable from it. Any other name an
  ancestor's slot merely shares is the body's own local. The context's names used to block
  those for every body: each function in a module using its own `hasOwnProperty`
  temporary stayed undeclared, kept alive only by the module `let`. Dropping the context's
  names entirely more than doubled `Shadowed` (168 → 399), hence the nested-use guard. Making
  `outer_names` the IR names of all ancestor slots, for every body, suppressed real locals
  (K4.4a's note). Only an *ancestor* counts as an owner: statements inlined from another
  function keep that function's stamps, and its slot is a local here (the checker's I3 now
  agrees).
- **The owner must bind the name** (`owner_binds`). A function owner declares every name any
  body gives its slots (`slot_names_of`, from `slot_names`, computed once). When the names
  disagree (I1), each name becomes its own variable in the owner's scope, shared by the
  bodies that spell it the same. That beats a dead local (React's current fiber: `c100` in
  some hooks, `_null2` in others; `renderWithHooks` declared `let c100 = arg1`, and the fiber
  never reached the hooks). K4.6 gives the slot one name. A module factory gets its slot
  names from ESM codegen at module level (`with_module_slot_names`). Declaring them in the
  factory body instead broke the re-export detection (`export * from` printed as a whole
  module). An owner that nothing creates any more binds nothing (`created_functions`): an
  async wrapper takes its generator's body and leaves the generator's
  environment-creating function unrendered.
- **Pre-existing declaration bugs** the module `let` was hiding, now visible and fixed:
  `written_here` counted writes by nested functions, so a slot only closures fill was never
  declared (`defineLazyObjectProperty`'s cache, an `__awaiter` callback's slots); phase 2's
  `declared` set ignored block scope, so `let self2` in one branch left `self2 = this` in the
  next unbound (`nested_block`); a branch's `let` did not reach a later top-level write
  (lodash `createWrap`'s `applyResult`).
- **Class method bodies got no declarations at all.** The class pass rebuilds them inside
  `Statement::Class`, where no declaration pass reached, so every constructor local lived on
  the module `let` (`declare_class_methods`). The IR walker does not visit a class's methods
  either, so they also counted as neither closures (moment's `keys` slot, read by
  `hooks.locales()`, stayed declared inside the branch that set it) nor created functions
  (every slot a class method owns looked like it had no rendering owner).
- **Codegen dropped `const x = globalThis.Builtin.….x`** as a global alias. That is only
  right for `globalThis.X` itself: `captureStackTrace` and `valueOf` are not globals (a
  separate commit).

**[measured]** at codegen, K4.4a (with the JSX fix) → K4.4b. Undeclared references are
counted per module (`compare.sh` with `PER_MODULE=1`, new), because K4.4b moves declarations
between modules' top levels, and the file-wide count credited a reference in one module to
another module's `let`:

| | Equinox | CoachRx |
|---|---|---|
| I3 `DeclaresCapture` | 306 → 3 | 335 → 4 |
| I2 | 2,195 → 2,134 | 1,443 → 1,384 |
| I3 `Undeclared` / `Unused` | 14 / 217 → 12 / 215 | 23 / 243 → 22 / 246 |
| undeclared references, per module | 19,954 → 7,404 | 12,707 → 7,844 |
| modules that do not parse | 1,653 → 1,653 | 1,125 → 1,114 |
| hunks | 18,457 | 6,550 |

On the v98 bundles, per module: Expensify 9.5.14 54,238 → 37,578 (unparsed 3,559 both);
Discord 350.8 180,348 → 108,974 (unparsed 1,725 → 1,726). I3 on Discord: `Undeclared`
5,517 (K4.3) → 239, `DeclaresCapture` 1,707 → 28.

The few `DeclaresCapture` left are readers of a slot whose owner no longer renders, which
keep their local by design. On the v96 bundles, per module, `worse:` lists only browser
globals the Node-based harness does not know (`window`, `DOMRect`, `XMLHttpRequest`, now
printed where a dropped `const X = window.X` used to be). The v98 bundles add a short tail
of the same kind as K4.4a's Discord names: reads that resolved to the module-level catch-all
`let` by accident and are unbound now that the function owning that name declares it. Checked
by hand: `context2` (+68, Expensify) is React's `readContextForConsumer` reading `context2`
while its parameter is `context`, and similar name mismatches. The others are 1–16
references each (`timeout`, `cN` counters, `guildMetadata`). The name mismatch is a naming
defect (K4.6/K4.6b), not a declaration one.

One more pre-existing gap the stamps cover now: an owner whose own reference lost its stamp
(moment's `keys = Object.keys`, one of the census's drops) still has the name its readers
give the slot, from `slot_names`, so it hoists the declaration they need.

**Performance, measured while doing this.** Discord took about 17 minutes, 70 % of it in two
passes over a few functions nested ~590 levels deep. The named-variable inliner ran
`cleanup_noise`, which itself recurses into and copies every nested body, at every level of
its own recursion, so cost grew with depth × size: F149797 (6,281 statements) took 117 s,
twice (W13, and again at the end of W16b). Cleaned once at the entry, it takes 0.6 s with
the same output: Discord 918 s → 443 s, Expensify 244 s → 206 s, byte-identical on all four
bundles. `fold_slot_index_fills` also got a cut (it scanned forward from every literal, even
one with nothing to fill): byte-identical, no measurable gain.

**K4.4c ✅ shipped.** Nearly all of the 217 came from one place. A function hoists `let x;`
for each of its own slots that no statement of its body writes, and it took the names from
two lists: what the context calls each slot, and, since K4.4b, every name any body gives
one (`slot_names_of`). Both lists hold names nothing reads as this function's variable any
more. The context keeps a name a later pass renamed in the IR, and `slot_names` keeps a
name a body used before its own late renamer changed it. Examples: the global function's
`let React; let React2; … let React23;`, and `initializeClassErrorUpdate`'s `let value2;`
beside the `value` its closures actually read. `PipelineContext::retain_read_slots` now
keeps an own-slot name only when the body references it, or a function below it does,
either stamped with one of this function's slots (block scopes included) or unstamped.
A reference below stamped with an ancestor's slot belongs to that ancestor, so it keeps
nothing alive, as long as the ancestor binds the name (`owner_binds`); this `let` would
only hide that binding. "Below" means the context's children plus the closures the IR
creates.

A first version followed the checker exactly and dropped a declaration whenever every read
below was stamped with another scope's slot. On v98 that left reads unbound: Discord +118
undeclared references, Expensify +23. Three shapes were involved:
- **Clones.** d3's four `link` factories share one child body, cloned after the context was
  built (F79629), so it is in no function's child list. The clone keeps the stamps of the
  factory it was cloned from, a sibling's slot (`45329:1`). The dead `let linkSource2;` in
  each other factory was its only binding.
- **I1 reads.** A read under a name the slot's owner never binds has a dead local as its
  only binding too.
- **Browser-global spellings.** ESM codegen declares no module slot spelled like a browser
  global (`is_builtin_global`), whatever `owner_binds` says. Discord's `TakeActionButtons`
  had a dead `let location;`, and the `{ location }` its children build read a module slot
  that is never declared. A read under such a name always keeps the declaration.

Dropping such a `let` turns an `undefined` into a ReferenceError, which matters for a read
like `if (flag)`. Those declarations stay until K4.6 gives each slot one name. The checker
still counts them as `Unused`, which is correct.

**[measured]** at codegen, K4.4b → K4.4c:

| | Equinox | CoachRx |
|---|---|---|
| I3 `Unused` | 215 → 7 | 246 → 3 |
| I2 `Shadowed` | 168 → 151 | 137 → 126 |
| I1, other I2, other I3 | unchanged | unchanged |
| undeclared references, per module | 7,404 → 7,404 | 7,844 → 7,844 |
| hunks | 163 | 145 |

Every changed line is a deleted bare `let x;` (218 on Equinox, 229 on CoachRx), and no
`worse:` names. The `Shadowed` drop is the same lines seen from below: an intermediate
scope's dead `let x;` was hiding an ancestor's `x` from a child that reads it.
On v98, the diff is likewise only deleted bare `let`s: Expensify 534, with `Unused` 569 → 95,
`Shadowed` 402 → 398, and undeclared references unchanged at 37,578; Discord 2,169, with `Unused`
2,042 → 134, `Shadowed` 1,804 → 1,529, and undeclared references 108,974 → 108,976. Discord's two
are `logger` and `voiceChannelId`, each read with a method call (`logger.error(…)`,
`voiceChannelId.getVoiceChannelId()`) that was already broken: it threw a TypeError on
`undefined` before and throws a ReferenceError now. `logger` is read inside a generator body
in `NativeNotificationsManager`'s constructor, where the dead `let` was. Why the walk
misses that read was not traced. The `Unused` left on v98 is mostly the declarations kept
on purpose (above).

What is left of `Unused` on the v96 bundles, besides those kept on purpose:
- `DeclareGlobalVar` in the global function (`__DEV__`, `__METRO_GLOBAL_PREFIX__`,
  `_sentryDebugIdIdentifier`). The IR turns each global the Metro prelude declares into a
  `var`, rendered as `let` inside `function global()`, so it binds a local. Those three are
  unread because the prelude stores through `globalThis.x`. `process` and `stack` in the
  same list *are* read, and the local shadows the real global (`globalThis.process =
  process || {}` reads the undefined local). That is a correctness bug older than K4,
  limited to the prelude, and needs its own fix: global declarations should not render as
  locals.
- `_pumpToNode`'s `c0` (Equinox): an async wrapper that took its generator's body. Its
  closures do read and write `c0`, so the declaration is right; most likely they are
  stamped with the generator's slot, which the checker does not count as the wrapper's own
  (K4.4b's `created_functions` note). F12123's `c1` is a counter of the same kind, not
  checked by hand.

### K4.5 — resolvers by identity — behaviour, one commit each
Replace each text-or-record resolution of a slot with the stamp, measuring each alone:

1. metro `closure_write_target` (`analysis/metro/propagation/phases.rs`) and the
   `closure_N` slot-update fallback (`…/propagation/mod.rs`), plus its three hand-rolled level
   walks (which ignore block scopes) — C10's second experiment, now exact;
2. W10 phase 1 and `resolve_var_to_slot` (`transforms/var_naming/closure_definitions.rs`),
   and `owner_var` — the owner's binding is the `Var` stamped with the slot in the owner's body;
3. ancestor inherit (`transforms/var_naming/ancestor_inherit.rs`);
4. `capture_sync` (`transforms/var_naming/capture_sync.rs`).

Then `baked_captures` has no readers and goes (`CACHE_VERSION`). *Acceptance:* the behaviour
gates per commit; I1/I2 counts non-rising.

**K4.5.1 ✅ shipped (metro).** One resolver, `phases::slot_write_target`, now gives the
`(owner, slot)` of a slot store for both metro passes. A stamped target names it outright;
otherwise the level is resolved through `ClosureContext::slot_owner`, which knows block
scopes. It replaces `closure_write_target` plus `closure_slot_owner`, the three
hand-rolled level walks in phase 2, and its `closure_N` fallback. Two facts shaped it:

- **Metro runs on both sides of W6.** W5 and the re-injection run before W6, when nothing
  is stamped and every store is a raw `ClosureVar{level, slot}`, so there the change is
  only block scopes in the walk. W7b runs `propagate_module_names_to_closures` again after
  W6. There the old code parsed `closure_2` in a child as level 0 and renamed the
  *child's* slot 2, mostly a no-op. Now the stamp reaches the real owner, which puts
  three old weaknesses in play, each held to its old reach:
  - *Eligibility is unchanged.* Only a placeholder target, or a raw `ClosureVar`, names a
    slot. Accepting any stamped store renamed slots that already had names.
  - *The single-store rule counts across functions, for stamped stores.* Node's `eos`
    writes its `callback` slot three times and a closure writes `callback = nop` once.
    Counted per function, that one store named the slot `nop`. A stamped store now counts
    every stamped store into its slot; a store before W6 still counts among its own
    function's. Counting across functions before W6 too lost good names (counters went
    back to `c12`, React's `promise` slot to `_null`).
  - *No block-scope owner after W6.* A name given in the context to a block scope's slot
    never reaches its builder's IR. d3-scale's `calendar` kept `let closure_5 = timeDay`
    while its readers became `timeDay2`. Measured on Expensify, dropping block-scope
    owners: 46 changed lines, and no new undeclared name except the three below; dropping
    every stamped store: 468 lines.

**[measured]** K4.4c → K4.5.1, changed lines / I1 / undeclared references per module:

| | lines | I1 | undeclared |
|---|---|---|---|
| Equinox | 0 | 2,294 → 2,294 | 7,404 → 7,404 |
| CoachRx | 5 | 2,807 → 2,806 | 7,844 → 7,844 |
| Expensify | 46 | 4,790 → 4,790 | 37,578 → 37,578 |
| Discord | 234 | 9,452 → 9,454 | 108,976 → 108,982 |

I2 and I3 are unchanged, except Discord `Unused` 134 → 133. Each change is a slot that had
the name of a value stored in a different slot:
- **CoachRx.** tanstack's `createNotifyManager` declared `let systemSetTimeoutZero2;`, never
  assigned it, and called it to schedule. The owner's binding is `closure_4 =
  systemSetTimeoutZero`, and the readers now call `closure_4`.
- **v98: Metro's `require`.** Two scopes up, `require` (`closure_2_0`, `closure_2_1`) wore a
  sibling slot's name: `webviewUrl2(dependencyMap[6]).intl`, `translate2(dependencyMap[8])`,
  and `sidecarUrl2(…)` while the real `sidecarUrl` was passed as `closure_0`. Every one of
  them was a dead local in the reading function. Both sides are now placeholders, and the
  value slot keeps the name. That is all of Discord's 110 name → placeholder substitutions,
  and Expensify's 18. The `worse:` lists (`closure_2_0`, `closure_2_1`, `closure_2_5`,
  `closure_2_6`, `closure_3_0`; +6 references on Discord, net 0 on Expensify) are those
  same reads: a ReferenceError now, a call on `undefined` before. The undeclared nested
  `require` placeholder is an older class (207 references on Expensify before this
  change); it is K4.R's.

**K4.5.2 ✅ shipped (W10), narrowed.** Two of W10's three resolutions now go by stamp.
- **Phase 1.** A capture's `(owner, slot)` comes from its stamp when the body uses that
  name for one function-owned slot only; otherwise from the bake record or the level, as
  before. Which names are captures at all is unchanged.
- **`owner_var`**, the owner binding a slot's new name renames. It is the placeholder the
  owner's own references are stamped with (`closure_3`, and the `cN` counters), when that
  spelling means only this slot in the owner's body. The old answer was the context's
  decided placeholder, which the owner often did not use. On v98 the old W10 renamed every
  reader of Sentry's tracing options (`traceFetch`, `enableHTTPTimings`, …) while the owner
  went on initialising `c62 = undefined`. The readers read dead locals, and now the owner
  binds the names. Renaming the owner's spelling is safe only when nothing is left behind,
  so `SlotFollow::owner_binding` takes the stamped binding only when every other reference
  to the slot is renamed with it. Otherwise it falls back to the decided placeholder, as
  before. The references that block it, each found as a `worse:` name:
  - another body's reference that W10 does not reach (XHR's `c14`, read in a class method);
  - one inside a `Statement::Class`, where the class pass already moved the method bodies
    and no per-function rename reaches (`allowTouches`, read as `c15` in a constructor);
  - one below the owner that lost its stamp.

  The owner's spelling must also be unique to the slot in its body. A rename goes by
  spelling, so a `c12` that spells two slots took `dependencyMap` with it. Finally, no
  owner binding is renamed to a constant's spelling (`_undefined`, `_null`, …). Those names
  come from a slot whose only known value is that constant, and two such slots in one owner
  then bind one name (Expensify `c15` +14 before this rule).

The object-key pass (`apply_object_key_literals`) keeps its old resolution. Resolving it by
stamp reached owners its level-0 shortcut never did. Some of that fixed things (Apollo's
`const DELETE = Object.create(null)`, GraphQL `query` documents), but it also renamed
bindings into names the owner already used. Two examples: `let closure_8 =
Base64.encode("facility:" + facilityId)` became `facilityId = …`, and reanimated's
`styleAnimations` lost the binding its worklet reads, which the old shortcut had renamed
right by accident. Each guard added for one case uncovered the next, so that pass is left
for its own commit, after `rename_slot` (K4.6) makes owner and readers move together.

**[measured]** K4.5.1 → K4.5.2:

| | lines | I1 | I2 | undeclared |
|---|---|---|---|---|
| Equinox | 345 | 2,294 → 2,245 | 2,117 → 2,109 | 7,404 → 7,404 |
| CoachRx | 322 | 2,806 → 2,781 | 1,373 → 1,363 | 7,844 → 7,844 |
| Expensify | 5,922 | 4,790 → 4,661 | 5,318 → 5,264 | 37,578 → 37,568 |
| Discord | 34,163 | 9,454 → 9,318 | 11,989 → 12,036 | 108,982 → 108,895 |

- **v96.** No `worse:` names. The placeholder → name substitutions are owners now binding
  the name their readers use. Before, the readers read a dangling module `let` or a dead
  local: Sentry's `flavorVersion`, `sentry_version`, `ttl`; XHR's `OPENED`…`DONE`. The
  name → placeholder substitutions are readers that move off a dead local onto the owner's
  real binding: RevenueCat's `CheckoutSessionStart`, a replay `logger` now
  `closure_85 = makeReplayLogger()`. A hook with two `useRef`s now writes the one its
  closure actually reads (slot 1, not slot 0).
- **Discord I2.** The rise is entirely Metro's `require`. Readers of the factory's
  `require` slot now print `require`, the spelling of the factory's own parameter (220
  `CaptureAndParam`, 135 `TwoSlots` where an async wrapper's merged body reads the same
  slot under a second stamp). Same value, same name. It is also why 677 lines of
  `require("module_724")` inside functions became hoisted imports. Without those, I2 falls:
  `CaptureAndLocal` 9,010 → 8,819, `Shadowed` 1,529 → 1,410, `TwoSlots` (other) 860 → 866.
- **`worse:` on v98.** Each name checked by hand was a read that was already broken:
  - `authStore` (+16): a dead `let authStore;` before, so a TypeError on `undefined`; a
    ReferenceError now. The read wants the `GuildRoomStore` import.
  - Expensify's `_var2` (+2): a slot named `var`, sanitized two ways. One of its three
    reads is now bound where none was.
  - The `closure_N_M`: nested `require` as in K4.5.1.

  The rest are 1–8 references each, not all read.

**K4.5.3 and K4.5.4 — blocked on a stamping bug (open).** Both were written and measured,
and on v98 both ran into the same fault: a stamp can name the wrong owner.
`ClosureContext::ancestor_at` walks the function tree one hop per level. That is right
only while every function on the way was created with its creator's own environment. A
function created with an outer one (`env_offsets`) has its own levels shifted to match;
its descendants do not.

Discord's async functions are the common case. A wrapper with no environment of its own
(`F43753`) creates its generator body (`F96550`) on its parent's environment, the
module's. A closure inside the body reads the module's slot at level 2, which is three
function hops away. The walk takes two and lands on the wrapper, which has no slots. The
stamp, made with that walk at W6, says `(43753, 0)` and calls the slot `guildId` where the
code calls `require(dependencyMap[8])`.

The fix is a walk by environment: undo the reader's own shift, and take
`1 + given(f)` function hops per level, where `given(f) = env_offset(f) −
env_offset(parent)`. It is written and unit-tested, and byte-identical on both v96
bundles. On v98 it changes far more than a resolver step should:
- Expensify: 8,142 lines, undeclared −54;
- Discord: 58,875 lines, 16,133 of them placeholders renumbering, undeclared −495, I1
  9,318 → 10,014, and 16 more module chunks that no longer parse.

It is its own stage (K4.2b: stamps walk environments), to be measured on its own. K4.5.3
(inherit takes the stamped slot) and K4.5.4 (`capture_sync` reads both names off the
stamps) follow it, because until then a v98 stamp under an environment-less function is
not to be trusted. The first measurement of K4.5.3 showed exactly that: the stamp sent
nested `require` reads to the wrapper, and 677 lines of recovered imports went back to
`closure_2_0(dependencyMap[8])`.

### K4.6 — `rename_slot` — behaviour
One operation, `rename_slot(all_ir, ctx, SlotRef, new_name)`: renames every `Var` carrying the
`SlotRef`, in every body, and the context's name with it, refusing when `new_name` is already
another binding anywhere those bodies can see (I2). Every post-W6 caller of
`update_slot_variable` (metro W7b, W9's IPA and Metro-role refresh, W10, W11d) goes through
it, so the context and the IR cannot drift.

Then reopen C10's first experiment — let `closure_N$K` through W10/W11's gate, ancestor inherit
and the declaration pass — which regressed only because a rename could reach some bodies and
not others. *Acceptance:* the behaviour gates; `resolveThenable` and `genMethod` correct;
I1/I2 falling.

### K4.6b — the late renamers go through `rename_slot` — behaviour
K4.3 found I1 at 828 at the end of naming and 2,294 at codegen (§7): the stages after naming
create two-thirds of the I1 violations. The renamers there work per body and by text —
W13's inliner, `rename_param_registers`, `rename_reserved_words`,
`make_sanitized_names_distinct`, `repair_switch_clobbers` — so a rename reaches the body it
runs on and not the other bodies that name the same slot. Each is moved, one commit at a
time, to rename a stamped `Var` by identity (all bodies) or not at all. Measure I1 at codegen
per commit. *Acceptance:* the behaviour gates; I1 at codegen ≤ I1 at the end of naming.

### K4.R — relocation checks — diagnostic, then behaviour
Identity matters wherever code moves between bodies, not only for closure naming: W16g
(`hoist_repeated_closures`) gives a repeatedly-created function one declaration in a shared
scope, W16h hoists module loads into the factory, W13 inlines. Each move is only correct if
every slot the moved statements name is visible at the destination — its `owner` is the
destination or on the destination's parent chain — and the destination binds no other thing
under that name. With absolute `SlotRef`s that is a check, not a guess:

1. `check_relocation(stmts, from, to, ctx)`: the slots `stmts` name whose owner is not
   visible from `to`, and the names `to` already binds differently. Logged per pass under
   `slots=debug`; a count of moves that would change meaning.
2. Each hoisting pass refuses a move the check rejects (behaviour, one commit per pass).

The census already shows hoisting moving identity correctly in the common case: W16h turns
~9k capture references into own-slot references of the factory, with every stamp intact.

### K4.7 — enforce I1/I2 at the end of naming — behaviour
A closing pass: for each remaining I1 violation pick one name (best `NameSource` among those
carried, then the owner's), for each I2 violation rename one side by identity, via
`rename_slot`. The K4.3 counts go to zero on the corpus, and the ceiling becomes zero.

### K4.8 — retire the spelling — cleanup
Where a `Var` has a `slot`, the placeholder parsers (`crate::ir::placeholder`) are no longer
consulted; `is_closure_name`-style gates become `var.slot.is_some()` plus a `NameSource` test.
The reanalyze/no-reanalyze distinction and the "must run after `resolve_closures`" prose (C8)
are reviewed for what still needs them. P1b's `NameSource::Debug` rung (K3) becomes a
`rename_slot` from the debug scope chain.

## 5. Risks

- **Stamp loss** is the main one, and K4.2 exists to measure it before anything depends on
  it. A stage that reads stamps must say what it does for an unstamped `Var` (today's text
  behaviour, never a guess).
- **Cache and memory.** `Option<SlotRef>` is 12 bytes per variable in IR held for 62,909
  functions; measure peak RSS and run time at K4.1 and K4.2 against the baseline run.
- **Inlining and hoisting** move statements between bodies. Absolute `SlotRef`s make that
  safe for identity, but I2 is per *rendered* scope: K4.3's checker has to evaluate it where
  codegen actually places the code, or it will report clean while the output collides.
- **Behaviour stages interact.** C10 showed that correcting one resolver changes what the
  next one sees. Hence one resolver per commit in K4.5, each measured against the previous
  commit, not against the baseline.

## 6. Non-goals

- Changing what names are chosen. K4 makes names consistent with identity; whether `obj` or
  `customAttributes` is the better name stays with W10/W11 and IPA.
- The 1,653 unparsable module chunks. They bound what the undeclared count can see, and are
  `gates.sh`'s parse check's concern; a stage that changes that number says so.

## 7. What K4.1–K4.3 measured

**[measured]** Equinox 11.43.0 (v96), release, `--no-cache`. Byte-identity was checked against
`f303870`'s output on Equinox and CoachRx 2.1.223 at every stage, and on Discord 350.8 (v98,
68.5 MB bundle, 170 MB output, ~18 min while sharing the machine) as a third bundle: K4.1,
K4.2 and K4.3 each identical.

**Stamp census** (`RUST_LOG=slots=debug`), stamped references by stage:

| checkpoint | captures | own | dropped (upper bound) |
|---|---|---|---|
| after W6 | 311,458 | 77,938 | — |
| after W11e | 285,321 | 77,938 | 0 (W11a folds `dependencyMap[N]` away) |
| after W13 | 208,022 | 55,728 | 64 (inlining; hand-checked: artifacts) |
| after W16h | 167,439 | 62,713 | 0 (hoisting moves captures to own) |
| at codegen | 161,779 | 58,446 | ~230 (no single render step drops one) |

**Identity violations** (`check_slot_identity`):

| | end of naming | at codegen |
|---|---|---|
| I1 one slot, several names | 828 | 2,294 |
| I2 two slots, one name | 62 | 37 |
| I2 capture beside a local | 3,798 | 1,873 |
| I2 capture beside a parameter | — ¹ | 110 |
| I2 capture shadowed by an intermediate scope | 73 | 167 |
| I3 own slot undeclared | — ¹ | 1,270 |
| I3 capture declared as a local | — ¹ | 306 |
| I3 declaration nothing reads | — ¹ | 217 |
| aliases (own slot spelled like the owner's local/param; not a violation) | 25,635 | 4,936 |

¹ Parameters are not named and declarations not inserted before codegen.

CoachRx at codegen: I1 2,807, I2 1,428, I3 1,467.

**Known cases.**
- `useAnimatedPropsLifecycleWithPrevNodeRef` (F4094): I1 on its slot 0 (`closure_0` in the
  owner, `current` in F4097/F4098), I2 capture-beside-local in F4097 and shadowed in F4098 —
  C10's "capture named `current` beside a local `current`", exactly.
- `genMethod` (F478): I1 on slot 1 — the owner binds `let closure_1 = index;` and its readers
  call the slot `index`. Harmless in the output today (the parameter is never reassigned),
  still two names for one slot, which is the shape that breaks when it is.
- `resolveThenable`: no violation, and correctly so. The current output is consistent
  (`const closure_0$2 = status;`, readers use `closure_0$2`); C10's regression was in an
  experiment that did not ship.

**What this changes for the later stages.**
- I1 nearly triples between naming and codegen: the late, per-body renamers are a source of
  their own (K4.6b), not just the resolvers K4.5 lists.
- I3 is three different defects (K4.4a/b/c). `prompt` (F2568) shows all of one chain: the
  owner reads `cancelButtonKey` undeclared, and the child that should write it declares
  `let closure_3 = String(index)` instead — the value never reaches the owner.
- The 1,091 bare `let closure_…;` in §1 overstated the unused-declaration problem; see K4.4c.
