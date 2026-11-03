# Later fixes: staged plan

Status: planned 2026-11-02. Item 1 and L1 done, L2 closed, 2026-11-03. This covers what is left after the loop header fixes and
`for_of_nested_for` (see `scripts/build/roundtrip_known_failures.tsv`). Measurements are from the build at 6f3705d.

**Overall order (agreed 2026-11-02),** across this plan, GENERATOR_LIFT_PLAN.md and
`../03_analysis/closure_model/IDENTITY_PLAN.md`:

1. The loop items: `loop_header_self_branch`, `for_of_nested_for`, `for_of_empty_body`.
2. L1, right after them. It throws at runtime and has the same loop-header shape.
3. L2.
4. Close A2: re-read A2e3b and A2e4 against the current build (measure, no code). Done 2026-11-03, both closed.
5. A3.0 and A3.0b step 2 (done 2026-11-03), A3 steps 1-3.
6. L5, before A3 step 4: every machine it lifts is one that 4a-4d need not carry.
7. L3 and L4 (one gate run), beside A3 step 4: the same generator code.
8. A3 steps 4a-5c, then A4.

**Item 1 done (2026-11-03).** `loop_header_self_branch` (e81184b), then one batch for the two for-of snippets:

- `for_of_nested_for`: the for-of plumbing scan stopped at the inner loop's constant step and limit, and at a
  parameter source copy. It now keeps any constant and a parameter copy. Each early exit left `iter.return()` on an
  iterator the output no longer names; those closes are stripped at any depth.
- `for_of_empty_body` on v96 was the `n++` value read after its write, from four places, each verified with stage
  dumps: the F12 object fold moved a store above a register set in between; `inline_named` reused a multi-use
  definition after a write to what it reads (now written out where it stood, lazily); v98 F3 propagated a copy of a
  closure slot past a store to it; F7 inlined across a slot write. A side review found the same with properties
  (`t = a.b; a.b = 5; use(t)`, and a call in between): three of four snippets were wrong before. `inline_named` now
  treats a property read as invalidated by a statement that may write memory.
  Snippets: `postinc_read_after.js`, `property_read_after.js`.
- `apply_pending_to_stmt` took a statement's used names after substitution, so a substituted value stayed pending and
  was written again (`obj.value = f(x); const fResult = f(x);`), which broke the generator lifter's case parsing.
- The F3 call rule drops copies only of bindings the function writes; dropping all of them lost module-loader
  imports (Equinox raw `tmpN(NNN)` loader calls 27 -> 279 in one run).
- The slot check caught a dropped `if`: `cleanup_noise` took `num = <outer num>` (an unstamped local given a stamped
  enclosing variable) for `x = x`. Only `slot = local` is that store now.
- Also passing now: `loop_reassign` (v96, v98, v99) and `lang_iterators` v98 in `env_model`.

Measured against 6f3705d: raw machines Expensify 303 -> 274, Discord 579 -> 452. Undeclared Expensify 11155 ->
10313, Discord 5275 -> 5040, Equinox 821 -> 889. The Equinox rise is the counter, not new reads: it masks a name by
any file-level declaration, and the for-of rebuild removed some of those. Per module, Equinox fell 3162 -> 2732
(earlier build of this batch). Slot identity on Equinox 754/319/11 -> 745/315/11. Output 0.7-1.5% smaller; no string
lost.

Open: a nested function that writes a captured variable during a call is not seen by the F3 call rule, so
`t = x; f(); use(t)` can still read the new `x`. It was wrong before this batch too.

Wrong output goes before identity work. The generator stages are low urgency and sit where they share code
with A3 step 4.

Each stage ends the usual way: its snippet passes the round trip on v96 and v98, its known-failure rows are removed,
ratchets hold, and the whole output is measured. Control-flow stages get their own gate run. Generator stages may share
one. Causes are marked **verified** (read in the code or seen by running) or **guess**.

| Order | Stage | Why here | Gate run |
|---|---|---|---|
| 1 | L1 for-in with header statements | Throws at runtime; hundreds of sites | Own |
| 2 | L2 `tmp20`, `tmpN.e.r` | Wrong or undeclared reads in ordinary code | Own, after L1 |
| 3 | L3 v96 `finally` | Wrong only on `.return()` | Shared with L4 |
| 4 | L4 v96 `yield*` | Rare in these bundles | Shared with L3 |
| 5 | L5 v98 machines | Machine stays raw, which is verbose and not wrong | Own |

Depends on the loop header fixes: L1, because it is the same loop shape. Nothing here depends on L3, L4 or L5.

## L1. A for-in whose loop header has statements

```js
function b(o) { var f = true; for (var k in o) { if (typeof o[k] === "function") continue; f = false; } return f; }
```

prints, on v98:

```js
const keys = Object.keys();
if (keys !== undefined) {
  flag = flag2;
  while (keys[tmp] !== undefined) {
    if (typeof arg0[tmp5] === "function") { continue; } else { flag2 = false; continue; }
```

