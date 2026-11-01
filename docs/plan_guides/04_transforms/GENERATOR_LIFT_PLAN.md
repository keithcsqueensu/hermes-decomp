# Impl plan — lifting v98 generator machines

> **Ownership.** *Owns* turning HBC >= 97 generator and async state machines back into
> `function*` / `async function` bodies when the yields sit under control flow. *Delegates*
> the naming and declaration of a machine's variables while it stays raw to
> `../03_analysis/closure_model/IDENTITY_PLAN.md` A2e, and the catch-loop dominator change
> (A2e3, A2e4) is ordered against this plan's G2 below.

Status: planned 2026-10-29. **G0 landed (2026-10-30).** **G1 landed (2026-11-01).** **G2 landed (2026-11-01).**

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

**Done (2026-10-30).** No output change on any bundle.
- Snippets (`examples/react-native/expressions/`): `gen_if`, `gen_for`, `gen_while_break`,
  `gen_try_catch`, `gen_try_finally`, `gen_two_returns`, `gen_delegate`, `async_catch`, beside
  `async_loop`. On v98 seven fail (raw machines) and `gen_two_returns` passes; all recorded in
  `roundtrip_known_failures.tsv` with the stage meant to fix each. On v96 three fail: G0b.
- Census (`--log genlift=debug`, one line per run; `trace` adds each machine and its reason),
  kept where every machine is decided (the wrapper check in W16b), by kind and reason; an
  unparsable case is classed by the statement its suspend sits under:

  | | Expensify | Discord |
  |---|---|---|
  | lifted | 312 | 847 |
  | unparsable case, suspend under an `if` | 140 | 677 |
  | unparsable case, no `return` at its top level (suspend deeper, under a `try` or a label) | 234 | 361 |
  | unparsable case, returns at its top level and fails otherwise | 45 | 511 |
  | two terminal cases with content | 69 | 146 |
  | no terminal case | 3 | 10 |
  | `emit_yield_chain` | 1 | 0 |

  Every machine reports as `generator`: the bundles' async code is mostly Babel's
  `asyncToGenerator` over a `function*`, a generator machine, and native `async` functions
  (Expensify prints 18) are marked async only after this point. Telling those apart is G4's.
- Ratchet: raw machines per bundle, counted by the executing-generator guard each keeps
  (`Generator functions may not be called on executing generators`), ceilings Equinox 0,
  Expensify 536, Discord 1,738 in `scripts/build/output_baseline.tsv`.

### G1 — The machine read off the CFG, log only (medium)

On the step function's CFG: the dispatch block and its label slot, each case's entry by
label, each suspend site (the label it stores and the result it returns), each resume entry
(the block after the protocol), and the protocol branches. Nothing is rewritten. Counted
against `state_machine_v98`: on every machine it lifts, both readings find the same cases and
suspends.

Exit: agreement on every machine the current lifter lifts, on all three bundles; each machine
G1 cannot read is classified.

**Done (2026-11-01), the exit met only in part.** `transforms/generator/machine_cfg.rs` reads
the CFG after F4, where status and label are slots, the action and resume value are parameters
0 and 1, and constants are propagated. No output change. `--log genlift=debug` logs one line:

