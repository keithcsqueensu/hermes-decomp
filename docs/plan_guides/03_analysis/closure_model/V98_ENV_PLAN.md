# Impl plan — the v98+ environment model

> **Ownership.** Split out of `K4_PLAN.md` K4.2b. *Owns* how the IR builder and the closure
> context decide which scope an environment access reaches, on HBC v98 and later. *Delegates*
> naming, stamps and `rename_slot` to `K4_PLAN.md`; this plan only changes which `(owner, slot)`
> an access is, never what that slot is called.

Conventions follow `PLAN.md`. **[code]** is `file:line` at the time of writing; re-derive it
rather than trust it. **[measured]** is a number produced by running the tree.

## 1. Why a plan, not another fix

The closure model was built on v96, where an environment is reached one way: `GetEnvironment
rD, N` walks `N` up from the running function, and every environment is a function's own. On
that bytecode a level is a function hop, and `ClosureContext::ancestor_at` was right.

v98 bundles reach environments in ways the model has no word for. Each one was found the same
way: a Discord diff looked wrong, a chain of `disasm` calls found a new shape, and a narrow fix
went in. Five shapes have been found so far, four of them while doing K4.2b:

| # | Shape (v98) | Found | What the model did |
|---|---|---|---|
| S1 | An async wrapper with **no environment** creates its generator body on its parent's (`CreateGenerator r, rParent`) | K4.5.3 | Counted function hops, so the body's closures named the wrapper as an owner |
| S2 | A **block environment built on an outer one**: the generator body's scope is `CreateEnvironment r8, r7, 3` with `r7 = GetParentEnvironment 1` | K4.2b | Gave every block scope the running environment as its parent |
| S3 | **Environments parked in slots.** The body stores 41667's environment in its slot 2 and its own scope in slot 1, and later states reload them (`LoadFromEnvironment r5, r1, 1`) | K4.2b | Treated each reloaded environment as a block scope of the function, keyed by slot number alone |
| S4 | **Three-operand `GetEnvironment dst, start, N`**, walking up from an arbitrary environment register, including one loaded from a slot | K4.2b | Added `N` to the start register's `level_of`, which is stale for a slot-loaded register |
| S5 | A generator body lays out its **states out of order**: a `CreateClosure` with the scope comes before the `CreateEnvironment` that builds it | K4.2b | The builder scans linearly, so the scope's parent was not yet known |