`Object.keys()` with no argument throws a TypeError, and `tmp` and `tmp5` are undeclared. Printed `Object.keys()` lines:
Equinox 95, Expensify 206, Discord 217. Not all need be this shape, so L1a counts them. The same snippet with no variable
written in the body prints a correct `for...in`. The oldest build in the scratchpad prints the same `Object.keys()`, so
this is not a recent regression.

**Cause.**
- **Verified:** `try_match_for_in` (`transforms/patterns/loops/for_in.rs`) needs `keys = ...`, then an `if` whose `else`
  holds only the first `cur = keys[idx]` and the `while`. A variable written in the body adds moves around
  `GetNextPName` (the disassembly has `Mov r3, r2; Mov r4, r3` before it), so the shape differs and the match fails.
- **Read, not run:** the lost argument. `handle_get_pname_list` (`ir/builder/opcodes_obj.rs`) builds
  `Object.keys(obj)` with no dummy `this`, and `strip_hermes_this` (`transforms/inline/strip_this.rs`) then removes the
  first argument of a call that survives to it. Only an unmatched for-in lives long enough for that.

**Stages.**
- **L1a.** Add snippet `for_in_header_value`: `b` above, a body that writes two variables, a `break`, and a nested for-in.
  Count which of the printed `Object.keys()` lines have this shape.
- **L1b.** Make the matcher accept header statements. They become the start of the for-in body, or stay before the
  statement it replaces if they run only on entry. Re-read the shape after the loop header fixes land first; they move
  these statements.
- **L1c.** Whatever still fails to match must not print as a real call. A lowered enumeration is not `Object.keys`, whose
  order and inherited keys differ. Use a named marker in the IR, so the output is visibly unrecovered and the count of
  `Object.keys()` goes to 0.

**Test.** The new snippet. Add a ratchet column for unrecovered enumerations.

**Done (2026-11-03).** Re-read after the loop header fixes: a loop tested against `undefined` (for-in, for-of) keeps
its header statements once before the `while` (`tests_undefined` in `loops.rs`), so the matcher saw extra copies such
as `flag2 = flag` between `keys = ...` and the `if`. **Verified** with an IR dump. The nested case also had
`then = [continue]` and a trailing `continue` after the inner `while`.

- L1b: `try_match_for_in` collects plain binding stores before the first fetch, rejecting any that touch the key or
  cursor registers. With a header, the object must be a plain value and the output is
  `if (obj != null) { header } for (k in obj) { ... }`: the header runs on entry when the object is not null or undefined, the
  only case where `GetPNameList` yields undefined. Inside the body it runs again before every `continue` of this loop,
  and at the end of a body that falls through. An exit branch equal to the code after the loop, or a trailing `else`
  equal to it, is accepted.
- L1c: an enumeration still unmatched prints as `__forInKeys(obj)`, with `strip_hermes_this` told not to take its
  argument. New ratchet column `unrecovered for-in` (`output_ratchets.sh`).
- Snippet `for_in_header_value` (a body that writes two variables, a `break`, a nested for-in) passes on v96 and v98.

Measured against 1f7a1d2: `Object.keys()` lines 95/206/217 -> 0 (Equinox/Expensify/Discord); `for...in` printed
1721 -> 1810, 2758 -> 2890, 799 -> 973; unrecovered `__forInKeys(` 6, 74, 40. Undeclared per module on Discord
11523 -> 10980 (the whole-file count rose 5040 -> 5074, the masking effect noted under item 1; the only names that
rose are `__forInKeys` itself and two `str` names by 4 each). Equinox 889 -> 843. Three strings flagged lost were
checked: two are the for-in variable now named (`${keys}` -> `${path}`), one a template now printed as a
concatenation.

Open, Equinox's 6: four are the "is empty" idiom, which structure recovery folds into
`keys === undefined || keys[x] === undefined` before the matcher runs; one has a `try` in the body; one had its first
fetch inlined into the `while` test. Expensify's and Discord's have not been classified.

## L2. Expensify's undeclared reads

**`tmp20` in `dispatchReducerAction`.** React's inlined `markRootEntangled`:

```js
var e = root.entangledLanes |= lanes;
for (var t = root.entanglements; e;) { var i = 31 - clz(e), l = 1 << i; if (l & lanes | t[i] & lanes) t[i] |= lanes; e &= ~l; }
```

The combined `lanes` value is inlined at its two uses before the loop, and the reads inside the loop print `tmp20`, never
declared. **Verified** from the output. The copy also re-reads `pending.lanes` after it has been assigned, which is wrong
in general. **Guess:** `inline_expressions` (`transforms/inline/mod.rs`) or `inline_single_use`
(`transforms/cleanup/advanced.rs`) removes the definition without counting reads in a following `while`. Reduce to a
snippet first, `inline_value_read_in_loop`, and bisect by pass.

