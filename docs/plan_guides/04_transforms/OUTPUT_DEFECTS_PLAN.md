# Impl plan — output defects outside the environment model

> **Ownership.** *Owns* the output defects that V98_ENV_PLAN's E1 fixtures and E0
> measurement found but that are not environment-model bugs. These are the `other:` rows of
> `tests/env_model.rs` KNOWN, the modules that still fail to parse, and the JSX residue.
> *Delegates* environment and slot identity to
> `../03_analysis/closure_model/V98_ENV_PLAN.md` and `K4_PLAN.md`.

Status: **D0, D1, D2 and D3 landed (2026-10-24).** Every root cause below comes
from an investigation run on 2026-10-23 against main at 330e329 (outputs `k4e3`). Each was
reproduced on a fixture or a bundle output. File:line references are as of that commit; check
them again when implementing. Results are in §5 (D0, D1), §6 (D2) and §7 (D3).

---

## 1. What is wrong, by size of the problem

**Unparsed modules** are re-parsed with acorn-jsx, one module at a time, and grouped by cause:

| # | Cause | Eq | CRx | Exp | Dis |
|---|---|---|---|---|---|
| U1 | `yield` left in an `async` body (v96) | 37 | 70 | – | – |
| U2a | `class X` declared over a name that is already bound | 1 | 1 | 2 | 2 |
| U2b | A parameter named like a class the body declares | 1 | – | 1 | – |
| U3 | An exported function collides with a module-level alias | 4 | 1 | 2 | 2 |
| U4 | A catch parameter redeclared by `let` | – | – | 1 | – |
| U5 | Several `export default` in the 4294967295 pseudo-module | – | – | 1 | – |
| U6 | Orphan chunk parsed as one scope (measurement artifact) | 1 | 1 | 1 | – |
| U7 | `arguments` used as a parameter name | 6 | – | – | – |
| U8 | `Symbol_…` identifiers keep `/` and `@` | – | 1 | 5 | – |
| U9 | `new` callee not wrapped in parentheses | – | – | 4 | – |
| U10 | `continue labelN` printed with no `labelN:` | – | 1 | 1 | – |
| U11 | JSX attribute string printed with escape sequences | – | 2 | – | – |
| U12 | Depth-limit placeholder printed as an empty expression | – | – | – | 1 |
| | **Total** | **50** | **77** | **18** | **5** |

**Defects that parse but mean something else.** These are wider than the parse failures,
because most of them never break syntax:

| # | Defect | Measured frequency |
|---|---|---|
| B1 | Builtin tables misindexed: v99 is off by 2, and by 3 past `setFunctionName`. v98's public indices are off by 2 | every v99 builtin. v98 bundles: 0 misnamed so far (see B1) |
| B2 | A default parameter past the header arity is dropped, so the body reads `argN` | Exp 1,019, Dis 944 (v98); 0 on v96, where Babel lowers defaults |
| B3 | A rest parameter's `.slice(N)` loses N (`strip_hermes_this` eats it) | Exp 93, Dis 63 `[...arguments].slice()` |
| B4 | A template literal or JSX keeps a stale register name (`${tmp}`) | Eq 278 `${tmpN}`, 182 of them wrong; Dis 1,612 lines |
| B5 | A call callee, member object or `new` callee is under-parenthesized | Eq about 92 lines: `await f(x)()`, `a ? f : g(x)` |
| B6 | `document.createElement("tag")` printed as JSX `<tag />` | Eq 36, CRx 70, Exp 73, Dis 154 |
| B7 | `X = function(){}` turned into `class X` because X is capitalized | in U2a. Also a meaning bug: a function component becomes a class that needs `new` |
| B8 | v98 private class members (brand, `#x`, constructor) not recovered | Dis 2 classes. Fixture class_scopes |
| B9 | v96 generator wrapper collapse misfires on a factory that returns a generator | fixture generator_states.v96; 0 in bundles |

**E1 KNOWN rows each item clears:** async_closures.v96 (B4), async_closures.v99 and
deep_nesting.v99 (B1), deep_nesting.v96 (B5), generator_states.v96 (B9), param_scopes.v96 and
.v98 (B2 + B3), param_scopes.v99 (B1 + B2 + B3), async_forms (D2.1), class_scopes (B8).
Re-word what stays: after B1, deep_nesting.v99 fails like its v98 twin (`tmp2`, env).

