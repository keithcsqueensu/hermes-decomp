# Impl plan — lifting v98 generator machines

> **Ownership.** *Owns* turning HBC >= 97 generator and async state machines back into
> `function*` / `async function` bodies when the yields sit under control flow. *Delegates*
> the naming and declaration of a machine's variables while it stays raw to
> `../03_analysis/closure_model/IDENTITY_PLAN.md` A2e, and the catch-loop dominator change
> (A2e3, A2e4) is ordered against this plan's G2 below.

Status: planned 2026-10-29. **G0 landed (2026-10-30).** **G1 landed (2026-11-01).** **G2 landed (2026-11-01).** **G3a, G3c and G3d landed (2026-11-01), with harder short-circuit shapes still wrong;** G3b is open in part.

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

What this stage changed or found:
- The two lifters are compared at different times: the statement lifter's answer in
  `generate_ir` is not its answer at W16b. Machines it refused early but lifted late now go
  through the rewrite (W16b's lifted count on Expensify 312 -> 249, Discord 847 -> 759), so this
  stage does change some existing lifts' output. They read better, but G5 should compare them.
- Slots of functions folded into a wrapper's body take one name too (`separate_step_env_names`):
  a lifted body split them (`items = self`, `closure_4.items`).
- `yield` now reaches operand positions, so `!(yield x)` and `"a" + (yield x)` print parentheses.
- Refused by the rewrite on Expensify: 11 where a register from the dispatch reaches the body,
  1 with no suspend. A scan of the 151 `function*` lines new on Discord flagged 6 with `arg0 ===
  1` or a result object nearby. 3 hold a nested raw machine, and 1 is a lifted function inside an
  outer raw machine with a `try`, whose protocol the scan ran on into. The other 2 are
  `function*(arg0, value)`, a shape already in the old output (200 -> 197 bundle-wide), not traced
  to their lifter. G5 reads those.

What stays raw is mostly G3's: a `try` in the source (G1's table). `raw_async` now lifts, so it
no longer covers a raw machine; `gen_try_catch` and `gen_try_finally` do.

### G3 — try/catch/finally inside a machine (medium)

The machine's own handler regions (the catch labels the current lifter skips) become `try`
statements around the resumed regions they protect. A `finally` that runs on `.return` is
kept as a `finally`.

Exit: the `try` snippets pass; the census row for catch-carrying cases is 0.

Split on 2026-11-01. `finally` needs a different read, and a bug outside generators turned up.

**G3a, `try`/`catch`. Done (2026-11-01).** `machine_cfg` reads the shared catch block's test chain
on the try-state slot, splits blocks at its stores, and pushes one handler per state, outermost
first, marked `exact_range` so that structure recovery ends the body at the region's one exit.

| | Expensify before | after | Discord before | after |
|---|---|---|---|---|
| raw machines | 401 | 326 | 1247 | 678 |
| undeclared | 12713 | 12635 | 5824 | 5254 |
| unparsable modules | 0 | 0 | 0 | 0 |

v96 is byte-identical. `gen_try_catch` and `async_catch` round-trip. The strings lost are not code:
`"attempt:"` and `"viewed_user:"` now print inside templates, and the rest are unused module-path
arguments to `require`.

- The old lifter now refuses a catch label and any lift with machine code left in it. Skipping the
  label lost the handler: a small test printed `2 eundefined 4`. A machine with a `try` takes the
  CFG lift or stays raw.
- `cleanup_noise` no longer drops a bare `return;` that code follows. A tail copied by
  `reemit_exit` relies on it: Expensify's onboarding handler called its navigation twice without
  it. That restored 125 returns in Expensify's lifted code and 178 in Discord's.

Still raw, by CFG-lift refusal (Expensify, Discord): "several ways out" 24, 62; "a register from
the dispatch" 13, 37; "no suspend" 6, 10. The bigger group by reading is G3b's.

**G3b, `finally`, and the refusals above. Open.** A `finally` is lowered as catch-and-rethrow with
copies of its body on each exit, including the `.return` side of the protocol, so the read fails
with "protocol: unexpected branch" (18, 170). `gen_try_finally` is the test. The dispatch register
is untraced.