**`tmpN.e.r`.** The two samples read (`tmp13`, `tmp229`, SheetJS ranges) are declared and fine at those lines, as a
`const` inside an `if`. **Unknown cause:** the undeclared reads may be elsewhere, or in an outer scope past that block.
Start by listing undeclared `tmpN` with `e` and `r` reads from `scripts/measure`. If they are reads past the block, this
is a hoisting bug and gets its own snippet. If none remain, close the item.

**Test.** Snippets as above. The undeclared count on Expensify should fall; it is a reading aid, not a gate.

**Closed (2026-11-03), no code change.** Both re-read on the L1 build (eeefc99):
- `tmp20`: the loop now reads `tmp30`, the combined lanes computed once before `pending.lanes` is written; every read
  in the loop is declared. Fixed by item 1's value-ordering batch (1f7a1d2). The reduced `markRootEntangled`
  snippet passes the round trip, so no snippet was added.
- `tmpN.e.r`: Expensify prints 315 `tmpN.[es].[rc]` reads. For each of the 28 names involved, every undeclared
  reference (`undeclared.mjs --per-module --show`) was matched against its line: none is a range read. The undeclared
  `tmpN` that remain are other reads of the same common names, not this shape.

## L3. v96 generator `finally`

```js
function* g() { try { yield 1; yield 2; } finally { log.push("fin"); } }
```

prints as `try { yield 1; yield 2; log.push("fin"); } catch (e) { log.push("fin"); throw e; }`, so `.return()` skips the
`finally`.

**Cause. Verified:** v96 has real generator opcodes. After each `ResumeGenerator`, a jump on the return flag leads to a
copy of the `finally` (`L62`, `L86` in the disassembly). `transform_generator_cfg` (`ir/builder/generator_cfg.rs`) keeps
only the `not_returned` side, so the copies vanish. v98 already solves the same shape with `fold_finally`
(`transforms/generator/machine_cfg.rs`), but v96 never reaches it.

**Reach. Guess:** an `async` function resumes only by next and throw, so its catch-and-rethrow already behaves correctly.
Then only explicit `.return()` and early exit from a `for...of` over a generator are wrong. L3a should count this.

**Stages.**
- **L3a.** Keep each dropped side's statements in `transform_generator_cfg`, ignoring a bare `CompleteGenerator; Ret`.
  Count, with no output change, how many v96 generators and async functions in Equinox have a non-trivial side.
- **L3b.** Pass those copies to `fold_finally` after structure recovery. `gen_try_finally` and `gen_finally_cases` are the
  tests. v96 has no raw machine to fall back on, so a side that does not match prints as today and is counted in a trace.
- **L3c.** `try`, `catch` and `finally` together: `gen_try_catch_finally`, printed today as a catch-and-rethrow around a
  `try`. Depends on L3b. Split off if the nesting does not fold directly.

## L4. v96 `yield*`

`var r = yield* inner();` prints as the lowered loop, with `HermesBuiltin.ensureObject`, `getMethod` and
`generatorSetDelegated`. `yield iter2` then yields the result object. **Verified** from `gen_delegate`.

`Expression::Yield` already has `delegate`, and `generator_cfg.rs` always sets it to false. **Plan:** recognise the shape
as a pattern in `transforms/patterns/`: the `generatorSetDelegated` call identifies it, then match the whole loop,
including the `throw` and `return` arms, and print `yield*`. Refuse and keep today's form if any arm differs.

**Reach.** `generatorSetDelegated` is printed on 4 lines of Equinox and 0 of Expensify and Discord. This is low value, and
the match is exact, so it is small.

## L5. v98 machines

- **`gen_delegate` (v98).** The machine stays raw and `yield*` is lowered inside it. Reuse L4's match on the CFG lift's
  region. Do this only if a real bundle needs it: none of the 882 raw machines prints `generatorSetDelegated`.
- **`gen_try_catch_finally` (v98).** `fold_finally`'s `.return` sides do not match when the `try` has both `catch` and
  `finally`. **Guess:** the state table holds a dispatch entry for the handler, read at `finally_side`. Start from the
  refusal in the lift trace. This also covers the "still raw" list in GENERATOR_LIFT_PLAN.md G3b only if the same refusal
  reads there, so count refusals before and after.

## No plan needed here

- **`capture_vs_local` (v96).** Already planned as A4d2 in `03_analysis/closure_model/IDENTITY_PLAN.md`, formerly K4.7. Do
  not fix the one name here. It needs a rename by identity, and that plan's A3 and A4 come first.
- **`nested_destr` (v59, v60, v62, v71).** Skip unless those versions are needed. This corpus has only v96 and v98, so these
  are the 4 rows `roundtrip.sh` reports as not checked. Testing them needs `fetch_hermesc.sh` and `build_corpus.sh` first,
  and neither has been run on this machine (no `.toolchains`). The likely place is `for_of/legacy.rs`, a guess.
