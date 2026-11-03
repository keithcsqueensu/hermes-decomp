# Later fixes: staged plan

Status: planned 2026-11-02, nothing started. This covers what is left after the loop header fixes and
`for_of_nested_for` (see `scripts/build/roundtrip_known_failures.tsv`). Measurements are from the build at 6f3705d.

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