**"Several ways out". Done (2026-11-01).** A `return` inside a `try` left the region as a second exit,
because the block before it holds the state store's `r = 3` and only an empty block counted as quiet.
A chain of plain register assignments ending in the return of a computed value cannot throw, so it now
prints inside. A call in `return f(x)` runs before the exit and is still caught. `gen_return_in_try`
is the test. Expensify raw 332 to 307, Discord 678 to 618; undeclared 13159 to 13134, 5588 to 5528; no
string lost; equinox unchanged. The refusal stays for exits that can throw. Diagram: [`several_ways_out.html`](diagrams/several_ways_out.html).

**G3c, the same bug in ordinary code. Done (2026-11-01).** This stage is the one exception to the
rule that v96 output does not change: the old output was wrong. `cleanup_noise` dropped a trailing
`return undefined` from any nested list, code after it or not. It now drops one only where the end
of the list is the end of the function (the last statement of the body, through `if`, `try` and
blocks, never a loop or `case`). `return_nested` is the test.

| | Equinox v96 | Expensify | Discord |
|---|---|---|---|
| lines changed | 3093 | 8276 | 4577 |
| strings restored / lost | 11 / 0 | 3 / 0 | 7 / 0 |
| undeclared | 1485 -> 1488 | 12635 -> 12636 | 5254 -> 5256 |
| raw machines | 0 | 326 | 678 |

Without the `return`, the code after the arm ran. An arm that emptied took its `if` with it, and a
loop body that emptied lost its calls: `while (readResult !== null) {}` had dropped
`closure_8.push(readResult)`. The undeclared rise is restored reads of `window` and `__DEV__`,
which are real globals. Hunks were sampled by kind, not read one by one.

**G3d, wrong output found by G3c. Partly done (2026-11-01).** Two bugs, both older than G3c. `loop_two_exits` and `a && b || c` are fixed, whether the arms return or assign (`branch_and_or_value`). **Open:** `(a && b || c) && d`, the same test in a loop when its copy is refused, and two such tests in a row (`branch_and_or_assign`). Those want short-circuit conditions recovered as such, where this only copies a tail. Diagram: [`and_or_copy.html`](../03_analysis/diagrams/and_or_copy.html).

`loop_two_exits`: a `for` with a `return` inside prints as `while (i !== 1)`, losing `i < n` and the repeat.

A branch whose targets were both emitted prints nothing. In
`a && b || c`, the arm holding `b` jumps to two blocks already emitted, and structure recovery
returns empty for both, so the test is lost. `_iterableToArray` prints `Array.from` where the
bytecode tests `@@iterator` first. G3c did not cause it: the old output lost the same test.
`branch_and_or` is the test, a known failure on both versions. Both are wrong-output bugs outside
generators, so they go before G3b.

The fix: a tail reached again that ends in a branch is copied as an `if` tree, up to 16 blocks,
stopping at the innermost merge. Each copy gets registers of its own, except one it reads before
assigning, because later passes read a register assigned once as one value. A copy whose values
are read past it share that register if blocks already assign it several times, since it is then one variable. A register assigned in one block is read by later passes as one value, so that copy stays empty.

Sharing registers, measured against the generator batch: no string lost, raw unchanged, output grows 0.3%, 0.4%, 0.4%. Undeclared 1500 to 1527, 13132 to 13138, 5489 to 5531: a copy reading a register assigned in a sibling arm, where the old output had dropped the test. Silent became loud, and it is open.
 A loop whose header has statements and whose body has one
other exit keeps the `for` test by breaking to that exit. A test against `undefined` is left alone,
because that is the for-of and for-in form its recogniser reads.

Measured against G3c (equinox, Expensify, Discord): no string lost and 0 unparsable on all three.
Output grows 1.5%, 3.6%, 2.6%. Raw machines stay 0 and 678. Expensify goes 326 to 332, all in module
1039, whose tail held 3 raw machines and now prints on each of its 3 paths where it printed on one.
No machine newly refused; the ceiling is raised for that reason.

Undeclared names rise: 1488 to 1506, 12636 to 13159, 5256 to 5588. **Open, split off:** most of
the rise is code that used to be dropped and now prints, which then hits older defects. The largest
is a bare import statement beside a broken tag (`Pressables;` then `<tmpResult.PressableOpacity>`),
present 86 times in Discord before and 343 after. It was not traced to its pass. The old output
returned nothing on those paths and the new one throws on the tag, so both are wrong, but the new
one is loud. Copies made by `reemit_exit` keep their registers: renaming them lost 36 strings on
Discord and did not help `tmpResult`.

