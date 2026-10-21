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
K4.0 harness ──► K4.1 Var ──► K4.2 stamp ──► K4.3 checker ──┬─► K4.4 declare by identity
   (this commit)   (repr)       (repr)          (repr)        ├─► K4.5 resolvers by identity ──► K4.6 rename_slot ──► K4.7 enforce I1/I2
                                                              └─────────────────────────────────────────────────────────► K4.8 retire
```

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

### K4.3 — the identity checker — diagnostic, byte-identical
`check_slot_identity(all_ir, ctx) -> Violations { i1, i2, i3 }`, run at the end of naming
when `RUST_LOG=slots=debug`, and in a corpus test that fails when the count on
`HBC_CORPUS_BUNDLE` exceeds a recorded ceiling (the pattern `syntax_baseline.tsv` uses). It
reports, for each violation, the `SlotRef`, the bodies and the names.

This replaces guessing with a count: the number of C9/C10-class defects left in the output,
exactly. Every later stage is judged by it falling. *Acceptance:* byte-identical; the
ceilings recorded; the known cases (`genMethod`, `resolveThenable`,
`useAnimatedPropsLifecycleWithPrevNodeRef`) each show up, or are explained as not violations.

### K4.4 — declare by identity — behaviour
`own_env_slot_names` / `ancestor_env_slot_names` (`pipeline/context/rendering.rs:201,434`)
compute from the stamped IR — the names the body actually uses for slots it owns, and the
names its ancestors' slots carry — instead of from the context. This is the step K4-small
could not take: there is no second name to disagree with.

*Expected:* fewer bare `let closure_…;` (1,091 at baseline) and fewer undeclared slot names;
I3 to zero. *Acceptance:* the behaviour gates.

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