---

## 2. Phases

Each item lists its root cause, fix, size, risk and measurement. Order is by payoff over
risk. Within a phase the items are independent, one commit each.

### D0 — measurement first (S)

- **Split orphans.** `undeclared.mjs`/`unparsed.cjs` parse the whole orphan section as one
  scope (U6). Split it per function.
- **New counters** in the compare output, so every item below has a number before and after:
  - bare lowercase JSX elements (B6);
  - `${tmpN}` (B4);
  - unbound `argN` and the `let x = argN; if (argN === undefined)` shape (B2);
  - `[...arguments].slice()` (B3);
  - `HermesBuiltin.` and `builtin\d+` (B1);
  - `goto B` (B9);
  - `Cannot initialize private field twice` (B8).
  - The investigation's scripts are a starting point: scratchpad `inv_class/argn2.py`,
    `dupclass2.py` and `inv_jsx/tags.cjs`, `reparse.cjs`.
- **A builtin-table test.** Generate each `resources/builtins/BuiltinsNN.json` from
  `C:/src/hermes-vNN/include/hermes/FrontEndDefs/Builtins.def`, using the enum expansion in
  `Builtins.h`. NORMAL, BUILTIN, PRIVATE and JS each count once; `MARK_FIRST_*` are aliases.
  Assert the checked-in table matches the generated one, plus the fixture-known indices: v99
  44 = `throwTypeError`, 47 = `copyRestArgs`, 58 = `spawnAsync`.

### D1 — small fixes with the biggest counts (each S, low risk)

1. **B1, builtin tables.**
   - `Builtins99.json` leaves out `Symbol` and `eval` at 0–1 and `setFunctionName` at 55.
     The result is `/* builtin58 */` for `spawnAsync`, `HermesBuiltin.apply` for
     `copyRestArgs`, and `Object.assign()` for `throwTypeError`.
   - Regenerate it, with the two normal builtins named `Symbol` and `eval`:
     `builtin_name_to_expr` splits on the dot, so `globalThis.Symbol` would print
     `globalThis.globalThis.Symbol`.
   - **Open question for v98:** `Builtins98.json` has `_reservedNormal37/38` where the v98
     tree has the normal builtins at 0–1, which shifts public 0–38 by 2. Discord and Expensify
     show no misnamed builtins, so either their compiler matched the table or they emit no
     static public builtins. Check a v98 bundle's `CallBuiltin` indices against both orders
     before changing v98.
   - Clears async_closures.v99. deep_nesting.v99 and param_scopes.v99 move to their v98
     reasons.
2. **U1, async bodies keep `yield`** (107 modules, the largest single cause).
   - v96 chain: wrapper → `CreateGenerator` stub → state machine.
   - W14 converts only the stub. The helper is an unnamed import (`_mod47.default`), so
     `looks_like_async_helper` misses it.
   - W15 `unwrap_async_wrappers` (`async_detection.rs:326-337`) then copies the
     unconverted innermost body into the async wrapper.
   - Fix: call `convert_yields_to_awaits` and `mark_async` on the copied body. Checked: all
     107 modules parse once `yield` becomes `await`.
3. **B2, default parameters dropped.**
   - `params_from_names` (`pipeline/mod.rs:290-293`) stops at
     `names.len()`/the first unused position. The header arity stops at the first default.
   - Fix: count = max(arity, 1 + highest `Parameter(i)` the body reads), padding with
     `argN`.
   - Apply the same body-aware count on the paths with no names: `context/rendering.rs:540`,
     `context/codegen.rs:185`, `pipeline/mod.rs:104`, `rendering/missing.rs:88`,
     `context/mod.rs:179`.
4. **B3, rest index lost.**
   - `spread_rest.rs:43-47` builds `.slice(N)` with no receiver argument.
   - `strip_this` (`inline/strip_this.rs:189-216`) then strips N as if it were `this`.
   - Fix: exempt `[...arguments].slice`, as it already exempts one-argument `Object.create`.
   - B2 + B3 together make param_scopes v96/v98 pass (checked by hand: `3,7,8`).
