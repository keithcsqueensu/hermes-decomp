# Impl plan — lifting v98 generator machines

> **Ownership.** *Owns* turning HBC >= 97 generator and async state machines back into
> `function*` / `async function` bodies when the yields sit under control flow. *Delegates*
> the naming and declaration of a machine's variables while it stays raw to
> `../03_analysis/closure_model/IDENTITY_PLAN.md` A2e, and the catch-loop dominator change
> (A2e3, A2e4) is ordered against this plan's G2 below.

Status: planned 2026-10-29, nothing landed.

## 1. Where things stand

HBC v97 removed the generator opcodes. `hermesc` lowers `function*` (and `async`, through a
generator) to a step function that dispatches on a label slot: each case runs from one
resume point to the next suspend (`label = N; status = 1; return { value, done: false }`),
and starts with the resume protocol (`if (arg0 === 1) throw arg1; else if (arg0 === 2)
return { value: arg1, done: true }; x = arg1`). `state_machine_v98` rebuilds the body after
structure recovery, from the printed shape of the cases, and lifts a machine only when every
case is a straight run to its suspend.

Census on Expensify (`--log genlift=trace`, 2026-10-29): the lifter gives up 492 times.

| Reason | Count | What it is |
|---|---|---|
| unparsable case with no throw | 419 | a case whose suspend sits under an `if`, a loop or a `switch`; the case is a region, not a run |
| two terminal cases with content | 69 | two ways to finish, which this pass cannot order |
| no terminal case | 3 | |
| `emit_yield_chain` | 1 | |

Raw machines: Expensify 536, Discord 1,738. The round-trip corpus already has the main shape
as a known failure: v98 `async_loop`, an `await` inside a `for` (`roundtrip_known_failures.tsv`).

The cause is where the lift happens. After structure recovery, a loop around a suspend is
gone: the loop's body is split across cases, and the back edge is a store of an earlier
label. On the CFG, before structuring, it is still a graph: a suspend is an edge from the
suspend block to the resume block of the label it stores. Rewriting that edge as a `yield`
statement followed by a jump, and dropping the dispatch, gives an ordinary function CFG, and
the existing structurer recovers the `if`s and loops around the yields.

## 2. Goal and non-goals

Goal: a machine whose cases form a graph through label stores lifts, whatever control flow
surrounds its suspends, and prints as a `function*` or `async function` that runs as the
source does.

Not in this plan: v96 (it has real generator opcodes); naming the machine's variables (A2e,
then A3 step 4); the Babel / regenerator wrappers that W15 already unwraps.

## 3. Stages

Each stage reports raw machines per bundle, the bail census, the round-trip results of the
generator snippets, and unparsable / undeclared / consts written on every bundle. v96 output
must not change in any stage.

### G0 — Fixtures and a ratchet (small)

- Corpus snippets, compiled for v98 and run by the round-trip gate: `yield` under `if`/`else`,
  in a `for`, in a `while` left by `break`, inside `try/catch`, inside `try/finally`, two
  `return`s with values, `yield*`, an `await` in a loop (`async_loop`, there already), and an
  `await` inside `catch`.
- The census split by machine kind (generator, async) and by the shape of each case that
  fails, logged with `genlift`.
- A gate ratchet on raw machines per bundle (ceiling today's count), like unparsable and
  consts written.

Exit: the snippets are in the corpus with their current verdicts recorded as known failures;
the ratchet is in the gates.

### G1 — The machine read off the CFG, log only (medium)

On the step function's CFG: the dispatch block and its label slot, each case's entry by
label, each suspend site (the label it stores and the result it returns), each resume entry
(the block after the protocol), and the protocol branches. Nothing is rewritten. Counted
against `state_machine_v98`: on every machine it lifts, both readings find the same cases and
suspends.

Exit: agreement on every machine the current lifter lifts, on all three bundles; each machine
G1 cannot read is classified.

### G2 — The CFG rewrite, for the machines the current lifter refuses (large)

Each suspend becomes `[resume =] yield value` (or `await`) and a jump to the resume entry of
the label it stores. The protocol branches go: `.throw` and `.return` are what `yield`
itself does. The status and label bookkeeping goes, and the entry jumps to the first case.
The structurer then runs on the result as on any function. Only machines the current lifter
refuses go through it, so no output it produces today changes.

The loop-shaped dispatch that blocked the catch-loop fix (IDENTITY_PLAN A2e3) does not exist
after the rewrite. A2e3 is re-read once G2 lands, and may reduce to the dominator change
alone (A2e4).

Exit: raw machines down on both v98 bundles, with the remainder classified. The snippets for
`if`, loops and two returns pass. Unparsable, undeclared and consts written do not rise.

### G3 — try/catch/finally inside a machine (medium)

The machine's own handler regions (the catch labels the current lifter skips) become `try`
statements around the resumed regions they protect. A `finally` that runs on `.return` is
kept as a `finally`.

Exit: the `try` snippets pass; the census row for catch-carrying cases is 0.

### G4 — async (medium)

`spawnAsync` wrappers and the W14 detection: an async step's suspends become `await`. The
`async_loop` known failure passes and is removed from `roundtrip_known_failures.tsv`.

### G5 — One lifter (medium)

Both lifters run on every machine, and on each machine the old one lifts the outputs are
compared. Where the CFG lifter is equal or better on all of them, it replaces
`state_machine_v98`'s case parsing (the two-terminals rule, catch-label skipping and the exit
flattening go). Each difference is read before the switch.

### G6 — Cleanup after a lift (small to medium)

The follow-ups found with the catch-loop: inside newly lifted bodies, results are kept and
loader temporaries are not inlined, so imports are not lifted. Each is fixed with its own count.

## 4. Order and size

G0 → G1 → G2 → G3 → G4 → G5 → G6. G0 and G1 change no output. G2 is the large one. G3 and G4
can be done in either order after it. In IDENTITY_PLAN, A2e1 and A2e2 (one name per parked
slot) go first or alongside, because a lifted machine's variables come from the same parked
scope.

## 5. Risks

- **A wrong lift is worse than a raw machine.** A raw machine is verbose but runs as the
  bytecode does. G2 lifts only what G1 read completely; anything else stays raw and is counted.
- **Resume values.** The value a `yield` evaluates to is the resume parameter, read at the
  resume entry. Missed, a `const x = yield v` prints as a bare `yield v`, which parses but
  loses data. The snippets check the values that flow back in.
- **Line diffs.** Lifting rewrites whole functions. Judge by the counts, the round trip and
  the census, and read samples by class.