S1, S2 and S5 are fixed on `feat/k4-2b`. S3 and S4 are not. The `CreateClosure` record now uses
`env_level_of`, but a reloaded environment is still identified as `NESTED_ENV_LEVEL_BASE +
slot`. Two different environments in slot 1 of two different scopes therefore share one
identity, and the environment of a real ancestor (41667's, in slot 2) is printed as a fake
block scope of the body.

The pattern is the problem: each fix teaches the level arithmetic one more case, and nothing
says how many cases are left. The census below says there are many.

## 2. Census

**[measured]** `hermes-decomp disasm` of the whole bundle, opcodes by operand count. "Slot-held"
is a register written by `LoadFromEnvironment` and then used as an environment operand. That
count is a linear scan per function with no CFG, so it is approximate in both directions.

| | Equinox 11.43.0 (v96) | Expensify 9.5.14 (v98) | Discord 350.8 (v98) |
|---|---|---|---|
| functions | 63,957 | 110,147 | 158,673 |
| `GetEnvironment` (2 operands) | 60,083 | — | — |
| `GetParentEnvironment` | — | 87,734 | 152,250 |
| `GetEnvironment` (3 operands, S4) | — | 1,858 | 5,152 |
| `CreateEnvironment` (1 operand) | 20,661 | — | — |
| `CreateFunctionEnvironment` | — | 20,359 | 31,841 |
| `CreateEnvironment` (3 operands, S2) | — | 2,482 | 3,066 |
| `CreateTopLevelEnvironment` | — | — | 6,173 |
| `GetClosureEnvironment` | — | — | 295 |
| `CreateGenerator*` | 907 | 807 | 2,564 |
| loads through a slot-held env (S3) | 0 | 3,421 | ~24,000 |
| stores through a slot-held env | 0 | — | ~4,500 |
| closures created with a slot-held env | 0 | 26 | 294 (222 functions) |

None of S2–S4 occurs on v96. That is why v96 stays byte-identical through every fix so far.
It is also why the v96 gates say nothing about v98 here.

## 3. Goal

Every environment access on every version resolves to **the scope that owns the environment**,
by tracking environments as values instead of inferring them from levels:

- An environment value is a scope plus a parent, and it can be stored in a slot and loaded
  back.
- A closure's chain is the list of scopes reached from the environment it was created with.
- Levels stay in the IR as they are. The context derives "level `L` of function `f` is scope
  `X`" from that chain, so `ancestor_at`, stamps and every walker read one answer.
- An access the model cannot resolve is **counted and named**, never guessed. Today an unknown
  register defaults to level 0, the running function (`EnvRegMap::level_of`).

**Not in scope:** naming, and the JSX tag problem in §6, which this plan only measures.

## 4. Phases

### E0 — measurement that can see the problem

1. **Parse JSX in `undeclared.mjs`** (acorn-jsx). Today any module printed as JSX counts as
   "unparsed", so the K4.2b plan note read "16 modules no longer parse" when 16 modules had
   simply become JSX. Until this lands, `unparsedChunks` is not a breakage signal on React
   bundles.
2. **`scripts/measure/env_census.sh`**: the §2 table, checked in and run on each bundle.
3. **An unresolved-access count** printed at `RUST_LOG=slots=debug`, next to I1–I3. It starts
   as "accesses through a slot-held env" and later becomes E3's "unresolved".

Exit: the §2 table and the unresolved count reproduce from scripts.

### E1 — ground truth from source

hermesc for v98 and v99 is on disk (`C:/src/hermes-v98/build/bin/Release/hermesc.exe`,
`…-v99/…`). Write small JS fixtures that each isolate one shape, compile them at v96, v98 and
v99, and record each environment access's real scope from the source:

- async function, async arrow, async method (S1)
- generator, async generator, a closure created inside a later generator state (S3, S5)
- a `let` captured in a loop body, a `catch` binding captured, a class with static blocks and
  private members, default parameters that capture (S2)
- three nesting levels of each, so a wrong hop count shows up as a wrong scope rather than
  coinciding with the right one

Each fixture asserts two things:
- the decompiled output re-runs under `hvm` with the same printed result (`tests/vm_verify.rs`
  already drives `HERMES_VM_V98`/`V99`);
- the `(owner, slot)` of each capture matches the source scope.

Open question: whether `hermesc -g` debug scope descriptors can produce the expected scopes
automatically instead of by hand. Check this first; it decides how cheap fixtures are.

Exit: each S1–S5 has a fixture that fails on `main` and passes on `feat/k4-2b` (S1, S2, S5)
or is marked as expected-failing (S3, S4).

### E2 — environments as values in the builder

Replace `EnvRegMap`'s per-register integers (`reg_level`, `reg_source_slot`, `created_envs`)
with an abstract environment value per register:

```
Env = Own                       // CreateFunctionEnvironment (first), v96 CreateEnvironment
    | Closure(n)                // GetParentEnvironment n / v96 GetEnvironment n
    | Up(Env, n)                // 3-operand GetEnvironment dst, start, n
    | Created(k, parent: Env)   // 3-operand CreateEnvironment / CreateInnerEnvironment
    | InSlot(Env, slot)         // LoadFromEnvironment of an environment
    | Unknown                   // GetClosureEnvironment, a merge of different values
```

It is computed over the CFG to a fixpoint, not by a linear scan, which removes S5 as a class.
Per function it emits facts, not levels:
- each environment it creates, with its parent;
- each environment value it stores into a slot;
- each closure it creates, with the `Env` it was given;
- each load and store, with the `Env` it went through.

The IR's `ClosureVar { level, slot }` is still produced from these as today, so nothing
downstream changes in this phase.

Exit: on every fixture and bundle, the IR is byte-identical to `feat/k4-2b`, and the facts are
dumped (`HD_ENV_DUMP`).

### E3 — whole-program scope graph

Build the graph from E2's facts:
- **Nodes** are scopes: function environments and created environments.
- **Edges** are parents.
- **`InSlot(E, s)` resolves** to the environments stored into slot `s` of the scope `E`
  denotes, by any function that runs on that scope. In S3, the body's first state stores both
  environments into its wrapper's slots and its later states load them back.

Iterate to a fixpoint. An `InSlot` with no store, or with stores of different environments, is
`Unknown` and is counted.

This graph replaces `env_given`, `creators`, `block_parents`, `creation_scopes` and the
`NESTED_ENV_LEVEL_BASE + slot` identity. Each function gets its chain, the list of scopes at
levels 1, 2, …, and `env_hops` / `ancestor_at` read it.

Exit: E1's fixtures all pass, including S3 and S4, and the unresolved count is reported per
bundle.

### E4 — switch over and measure

Stamps (W6), `slot_owner`, `resolve_baked` and the walkers that still count
`parent_function` hops all read the E3 chain. Some of those walkers are in `metro/propagation`,
`depmap_rewrite`, `batch.rs`, `codegen.rs` and `closure_definitions.rs`. Grep for
`parent_function.get` and decide each one: scope walk or function walk.

Measured like a K4 stage:
- v96 byte-identical;
- v98 I1, I2 and I3;
- undeclared references per module, with the `worse:` list classified by hand;
- gates.

Exit: K4.5.3 and K4.5.4 unblock. Their first measurement went wrong only because stamps were
wrong under S1.

## 5. Order with K4

1. Land **K4.2b** (`feat/k4-2b`: level remap plus block parents) once its measurement is
   classified. It fixes S1, S2 and S5 with small code, and E1's fixtures measure it.
2. **E0, then E1.** These are cheap and they change how every later step is judged.
3. **E2, then E3, then E4.** K4.5.3, K4.5.4 and K4.6 wait for E4 on v98. On v96 they could go
   ahead now, but measuring them on v96 alone is how C10-type regressions got through before.

## 6. Found on the way (not this plan's to fix)

- **Lowercase JSX tags.** `<closure_0 />`, `<guildId …/>` and `<require …/>` are printed as JSX
  tags. In JSX a lowercase tag is an intrinsic element (a string), not the component, so the
  output means something else. **[measured]** Discord on `main` has 299 placeholder tags and
  about 1,800 other lowercase non-HTML tags. The JSX emitter should fall back to a call (or
  capitalize) for a name that is not a valid component reference. Belongs to the JSX plan.
- **`GetClosureEnvironment`** (295 on Discord) is ignored by the builder
  (`handle_get_closure_environment`). It is E2's `Unknown` until a fixture shows what it
  reaches.
- **A second `CreateFunctionEnvironment`** in one function is taken as a block scope built on
  the running environment (`claim_function_env`). Its parent is the closure environment. E2
  should model it as `Created(k, Closure(0))` and E1 needs a fixture for it.