5. **B4, stale names in templates and JSX.**
   - `rename_expr` (`var_naming/renaming.rs:158-252`) has no `TemplateLiteral` or `JSXElement`
     arm, so F24 renames a definition but not its use inside `${}`.
   - Fix: rebuild `rename_expr` on `MutVisitor`, which already walks every variant, so a new
     variant cannot be missed again.
   - Checked on Equinox: `${tmpN}` drops from 278 to 96, and dead-binding cleanup stops
     deleting the renamed definitions. Clears async_closures.v96.
6. **B5 and U9, parentheses.**
   - One `needs_parens_as_lhs` helper for call callees (`expr_gen.rs:257-262`, which wraps
     only functions today), member objects (the `compound` list, about line 187, which leaves
     out `Await`/`Yield`) and `new` callees (about line 366, unguarded).
   - Clears deep_nesting.v96 and the 4 Expensify U9 modules.
7. **B6, DOM `createElement` lowered to JSX.**
   - `jsx_factory_name`/`is_jsx_call` (`jsx.rs:46-67`) accept any receiver, and a string
     tag passes unchecked.
   - Fix: require at least 2 arguments for classic `createElement` (Babel always passes
     props, `null` at least), and reject `document`, `_document*` and `ownerDocument`
     receivers.
   - This also takes the 3+3 lowercase residuals off E0's count: they are
     `<style />`/`<canvas />` from `document.createElement`.
8. **U11, JSX attribute strings.** `expr_gen.rs:506-508` prints with `{:?}`, but JSX attribute
   strings have no escapes. Fix: emit `key={"…"}` through the JS string printer whenever the
   value holds `"`, `\`, a newline or a control character.
9. **Codegen one-liners:**
   - U4: drop `let P;` at the head of a catch body when P is the catch parameter
     (`codegen/control_flow.rs:160-175`).
   - U7: filter `arguments`/`eval` out of IPA parameter names (`naming.rs:169`).
   - U8: sanitize the `Symbol_{other}` branch (`util.rs:63-69`).
   - U12: print the depth placeholder as `void 0 /* … */` in expression position
     (`ir/depth.rs:50`, `ir/expr/display.rs:237`).
   - U5: generalize the duplicate default-export guard (`esm_gen.rs:395-402`). Separately,
     find out why worklet factories land in module 4294967295.
10. **B8.2, `AddOwnPrivateBySym` operands swapped** (`opcodes_prop.rs:349-352`). The `.def`
    comment is wrong; the interpreter takes O2 as the value and O3 as the symbol
    (`Interpreter.cpp:3233`).

**Exit:**
- Unparsed modules: Equinox 50 → about 6, CoachRx 77 → about 5, Expensify 18 → about 8,
  Discord 5 → about 4. What remains is D2.
- KNOWN loses 6 to 8 rows.
- v96 output changes, so this is the first change since E0 that is not byte-identical on
  v96: classify each `worse:` list by hand, as for K4 stages.

### D2 — medium fixes that also change meaning (M, medium risk)

1. **`let Box` beside `class Box`** (async_forms).
   - A class kept in an environment slot has its initial `undefined` store turned into
     `let Box` (`inline/declarations/mod.rs:408-436`).
   - `declared_by_decl` is computed at `:378` but used only for the block hoist.
   - Fix: when the name is in `declared_by_decl`, drop an `= undefined` assignment and never
     make any other assignment a `let`.
   - Then async_forms.v98 fails on an environment cause (`closure_1_0`), which E4 owns.
2. **B7/U2a, classes without evidence.**
   - `class_patterns/analyzer/mod.rs:316-330` promotes any capitalized `X = function(){}`.
   - Fix: require prototype methods, a `super` call or an instance-field candidate. When
     the statement was an assignment, emit `X = class X {…}`.
3. **U2b, a parameter named like a body class.** Drop IPA parameter names that the body
   declares (`naming.rs:169`, after `run_ipa`).
4. **U3, an exported function colliding with a local alias.**
   - Two real bindings, so text demotion would be wrong.
   - Fix: rename the alias in the IR. Seed `used` in `module_hoist/names.rs:14-82` with
     function declaration and export names.
5. **U10, labels as structure.**
   - Labels exist only as `Comment("labelN:")` markers (`codegen/mod.rs:365-395`), and a
     loop rewrite loses them. The likely place is the iterator→for-of lift; not pinned down.
   - Fix: a label field on loops, or a `Labeled` statement.
6. **B9, generator wrapper collapse.**
   - (a) `generator_wrapper_target` (`generator_wrapper.rs:85-86`) matches a factory that
     returns a generator function, and chained replacements remove the real body
     (`transforms_phase.rs:338-380`). Drop a replacement whose inner is itself a wrapper.
   - (b) `render_missing_from_bytecode` (`rendering/missing.rs:59-62`) uses
     `DecompileOptionsV2::default()`, which turns off structuring. Use the pipeline's options.
   - Clears generator_states.v96.

### D3 — v98 private class members (L)

class_scopes v98/v99, and the 2 Discord classes with private fields. These pieces are
independent:
- **Private names in the IR.** `handle_create_private_name` (`opcodes_prop.rs:292-311`)
  makes every private name a variable, so the brand `CreatePrivateName(K)` reads `K` while
  `K` is in its TDZ. Give private names their own IR value: a brand prints as a `#K() {}`
  member and its check as `#K in obj`.