**G3b, `finally` on v98. Done in part (2026-11-01).** A `.return` side that leaves the try state, runs
straight-line code and returns its value now reads. `fold_finally` turns the catch and rethrow plus the
copy after it into a real `finally`, and every side must have run the same code, compared with registers
renumbered, or the machine stays raw. `gen_try_finally` and `gen_finally_cases` pass. **Still raw:** a
`try` with both `catch` and `finally` (`gen_try_catch_finally`), a `finally` with a branch or a `yield`
of its own, and a `return` through a `finally` with a call in it (several ways out). **Still wrong on
v96:** that lowering is read by a different path and prints the catch and rethrow, so `.return` skips
the `finally`.

`finally` and `gen_if` together: Expensify raw 307 to 305, Discord 618 to 579; undeclared 1506 to 1500,
13134 to 13132, 5528 to 5489; no string lost. "protocol: unexpected branch" falls from 18, 170 to 8, 14.

**`gen_if`, v96. Done (2026-11-01).** An exception to the rule that v96 output does not change: this is
wrong output. `ResumeGenerator`'s return flag tested with `JmpFalse` jumps to continue, and the path
always kept the false side, so `return yield 2` printed and the code after it was lost.

**Loop header statements. Done in part (2026-11-01).** An exception like G3c: wrong output outside
generators. The back edges of a loop come to its header, so `stmts; while (cond)` ran the statements
once: `for (;;) { push(x); if (done) break; x = next(); }` pushed one item. They now stay inside
`while (true)`. Against the assign form: Undeclared 1527 to 1519, 13138 to 13107, 5531 to 5530; for-of
counts unchanged; no string lost; raw unchanged. `loop_body_then_break` and `loop_header_statements`
pass. **Open:** a header's value read after the loop stays hoisted (`loop_header_value_after`), and a
header branching to itself with more of the loop after it (`loop_header_self_branch`).

**A `try` in a loop. Done (2026-11-02).** Wrong output outside generators: any `for` holding a `try`
ran its body once. A catch was a root of the dominator tree, so the block it rejoins was dominated by
nothing above the `try`, the back edge went unseen, and the loop was lost. Dominators now come from
the protected range. Four more fixes were needed once the loop was seen. A header that starts a `try`
gets an empty block in front, so the loop opens first. The fence after a `try` goes past empty blocks,
and with several exits it takes the one the catch reaches first. A merge past the fence is no merge
inside the `try`. A copied tail reaching the innermost loop's header or exit continues or breaks.

`continue` no longer counts as empty, in `is_effectively_empty` and `is_effectively_empty_body`: an
`else { continue; }` was left out and fell into the code after it. The loop's exit now prefers the
back edge's way out.

`try_in_loop`, `try_continue_in_loop` and `try_in_loop_cases` pass on v96 and v98. Against the previous
step: undeclared 1519 to 1517, 13107 to 13077, 5530 to 5530; Expensify raw 305 to 299, Discord 579;
0 unparsable. Output changes widely (equinox shrinks 1.8%), because code after a `try` no longer prints
inside it and its catch: Discord's module 964 had most of its body in a `try`. Strings lost, each read:
a dead enum (`Stopped`), `require` placeholders from those tails, a duplicate import reference. One
real: `_NullFlags`, recorded as `switch_case_break_if`, wrong before and after in different ways.

**A read moved past a store. Done (2026-11-02).** Wrong output outside generators: `s = log.join(); log =
[]; print(s)` printed the store first, because a store to a property counted as no effect.
`FreshObjects` makes one a barrier in `inline_expressions` and `optimize_chain_access`, unless its object
is a literal nothing else has seen. `stmt_has_side_effects` itself is unchanged: object slot fills and the
JSX fold rely on it. `inline_past_global_write` and `inline_store_order` pass.

Against the previous step: undeclared 1517 to 1516, 13077 to 13036, 5530 to 5520; raw unchanged; closing
JSX tags 13624 to 13623 in Expensify. Discord gains 1552 `.call(tmp` forms where a method read no longer
crosses a store: right, and less readable. A barrier on every store instead took undeclared to 1330, 12532,
5312 and Discord raw to 552, but lost most JSX in Expensify (13624 to 6922). That gap is unexplained and
may hold more wrong output worth reading.

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