| reading against the lifter (statements at `generate_ir`) | Expensify | Discord |
|---|---|---|
| both read, same suspends | 229 | 780 |
| both read, suspends differ | 15 | 58 |
| lifter lifts, CFG refuses: a `try` in the source | 43 | 270 |
| lifter lifts, CFG refuses: body reads status or label | 2 | 1 |
| CFG reads, lifter refuses (G2's machines) | 209 | 593 |
| neither: a `try` in the source (G3) | 147 | 749 |
| neither: no label test, so no suspend | 127 | 75 |
| neither: protocol has code in front | 34 | 35 |
| neither: other | 1 | 3 |
| no status guard, so no machine (a top-level `K === x` test fooled the old check) | 431 | 472 |

Agreement is not total, and neither gap is resolved. Where suspends differ the lifter always
has one yield and the CFG 2 to 6: it may be dropping yields, or several suspends may share a
resume. Read them in G5. A source `try` lowers to a catch that stores a label and rejoins the
dispatch, a label with no resume protocol, plus a try-state slot; the lifter lifts 43 and 270
such machines by skipping that label, which G3 and G5 must check.

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

**Done (2026-11-01).** `machine_cfg::rewrite` runs in `generate_ir` on a machine the CFG reads
and the statement lifter refuses, then structure recovery runs again on the result. Each resume
entry has its own register for the resume value; a block read from two entries stays raw.

| | Expensify | Discord |
|---|---|---|
| raw machines | 536 -> 401 | 1,738 -> 1,247 |
| undeclared | 12,677 -> 12,713 | 6,507 -> 5,824 |
| I1 | 2,096 -> 2,047 | 6,866 -> 6,641 |
| I2 | 948 -> 943 | 1,728 -> 1,723 |

v96 is byte-identical. Unparsable and consts written do not rise. `gen_if`, `gen_for`,
`gen_while_break` and v98 `async_loop` pass and left the known failures; `gen_two_returns`
already passed. Ratchets lowered to 401 and 1,247.

Expensify's undeclared rise is the counter, not new bad reads. `undeclared.mjs` ignores a name
declared at the top of any module, and an unrelated module's top-level `tmp17`, `tmp22`, `tmp23`,
`tmp25`, `tmp28` and `tmp30` hid 227 reads of those names on lines this stage did not change
(React's `while (null !== tmp22)`, a wrong name in its own right). Those declarations moved. The
rest of the difference is -191.

Three things this stage changed or found:
- The two lifters are compared at different times: the statement lifter's answer in
  `generate_ir` is not its answer at W16b. Machines it refused early but lifted late now go
  through the rewrite (W16b's lifted count on Expensify 312 -> 249, Discord 847 -> 759), so this
  stage does change some existing lifts' output. They read better, but G5 should compare them.
- Slots of functions folded into a wrapper's body take one name too (`separate_step_env_names`):
  a lifted body split them (`items = self`, `closure_4.items`).
- `yield` now reaches operand positions, so `!(yield x)` and `"a" + (yield x)` print parentheses.
- Refused by the rewrite on Expensify: 11 where a register from the dispatch reaches the body,
  1 with no suspend. Three of 151 sampled new Discord `function*` bodies still show `arg0 === 1`
  with no nested raw machine: `function*(arg0, value)` shapes the statement lifter let through,
  not this rewrite, which drops the protocol. G5 reads those.

What stays raw is mostly G3's: a `try` in the source (G1's table). `raw_async` now lifts, so it
no longer covers a raw machine; `gen_try_catch` and `gen_try_finally` do.

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

### G0b — v96 generators found wrong by G0's snippets (not this plan's lifter)

§2 leaves v96 out: it has real generator opcodes and nothing to lift. But G0's snippets, which
also run on v96, show three v96 defects that drop or change code, found 2026-10-30 and listed
as v96 known failures:
- `gen_if`: after `if (x) { yield 1 } else { yield 2 } yield 3`, the else branch's resume is
  printed `return yield 2`, so the trailing `yield 3` is gone from that branch.
- `gen_try_finally`: the `finally` is printed as catch-and-rethrow (its body copied after the
  `try` and into the `catch`), so `.return()` on a suspended generator skips it.
- `gen_delegate`: `yield*` is not recombined; the lowered delegation loop prints with
  `HermesBuiltin.ensureObject`, `getMethod` and `generatorSetDelegated`.

Equinox is v96, and its patch sites are found by reading decompiled code. **Until these are
fixed, read a decompiled v96 generator or `async` function against its disassembly before
trusting a branch's contents.** They want their own stage in structure recovery for v96
generators (the resume after a yield inside a branch, `finally` on the generator return path,
`yield*`); each snippet leaves the known-failure list when its stage lands.

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