- **The constructor.** Turn `Object.create(new.target.prototype)` back into `this`, and
  remove the double-init guard and the brand and field installs.
- **Rendering.**
  - The constructor's own slot 0 prints under the factory's slot name (suspected:
    `walk.rs:317`, slot naming inside `Statement::Class` methods).
  - An arrow inside a class prints `/* body not rendered */`.
  - v98 `peek` loses `this`.

Do D3 only after D1 and D2; it touches the IR type.

---

## 3. Environment items carried from V98_ENV_PLAN §6

- **A second `CreateFunctionEnvironment`.** E2 already models it as built on the closure
  environment, which is correct (v98 `BytecodeList.def`: "using the current function's
  enclosing environment as the parent"). E4 found what produces it: Hermes inlines an
  object-literal factory, and each copy gets a sibling scope of the inlining function.
  Expensify's F72596 builds 32 of them and creates the same 5 closures in each. A function
  created in more than 16 scopes is `TooMany` to the scope graph, so its levels stay
  unresolved. That case belongs to E4.
- **`GetClosureEnvironment`** (295 on Discord) still needs a fixture before it is modelled.

## 4. Side findings

- `--show-offsets` turns off the v96 generator CFG transform: `generator_cfg.rs:41` checks
  `statements.last()` for `__yield_point__`, but the offset comment comes after it. Debug
  output then differs from real output.
- v98 prints many `HermesBuiltin.arraySpread`/`apply` calls without folding them (Discord
  1,728/334, Expensify 1,314/462). That is a folding pass to write, not a bug in an
  existing one.
- `DeclareGlobalVar` prints `let out;` at script top level.

## 5. D0 and D1 results

**[measured]** Base: main at a16c09f (E4). New: D1 complete. `scripts/measure/undeclared.mjs
--per-module`, which since D0 parses the orphan section one function at a time and prints the
defect counters on a second line.

| | Equinox | CoachRx | Expensify | Discord |
|---|---|---|---|---|
| unparsed modules | 49 → 8 | 76 → 4 | 17 → 7 | 5 → 4 |
| unparsed lines | 49,053 → 5,386 | 28,775 → 1,375 | 169,051 → 67,182 | 95,869 → 729 |
| undeclared, modules parsed in both | — | — | 70,311 → 52,742 | 106,283 → 94,107 |
| placeholders, modules parsed in both | — | — | 5,112 → 5,112 | 17,992 → 17,992 |
| `argN` undeclared (B2) | 1,164 → 1,165 | 512 → 511 | 2,926 → 740 | 2,717 → 246 |
| `[...arguments].slice()` (B3) | 0 | 0 | 93 → 0 | 63 → 0 |
| `${tmpN}` (B4) | 211 → 58 | 64 → 27 | 1,373 → 457 | 1,547 → 633 |
| intrinsic JSX tags (B6) | 51 → 31 | 78 → 58 | 102 → 63 | 165 → 14 |

- The only names that got worse on the shared modules are `document` (+8, +33: B6 makes
  `document.createElement` a real reference) and a few `objN`.
- The remaining intrinsic tags are `createElement("svg", props)` and the like: real elements.
- Equinox's `argN` count is not B2: v96 lowers defaults in Babel. Not yet looked at.
- E1 KNOWN lost 6 rows: async_closures.v96 and .v99, deep_nesting.v96, param_scopes.v96,
  .v98 and .v99. deep_nesting.v99 now fails like its v98 twin.
- B1 was wider than planned: v98's table was wrong too (`GetBuiltinClosure 1` is `eval`, which
  printed as `Date.UTC`), and v97 had no table. upstream_pin now re-derives every table from
  its checkout.
