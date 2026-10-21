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
  assignment.
- **K4.4c — drop declarations nothing reads as this binding** (I3 `Unused`, 217). The
  1,091 bare `let closure_…;` in §1 is *not* this number: most of those are hoisted
  declarations of destructuring targets the body does assign (`let closure_3; … [tmp4,
  closure_3] = …`). The checker counts a bare `let` as unused only when no reference in the
  body, and none below that resolves to this function's slot or is unstamped, reads it.

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
