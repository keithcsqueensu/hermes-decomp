# Impl plan — B, putting hermesc's inlined helpers back

Status: **proposed (2026-10-28), not started.** Depends on IDENTITY_PLAN.md (Plan A) through
stage A3 step 5 (shared bodies by identity); starts after it.

## 1. Where things stand

`hermesc -O` inlines small functions into their callers. When the inlined function creates a
closure, every caller now creates the *same* compiled closure on an environment of its own:
one compiled function, many creators, each with its own live scope. S3.3 and S3.4 made the
decompiler print that correctly, by printing the closure's body once per creator with that
creator's names (`shared_body_variants`, `ClosureContext::shared_creations`). Correct, but it
is the compiler's inlining printed back as source, and it has three costs:

- **Size.** Discord's `dispatchOrResetSocket` callback has 187 creators and is printed 187
  times; ts-pattern's `chainable` methods are printed in each of 32 matchers (module 5745);
  Expensify's `console` wrappers once per level in F0. Shared functions with more than one
  creator: Expensify 980, Discord 1,324 (S3.3 TooMany census, after the cap was raised).
- **Readability.** The reader sees 187 near-identical blocks where the source had one helper
  and 187 calls.
- **What copies cannot express.** A shared function that creates itself (ts-pattern's
  `optional`, whose inlined `chainable` creates it again) is created on a new `pattern` each
  time. Copies would recurse without end and one declaration reads one creator's scope, so the
  decompiler declares it once per creator and its reads stay unknown (fixture
  `inlined_recursive`, KNOWN; `closure_1_0` undeclared). Only a function taking `pattern` as a
  parameter expresses it, which is what the source had.

**Often the original survives.** hermesc keeps an inlined function when it has other uses.
Discord's `dispatchOrResetSocket` is F65764, itself one of the callback's 187 creators; the
other 186 are its inlined copies. Where the original exists, putting the helper back is
replacing each inlined copy with a call to it.

## 2. Goal and non-goals

Goal: where creators of one compiled closure hold copies of one inlined function, print that
function once and a call to it at each site, with the per-site values as arguments; and give
a self-creating shared function a factory form.

Not in this plan: undoing inlining that created no closure (a plain helper inlined into its
callers leaves no shared compiled function to anchor on; out of reach without one); undoing
other optimizations hermesc applied after inlining.

## 3. The shape to recognise

An inlined copy, in a creator, is the slice of the creator's code that:

1. builds the copy's environment (`CreateFunctionEnvironment` past the creator's own, or a
   block environment: the creation scope in `shared_creations`),
2. stores the inlined function's parameters and locals into its slots (the per-site values
   are what is stored from outside the slice),
3. creates the shared closure(s) on it and leaves them as the slice's result (returned,
   stored into an object literal, passed on).

The same slice in every creator, up to its inputs, is the inlined body. After Plan A it can be
compared by identity: same operations on corresponding identities, inputs named by where they
come from.

## 4. Stages

Measured like Plan A's stages: env fixtures, the output ratchets, the module oracle (§3 of
Plan A), undeclared and placeholder counts, strings and properties per module (here a module
*should* shrink; each shrink read, as a removed copy, never a removed effect), gates, run time.

### B0 — Census (small)

Per shared function: creators, scopes, whether an uninlined original exists (a creator whose
own body is the slice, or a function whose body matches it), output size attributable to the
copies. Exit: the numbers per bundle in this file, and the candidates ranked by size.

### B1 — Recognise the slice (medium)

Find each creator's slice (§3) and compare slices across creators. A slice that differs only
in its inputs is a copy; one that differs otherwise (hermesc folded a constant argument into
it, dropped a branch the constant made dead, or inlined only part) is not, and is counted by
why. Exit: the share of creators whose slice matches, per bundle, with the mismatches
classified.

### B2 — Call the surviving original (medium)

Where an uninlined original exists and the slices match it, replace each copy with
`original(args)`, the arguments being the slice's inputs in the original's parameter order.
The original's name is its own. Exit: Discord's `dispatchOrResetSocket` printed once and
called at 186 sites; the env fixtures for shared bodies still pass; output smaller, nothing
else lost.

### B3 — Synthesize the factory (medium to large)

Where no original survives, print the slice once as a function: parameters are the inputs that
differ between creators (an input the same everywhere stays inside), the body is the slice,
the result is what it returns. Name it from the closure it creates or the property it is
stored under, as the naming passes would. The self-creating case is this with the factory
calling itself: `optional(pattern)` creating `chainable({ ... })` whose method calls
`optional`. Exit: `inlined_recursive` passes; ts-pattern's matchers each call one `chainable`.

### B4 — Placement and order (medium)

The synthesized function is declared where every creator sees it (the nearest common scope,
usually the module), never above a binding it reads; a call replaces the slice at the slice's
position so evaluation order holds; a slice whose inputs are evaluated in an order a call
would change is left as copies and counted. Exit: no new undeclared or TDZ reads (module
oracle), the fixtures pass.

## 5. Order and size

Plan A to A3 step 5 → B0 → B1 → B2 → B3 → B4. B2 is the cheap win where originals survive
and can land before B3. B3 and B4 are the larger part.

## 6. Risks

- **Not a copy after all.** hermesc optimizes the inlined body in its new context (constant
  arguments folded, branches dropped). A slice that differs is left as copies; matching up to
  inputs only, never up to "similar".
- **Effects and order.** A call evaluates its arguments before the body; the inlined slice may
  interleave them. B4 checks order and leaves what it cannot prove.
- **`this`, `arguments`, `new.target`.** Inlining binds these to the caller's; a synthesized
  function must take them as parameters or the slice is left alone.
- **Hoisting and TDZ.** A function declaration placed above a `let` it reads, or a call moved
  across one, changes what runs; the module oracle is the check.
- **Debugging the output.** A reader comparing with stack traces or the bytecode sees calls
  where the bytecode has none; say so in a comment at the synthesized function.