- U1's fix is not where §2 put it. The yields reach the async function through the generator
  wrapper collapse, so the conversion runs once over every async body after the transforms.

**Left from D1:**
- U7: `consoleHandler(arguments, …)` (Equinox, 2). The name does not come from IPA, which no
  longer proposes it.
- U8: `Symbol_x[Symbol.x@@]` (CoachRx, 1). A second shape, not the `@@x` name.
- U5: the duplicate `export default` in 4294967295 is printed from the body, not from the
  export list the guard filters (Expensify, 1).
- U10 (2) and the `already declared` rows (6, 2, 5, 4) are D2.

## 6. D2 results

**[measured]** Base: D1 complete (§5). New: D2 complete, plus the D1 leftovers.

| | Equinox | CoachRx | Expensify | Discord |
|---|---|---|---|---|
| unparsed modules | 8 → 0 | 4 → 0 | 7 → 0 | 4 → 0 |
| undeclared | 12,989 → 12,667 | 13,280 → 13,207 | 60,521 → 62,091 | 94,179 → 69,130 |
| placeholders | 1,666 → 1,414 | 669 → 579 | 8,317 → 9,066 | 18,006 → 3,799 |
| `class` declarations removed / added | 289 / 7 | 405 / 1 | 866 / 8 | 19,282 / 3 |

- Expensify's totals rise only because 67,000 more lines parse (the 4294967295 pseudo-module
  among them). On the modules that parsed before, undeclared falls 60,521 → 59,810.
- **D2.2 (B7) is the big one.** A capitalised function became a class on its name alone. On
  Discord that turned the React compiler's memoised callbacks and every `useStateFromStores`
  selector into `class S { constructor() {…} }`, which throws when called. They are arrows
  again, and the closures inside them resolve, which is most of Discord's placeholder drop.
  The removed classes sampled were all function components, factories or callbacks. The added
  ones are real classes that had been nested inside a wrongly promoted function.
- Names that got worse are small against that (Discord 1,241 references, Expensify 915, the
  bulk in newly parsed code). Expensify's `length` (+256) is a pre-existing bug, laid out
  differently: lodash's `keysIn` loses `const length = obj.length` in D1's output too.
- D2.1 changed where async_forms fails: it is now an environment cause (`closure_1$2`).
- D2.4 renames the export, not the local, which §2 proposed: the local is what the module's own
  code calls, so the export takes `X_export` and is published as `export { X_export as X }`.
- D2.5 needed no label field: the for-of lift was dropping the `labelN:` marker, which codegen
  already attaches.
- U5 was not a duplicate-export bug. The pseudo-module 4294967295 printed every unclaimed
  factory under one header. Each now gets its own; why they go unclaimed is still open.
- E1 KNOWN is down to 11 rows: block_scopes ×3, class_scopes ×2 (D3), async_forms ×2,
  deep_nesting.v98/.v99 and generator_states.v98/.v99. All but class_scopes are environment causes.

## 7. D3 results

**[measured]** Base: D2 complete (§6). New: D3 complete, with the fixes it turned up on the way.

| | Equinox | CoachRx | Expensify | Discord |
|---|---|---|---|---|
| undeclared | 12,667 → 10,130 | 13,207 → 11,841 | 62,091 → 53,845 | 69,130 → 59,407 |
| placeholders | 1,414 → 46 | 579 → 105 | 9,066 → 8,926 | 3,799 → 3,506 |
| constructors left building their own receiver | – | – | – | 189 + 224 → 4 |

