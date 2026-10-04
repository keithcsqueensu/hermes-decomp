# Impl plan — the closure / env-slot model

`../../01_read/unmodeled_regions/PLAN.md` backlogs **P1b** ("put the recovered debug names in the
decompiler") on a cost/benefit call: the payoff is debug-build cosmetics, and the plumbing to
get there is large. That call is right. This plan is about the plumbing it named, because
the plumbing is **not** debug-info plumbing — it is the crate's model of closures, it is
load-bearing for output the crate produces *every run on every bundle*, and it is where the
coupling the P1b note describes actually lives.

Restated: P1b reads as "the read side (debug info) is coupled to the write side (naming)".
The measurements below say the coupling is somewhere else. There is one **structured** fact —
"this value is environment slot `S`, `L` levels up" — that the crate discovers, immediately
renders into a **string**, and then spends seventeen sites in ten files (twenty-five in fifteen
today, §2) trying to parse back out. Debug info is simply one more producer that has nowhere to plug in. Fixing that is
worth doing on its own terms; P1b then becomes a consequence rather than a project.

> **Ownership.** Split out of `../../01_read/unmodeled_regions/PLAN.md` P1b, which is blocked on it.
> *Owns* the decompiler's closure / environment-slot model: how a capture is represented,
> named, and rendered. *Delegates* the debug section's on-disk format, and the question of
> emitting it, to `../../01_read/unmodeled_regions/PLAN.md`. Nothing here is write-path work.

Conventions follow the sibling plans. **[code]** is `file:line` at the time of writing —
re-derive rather than trust. **[measured]** is a number produced by running the tree, and
every one below is reproducible from §8.

Tree state: branch `feat/write-path-hardening`, `b7b61b2`, clean. **[code]** refs below were
re-derived at `main` `81c4e2a`, after the IR's three named reads/writes collapsed into one
`Binding` type and upstream v0.2.3/v0.2.4 were merged. **[measured]** numbers in §2 are still
the `b7b61b2` run except where marked re-measured; the 11.39.0 bundle is no longer on disk.

**Status at `81c4e2a`.** C3 is fixed (upstream `8ab1537`, see C3); half of K2 has therefore
landed. Upstream also added the C5 discriminator in the IR builder, though not where K1 puts it
(see C5 and K1). C1, C2, C4, C6, C7 and C8 stand. No K-phase has shipped as specified.

---

## 1. The model as it stands

The IR has a first-class node for an environment capture, produced by the bytecode reader:

```
Binding::ClosureVar { level: u32, slot: u32 }        [code] ir/types.rs:63
Value::Binding(..) / AssignTarget::Binding(..)       [code] ir/types.rs, ir/stmt/mod.rs:168
```

emitted by `handle_load_from_environment` / `handle_store_to_environment`
(`ir/builder/opcodes_environment.rs:86,108`), with `level` tracked across registers by
`EnvRegMap` (`ir/builder/env_state.rs`).

That node survives to pipeline stage **W6**, where `resolve_closures` **replaces it with a
string**:

```rust
// analysis/closure/mod.rs:272  (and :189 for the AssignTarget side)
Expression::Value(Value::Binding(Binding::ClosureVar { level, slot })) => {
    let encoded = encode_level_slot(level, slot);
    let name = if info.slots.contains_key(&encoded) { info.get_slot_name(encoded) }
               else if level == 0            { info.get_slot_name(slot) }
               else { crate::ir::Value::closure_var_name(level, slot) };
    info.record(&name, level, slot);
    Expression::Value(Value::Binding(Binding::Variable(name)))   // ← the structure ends here
}
```

Everything after W6 sees `Variable("closure_3")` or `Variable("dependencyMap")` and cannot
tell which is a real recovered identifier and which is a placeholder — except by looking at
the spelling. (The one exception since this was written: `info.record` keeps a side table,
`ClosureContext.baked_captures`, of baked name → `(level, slot)`, which the late inherit passes
in `transforms/var_naming/ancestor_inherit.rs` and `capture_sync.rs` consult — the parallel map
C6 predicts, keyed by the very spelling it records.) So it looks at the spelling:

```
                    ┌──────────────── the only lossless form ────────────────┐
reader ─► ClosureVar{level,slot} ─► ClosureInfo::get_slot_name ─► "closure_3" ─┐
                    └────────────────────────────────────────────────────────┘  │
                                                                                ▼
   W5e phases.rs:585   parse_closure_binding(name)               ── (level, slot), recovered
   W8  ipa/traversal   target_to_key → "closure_0_3"             ── a *different* spelling
   W10 closure_usage.rs:147  is_closure_name(name)               ── "is this a placeholder?"
   W11 closure_def_naming.rs:116  is_closure_name(name)          ── same question again
   …   ~21 more sites                                            ── §2
```

The `.hdcache` on-disk snapshot serialises `ClosureContext` verbatim
(`pipeline/cache.rs:117`, `CACHE_VERSION = 5` at `:33`), so any change to `ClosureSlotValue`
is a cache-format change.

### The naming ladder, and what it has no room for

`ClosureInfo::get_slot_name` (`analysis/closure/info/naming.rs:36`) is a fixed fallback
ladder over `ClosureSlotValue`:

| `ClosureSlotValue` | rendered | provenance |
|---|---|---|
| `Function { name: Some(n) }` | `n` | closure creation site |
| `Function { name: None }` | `f{id}` | synthesised |
| `RegExp` | `re{slot}` | store analysis |
| `Constant(c)` | text of `c`, else `c{slot}` | store analysis |
| `Variable(v)`, v meaningful | `v` | IPA / Metro / register naming |
| anything else | **`closure_{slot}`** | nothing known |

The enum carries a *value*, never a *provenance* or a *confidence*. There is no variant, and
no side channel, that means "Hermes itself told us this binding is called `count`". A debug
name is strictly better evidence than every rung on that ladder, and the ladder has no top.
That, not the codegen layer, is why P1b has nowhere to put its result.

---

## 2. Measurements

**[measured]** — `hermes-decomp decompile` (release) over
`com.equinoxfitness.equinox_11.39.0/hermes_bundle/assets/index.android.bundle.backup`
(v96, 62,909 functions), 17.0 s, output 41,447,553 B / 959,894 lines — byte-identical in size
to the figure `../../01_read/RISKS.md` records, so this is the shipped behaviour, not a variant.

| quantity | count |
|---|---|
| rendered env-slot placeholders in the output | **94,453** |
| — level-0 form `closure_N` | 60,802 |
| — level ≥ 1 form `closure_L_S` | **33,651** |
| distinct placeholder tokens | 1,246 |
| `c{N}` constant-slot placeholders | 5,311 |
| debug-info variable names available in this bundle | **0** |

**[measured]** — sniffer census: `grep -rn '"closure_"' crates/hbc-decomp/src/` returns
**17 sites across 10 files**, spanning `analysis/closure`, `analysis/metro`, `pipeline`,
`transforms/codegen`, `transforms/inline` and `transforms/var_naming`. Each is an independent
re-derivation of a fact the IR node held exactly. **Re-measured at `81c4e2a`: 25 sites across
15 files** — the same directories plus `analysis/ipa` — so the census has grown, not shrunk.

**[measured] — the debug→name path is inert end to end.** On
`tests/fixtures/locations.debug.v96.hbc`, the one fixture in the tree compiled `-g3` with
captured variables (`count`, `first`, `second`, `third` — confirmed present via
`debug --scopes`, scopes 9 and 16), replacing the body of
`DebugInfo::variable_map_for_function` with `BTreeMap::new()`, rebuilding, and decompiling
cold-cache produces output **byte-identical** to the unmodified build. Both consumers
(`pipeline/mod.rs:158`, `pipeline/ir_gen.rs:312` at `81c4e2a`) are dead in effect. The output still reads:

```js
globalThis.makeCounter = function makeCounter(arg0) {
  closure_0 = arg0;                        // ← Hermes named this `count`
  function bump(arg0) { const sum = closure_0 + arg0; … }
};
```

(experiment reverted; tree clean).

---

## 3. Findings

### C1 — the slot→name map is consumed as a register→name map **[code]**

`build_variable_map` keys by position within the scope descriptor — an **environment slot
index** (`debug.rs:581`, `var_map.insert(i as u32, …)`; P1b's own rule is `names[S]` for slot
`S`). Both consumers hand it to `rename_registers`, which matches the key against
`Binding::Register(r)` (`analysis/naming/renaming.rs:148,217`):

- `pipeline/mod.rs:176` — `if let Some(name) = debug_names.get(&r)`
- `pipeline/ir_gen.rs:318` — `rename_registers(statements, &debug_names)`

Slot 0 and register `r0` are different index spaces. This is a type confusion, not a
threading gap, and the doc comment on `variable_map_for_function` (`debug.rs:601`) states the
wrong space — "keyed by register index".

It has never produced a visibly wrong name, for two reasons that are both luck: the Equinox
bundle carries no debug names at all (0, §2), and on the fixture the registers that would
collide have been eliminated by propagation before stage F23 runs. The tests do not catch it
because `v96_recovers_a_real_variable_name` (`tests/debug_locations.rs:130`) reads the map
with `.into_values()` and never asserts anything about a key.

Consequence for P1b: the "thread the names through" work is not *starting*; it is *undoing* a
wrong connection first. That is worth knowing before estimating it again.

### C2 — two spellings of the same slot, one of which is documented as an invariant **[code]**

```rust
Value::closure_var_name(0, 5)   → "closure_5"      ir/types.rs:132
target_to_key(ClosureVar{0,5})  → "closure_0_5"    ir/utils.rs:72
```

`transforms/codegen/format.rs:136` carries the comment *"Must match `Value::closure_var_name`
so load/store of the same captured slot use the same identifier"* — the invariant is known
and stated, and `Binding::name()` (`ir/types.rs:73`) now delegates to it too.
`target_to_key` silently breaks it for `level == 0`, and its output is used as a
**rename-map key** matched against printed names by `rename_variables_in_stmts`
(`analysis/naming/renaming.rs:311`), at `metro/propagation/mod.rs:347` (and three more sites
in that file) and `ipa/traversal/mod.rs:68`. The spelling-parsers disagree about it too:
`parse_closure_binding` (`metro/propagation/phases.rs:585`) reads `closure_0_N` as `(0, N)`,
`parse_closure_capture` (`var_naming/closure_definitions.rs:192`) rejects it.

Blast radius is narrow but real: those sites run at W8/W9, and W9's own comment
(`pipeline/context/naming.rs:187`) confirms residual `ClosureVar` nodes still exist at that
point. A rename keyed `closure_0_5` can never match the `closure_5` that everything else
prints, so it no-ops. Silently — a rename that matches nothing looks exactly like a rename
that had nothing to do.

### C3 — 33,651 placeholders are excluded from naming by their spelling **[measured] [code]**

> **Fixed upstream** (`8ab1537`, merged with v0.2.3). `is_closure_name`
> (`transforms/var_naming/closure_usage.rs:147`) now splits the suffix on `_` and requires every
> part to be digits (`is_closure_numeric_suffix`), so `closure_1_5` passes the gate and the
> `collect_existing_names` reservation below no longer catches it. It does not delegate to a
> shared parser as K2 proposed: `parse_closure_slot` is gone, and its successor
> `parse_closure_capture` (`closure_definitions.rs:192`) is a separate re-derivation. The
> W10/W11 gate sites are now `closure_usage.rs:234,405,437,464,519` and
> `closure_def_naming.rs:116`. The 33,651 figure is the `b7b61b2` measurement and has not
> been re-taken; the text below is the finding as written.

`is_closure_name` (`transforms/var_naming/closure_usage.rs:119`) accepts `closure_{digits}`
and `c{digits}` and **rejects `closure_1_5`** — the level ≥ 1 form — because the suffix
`"1_5"` is not all digits. It is the gate on both naming passes:

- **W10** cross-function usage naming — `closure_usage.rs:212,239,265,302`
- **W11** definition-site naming — `closure_def_naming.rs:109`

So 33,651 of 94,453 placeholders (**35.6 %**) in the shipped Equinox output are structurally
unreachable by either pass, purely because of how they were spelled. Nothing in the code says
this was intended.

The same file family contains the contradiction: `parse_closure_slot`
(`closure_definitions.rs:151`) **does** parse `closure_1_5` — its doc comment enumerates the
case — but every caller is behind the gate that rejects it. One helper understands the
spelling, its guard does not.

A third-order effect: `collect_existing_names` (`closure_definitions.rs:215,222`) reserves
any name for which `is_closure_name` is false, so level ≥ 1 placeholders are also reserved as
if they were real user identifiers, constraining `make_unique_name` away from names it should
be free to use.

### C4 — the parent chain is derived twice and joined nowhere **[code]**

Two independent parent hierarchies over the same program:

| | source | field |
|---|---|---|
| IR side | `CreateClosure` parentage | `ClosureContext.parent_function` (`analysis/closure/context/mod.rs:15`) |
| debug side | `ScopeDescriptor.parent_offset` (`debug.rs:74`) | walked by nothing |

The debug chain is parsed, exposed, printed by `debug --scopes`, and consumed by no analysis.
P1b's rule ("walk `L + h` parent links up the debug scope chain") is a *third* traversal,
specified against the second chain, over a program where the first chain already exists and
is already correct. Any implementation of P1b that does not join them will drift from the
crate's own idea of who nests inside whom.

### C5 — `h` is measured; the crate guesses it **[code]**

P1b's discriminator is exact: **`h` is 0 if `F` creates its own environment, 1 if it does
not**, and the test is whether `F`'s body contains `CreateEnvironment`. The crate hits the
identical ambiguity and resolves it heuristically —

```rust
// analysis/closure/context/merge.rs:65
// Hermes GetEnvironment(0) in a nested function is often the *captured*
// parent environment (no local CreateEnvironment). …
// Also: if level-0 key is missing but ancestors have a stable slot, expose
// it at level 0 so Hermes-level-0 loads of the captured env resolve.   (:96)
```

— by merging ancestor slots down into level 0 with `or_insert`, over an "often". The bit that
would replace "often" with a fact is *observed and then discarded*:
`handle_create_environment` (`ir/builder/opcodes_environment.rs:11`) sets a register level and
returns `FlowResult::Noop`; nothing records that the function has its own environment.

This is the finding that most changes the picture. **P1b's measurement is valuable
independently of debug info.** Recording one bool per function — `has_own_environment` —
sharpens a heuristic that runs on every bundle, debug info or not, and it is the same bool
P1b needs. It should not be backlogged with P1b.

> **Partly addressed upstream** (`f85d2d1`, merged with v0.2.3). The bit is now measured:
> `IRBuilder` computes `owns_current_env` (`ir/builder/ir_builder.rs:167`, any
> `CreateFunctionEnvironment`, or a `CreateEnvironment` with fewer than three operands) and calls
> `EnvRegMap::set_borrows_current_env`, so `parent_env_level` shifts every
> `GetParentEnvironment` level out by one in a function that borrows its parent's environment.
> But it is spent at IR build time and not recorded: `ClosureContext` carries no such field,
> and the `merge.rs:65,96` ancestor-merge heuristics above are unchanged. Whether they are now
> redundant (or double-correcting) has not been checked. `handle_create_environment` itself
> still returns `FlowResult::Noop`, now after claiming a level via `claim_function_env` /
> `claim_created_env`.

### C6 — `ClosureSlotValue` has no room for a better source of truth **[code]**

Per §1: the enum is a value with no provenance. Adding debug names means either a new variant
whose priority is implicit in `get_slot_name`'s match order, or a parallel map that every
call site must remember to consult. Neither is a place; both are a place-shaped hole. Note
also that any change here is a `.hdcache` schema change (`pipeline/cache.rs:33`).

The second shape has since appeared: `ClosureContext.baked_captures` (§1) is a parallel map
from a baked spelling to `(level, slot)`, consulted by the two late inherit passes and by no
one else.

### C7 — codegen *does* have a context mechanism; the node is what is missing **[code]**

`Codegen` (`transforms/codegen/mod.rs:269`) already carries injected analysis context —
`import_map`, `dep_names`, `dep_ids`, `inline_bodies`, `nested_writes` — with an established
`with_imports` / `with_esm_mode` / `with_esm_module_meta` / `with_inline_bodies` /
`with_nested_writes` builder pattern. Adding a
`with_variable_names` is mechanically routine.

It would not work anyway, and that is the point: by print time the node is
`Variable("closure_3")`, indistinguishable from a source identifier that happens to be
spelled that way. Print-time substitution needs the structure to still be there. Codegen does
still handle `AssignTarget::Binding(Binding::ClosureVar)` (`format.rs:138`), so residuals reach it — but the
94,453 placeholders in §2 are overwhelmingly already strings by then.

### C8 — the ordering constraints are load-bearing and undertested **[code]**

`resolve_all_closures` (`pipeline/context/closures.rs:15`) documents a constraint that exists
only because of the lowering:

> `reanalyze`: … Use this on the **first** resolve pass only. A second pass after variables
> are already renamed must set `reanalyze: false` — re-scanning then would drop env-slot
> stores (they became plain `Variable` names) and wipe parent maps.

`metro/propagation/mod.rs:9` ("Late rewrite (after resolve_closures)") and
`depmap_rewrite.rs:7` ("MUST run AFTER `resolve_closures`") carry the mirror-image constraint. These are prose invariants around a one-way conversion,
enforced by comment. If the node survived, "has this been resolved yet?" would stop being a
question the pipeline has to keep track of.

---

## 4. Relationship to `../../01_read/unmodeled_regions/PLAN.md` § P1b

P1b's prose stated three things about this machinery that the code contradicts — that
`get_slot_name` had no pipeline callers, that `var_naming` renames only registers, and that
codegen has no context mechanism. **Those have been corrected in that plan directly**, in
place, rather than being tracked here; the sibling plans are explicit that where code and
prose disagree the code wins, and a correction that lives in a second document is one more
thing to keep in sync. P1b now points here for the blocker and states it as C1–C8 do.

**What survived the correction unchanged:** the payoff estimate. Hermes names only captured
variables, only in `-g3` builds, and the Equinox bundle has 0 (§2, re-measured). Debug-driven
naming remains cosmetics for debug builds. The argument of *this* document is that the
plumbing is not cosmetics — 94,453 placeholders per run say so — and that it should be
justified by its own payoff rather than by P1b's.

---

## 5. The shape of a fix

One sentence: **keep `ClosureVar` structured until codegen, and give a slot a name with a
provenance instead of a name.**

```rust
// ir/types.rs — the node stops being erased
Binding::ClosureVar { level: u32, slot: u32 }      // survives to codegen

// analysis/closure/info/types.rs — a slot's name gains a source
pub enum NameSource { Debug, Metro, Ipa, Definition, Usage, Synthesised }
pub struct SlotName { pub text: String, pub source: NameSource }
```

`get_slot_name` becomes the *renderer of last resort* rather than the sole namer, and
`NameSource` gives the ladder an explicit ordering with a top rung that debug info can occupy.
Every predicate in the §2 census becomes a field test (`source == Synthesised`) rather
than a `starts_with`, and C2/C3 stop being possible to write: there is no spelling to get
wrong and none to fail to recognise.

Two constraints on any such change:

- **`.hdcache` is a schema.** `ClosureContext` is serialised whole (`pipeline/cache.rs:117`);
  `CACHE_VERSION` (`:33`, currently 5) must be bumped.
- **The Equinox output is the regression test.** 41,447,553 B / 959,894 lines. Any step here
  should be judged by a diff against it, not by the fixtures — the fixtures have five
  functions and the bundle has 62,909.

---

## 6. Suggested phasing

Ordered so each phase is separately justified and separately shippable. **K1 and K2 pay for
themselves with zero debug info** and are the ones worth doing regardless of P1b.

### K1 — record `has_own_environment`; retire the "often" (C5) — open; the bit is measured, not carried
Set a per-function bool in `handle_create_environment`, carry it on `ClosureContext`, and use
it in `merge.rs:65,96` instead of the ancestor-merge heuristic. Small, local, and it is P1b's
measured discriminator arriving early. Acceptance: Equinox output diff is explainable
line-by-line — expect *changes*, and each one should be a level-0/parent capture that was
previously merged by guess. *Status:* upstream now computes the bool in `IRBuilder` and spends
it on a level shift (C5's note); what remains is carrying it to `ClosureContext` and retiring
the `merge.rs` heuristic, which first needs checking against that shift.

### K2 — one spelling, one predicate (C2, C3) — half landed: the predicate (C3) is fixed, the spelling (C2) is not
Make `target_to_key` delegate to `Value::closure_var_name`; make `is_closure_name` accept the
level form by delegating to `parse_closure_slot`, which already handles it. *Status:* the
second edit landed upstream by a different route (C3's note); `target_to_key`
(`ir/utils.rs:72`) still spells level 0 as `closure_0_N`. Two small edits
against the *current* string-based design — no refactor — that between them unblock 33,651
placeholders for W10/W11 naming. Acceptance: the level ≥ 1 placeholder count falls; total
placeholder count falls; no new `undefined`/collision in the Equinox output. **This is the
highest value-per-line item in the report** and it is independent of everything else here.

### K3 — `SlotName { text, source }` (C6)
Replace bare `String` in `ClosureSlotValue::Variable` and the `get_slot_name` return. Bump
`CACHE_VERSION`. Convert the §2 census sites from spelling tests to source tests, one file at
a time — each conversion is behaviour-preserving on its own. Acceptance: byte-identical
Equinox output at the end of the phase (this phase changes representation, not decisions).

### K4 — carry `ClosureVar` to codegen (C7, C8)
Stop lowering in `resolve_closures`; attach the resolved `SlotName` to the node; render at
`format.rs:138`. This is what removes the ordering prose in C8 and the reanalyze/no-reanalyze
distinction. Largest phase, and the only one that should wait for a reason.

### P1b, afterwards
With K1 (`h`), K3 (`NameSource::Debug` as top rung) and C1 fixed, P1b is: fix
`variable_map_for_function`'s key space, join the debug scope chain to
`ClosureContext.parent_function` (C4), and insert one `SlotName` per named slot. It stops
being a project. Its own payoff is still small — that part of the plan's judgement stands.

---

## 7. Non-goals

- **Emitting debug info.** Everything here is read-side and analysis-side. The write-side
  question is `../../01_read/unmodeled_regions/PLAN.md` § P6 and is untouched.
- **Making up names.** `closure_3` is honest. A wrong name is worse than a placeholder — the
  plan says so about P1b and it is equally true of K2: a level ≥ 1 capture becoming reachable
  by W10/W11 must still only be renamed on the same evidence W10/W11 already demand.
- **v97+ scope tables.** v98 removed the scope table; `DebugLayout::for_version`
  (`debug.rs:129`) already refuses what it has not derived. Nothing here changes that.

---

## 8. Re-deriving every number

```bash
cargo build --release -p hbc-decomp-cli
B=/c/apks/equinox/com.equinoxfitness.equinox_11.39.0/hermes_bundle/assets/index.android.bundle.backup
#   (11.39.0 is no longer on this machine, extracted or archived: re-baseline on the current
#    bundle before comparing against any number in §2)

# §2 — the placeholder census (note -a: the output contains bytes grep calls binary,
#      and without it the counts are silently short by ~4%)
./target/release/hermes-decomp.exe decompile "$B" 2>/dev/null > equinox.js
grep -ao 'closure_[0-9]\+\(_[0-9]\+\)*' equinox.js | sed 's/[0-9]\+/N/g' | sort.exe | uniq -c
grep -ao 'closure_[0-9]\+\(_[0-9]\+\)*' equinox.js | sort.exe -u | wc -l
grep -ao '\bc[0-9]\+\b' equinox.js | wc -l

# §2 — zero debug names in the shipped bundle
./target/debug/hermes-decomp.exe debug "$B" --vars

# §2 — the fixture's captured names, and the inertness experiment
./target/debug/hermes-decomp.exe debug crates/hbc-decomp/tests/fixtures/locations.debug.v96.hbc --scopes
#   then: stub DebugInfo::variable_map_for_function to BTreeMap::new(), rebuild,
#   rm -f crates/hbc-decomp/tests/fixtures/*.hdcache, decompile, diff. Revert.

# §2 — the sniffer census
grep -rn '"closure_"' --include='*.rs' crates/hbc-decomp/src/
```

`sort.exe`, not `sort` — see the alias trap in the machine notes. Delete stray `.hdcache`
files beside the fixtures after any experiment; a stale one will serve pre-change results.