- **The largest change is not a class fix.** `inline_expressions` cleared the definitions an
  enclosing statement had activated after the first statement of a nested list, so in
  `if (c) {…} else { use(r) }` the else branch read a register whose definition had already
  been consumed: a dangling `tmp`/`tmp2`. That is what made v98 `peek` print `tmp.#secret`,
  and it is most of the drop on every bundle.
- `collect_function_refs` used the plain visitor, which does not enter class methods, so a
  class's parent was never re-rendered once its methods' bodies landed: an arrow made in a
  constructor stayed `/* body not rendered */`. With that and the inline fix, the constructor's
  slot 0 no longer prints under the factory's name; there was no separate slot-naming bug.
- Private names needed no IR value of their own: they already flow through slot naming as
  `#name`. The brand (a `CreatePrivateName` without `#`) is named `#K` and printed as a field
  `#K;`, not the `#K() {}` method §2 proposed: Hermes installs it on each instance like a field,
  and the `#K in this` check behaves the same. `#f in obj` and `obj.#f = v` print as private
  syntax.
- `restore_constructor_this` gives a base constructor `this` back
  (`obj = Object.create(new.target.prototype)` → `obj = this`, the double-init guard, the
  `= undefined` installs and the trailing `return obj` go). A derived class's
  compiler-written constructor, `r = HermesBuiltin.applyArguments(this, new.target)`, read
  `this` before `super()` and threw (224 sites on Discord); it is `super(...arguments); const
  r = this;`. The 4 left on Discord build the receiver inside a branch and have no private
  fields; they run correctly as printed.
- **Not done: an explicit `super(…)`.** It lowers to a construct of the parent with
  `new.target` and prints as `new Own(arg0, 1, new.target, this)` (57 sites on Discord), and
  hermesc inlines such a constructor at its `new` site, which prints a private store outside
  the class. Fixture class_super records it.
- **Module 4294967295 was 1,447 real modules.** react-native-worklets registers each worklet
  factory as a Metro module keyed by its hash (`__d(F11977, 4739766416, [])`). The id was cast
  to u32, which saturates, so every registration overwrote the last (all named `PathEffects`,
  the last one) and batch.rs grouped the factories as one module. Ids are read as exact u64; one
  past u32 is filed under a synthetic id and printed as registered. `--modules`,
  `--from-module`, `deps`, `modules`, `extract`, `decompile --json`, `frida-hooks`, the TUI and
  the MCP tools take and print the registered id. U5's per-function headers went with it.
- **The inline fix exposed four naming heuristics**, which the slot-identity gate caught
  (Equinox I1/I2/I3 rose above their ceilings):
  - The closure-context walk analysed class method bodies with the enclosing function's
    names and registers, so a method's `tmp15 = closure_37` stood for the enclosing body's
    `tmp15`; moment.js's factory then named its locales table after another slot and
    `loadLocale` read a `closure_37` nothing assigned. Each method now sees a copy of the
    enclosing names and fresh registers.
  - A reserved word made a usable object key: React's `child.return = nextEffect` named the
    shared slot `return`, printed `_return`, which landed on a local `_return` and the body
    declared its own. Reserved words no longer name variables.
  - Inheriting an ancestor's name for a capture gave up when a plain local of the reader held
    the name, leaving a placeholder nothing declares (Sentry's `setState(closure_10)`). The
    local is renamed instead, when the owner really binds the slot under that name.
  - A body did not declare a local that any nested function also named, stamped or not, so
    superagent's `Request` constructor wrote the module's `root` slot as `self`. Declarations
    now yield only to names nested functions use stamped with a slot above the body, or read
    without assigning.
- Slot-identity ceilings are set to the new counts (`slot_identity_baseline.tsv` says why):
  Equinox I1 2,263, I2 2,117, I3 10; CoachRx 2,781, 1,390, 9. I3 falls by 11 on Equinox and
  25 on CoachRx because a capture nothing declared is now declared where it is read (that
  rises I2 by as much); the new I1 rows are a parent keeping an unused `let closure_0 =
  param` alias while its child reads the parameter.
- Against D2, Equinox's undeclared names fall by 2,654 and rise by 117 (`arg0` +35, the most).
- E1 KNOWN: class_scopes ×2 off; class_derived (new) passes; class_super ×2 (new) on. 11 rows:
  9 environment causes, class_super ×2.
