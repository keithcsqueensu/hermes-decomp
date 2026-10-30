# Impl plan — shared functions that create themselves

Status: **proposed (2026-10-30).** Split out of IDENTITY_PLAN.md A2c, whose exit names the case
(Expensify F71548). A2c1 built every block scope by its creation site; this is the half A2c
cannot close by itself.

## 1. The case

jQuery's `inspectPrefiltersOrTransports` (Expensify F71548):

```js
function inspectPrefiltersOrTransports(structure, options, originalOptions, jqXHR) {
  var inspected = {}, seekingTransport = structure === transports;
  function inspect(dataType) {
    var selected;
    inspected[dataType] = true;
    jQuery.each(structure[dataType] || [], function (_, prefilterOrFactory) {
      var dataTypeOrTransport = prefilterOrFactory(options, originalOptions, jqXHR);
      if (typeof dataTypeOrTransport === "string" && !seekingTransport && !inspected[dataTypeOrTransport]) {
        options.dataTypes.unshift(dataTypeOrTransport);
        inspect(dataTypeOrTransport);
        return false;
      } else if (seekingTransport) {
        return !(selected = dataTypeOrTransport);
      }
    });
    return selected;
  }
  return inspect(options.dataTypes[0]) || !inspected["*"] && inspect("*");
}
```

`hermesc -O` inlines `inspect` twice into F71548, so F71548 builds two block environments,
each holding its own `selected`, and creates the `each` callback (F107458) in both. The
callback calls `inspect` again, which the compiler also inlined into the callback, so F107458
creates itself as well. Its store `selected = dataTypeOrTransport` writes level 1: the block
of whichever creation it runs under.

What the output gets wrong today:

- F107458 has one body and one parent. The scope graph gives it three creation scopes (two
  blocks of F71548, its own block); `plan_env_levels` sees a creator at or below itself
  (`creates_itself`) and installs **no** parent, so its slot reads take the level model's
  walk, and its store `selected = ...` prints as `closure_0$2 = tmp2`.
- Shared-body copies (S3.3, `shared_bodies.rs`) render one copy per creation scope, renamed
  onto that scope's slots. They skip a function that creates itself: the copy would have
  to create a copy of itself, and the rename table is per (creator, scope).
- Since A2c1 both blocks of F71548 exist, so the second copy's read of `selected` is a slot
  of a real scope, but nothing writes it under that name: it still prints as the undeclared
  `closure_130_0`.

Scale: Expensify has 29 shared functions that create themselves (`shared: functions that
create themselves` in the scope-graph census); Discord and v96 are to be counted.

## 2. Stages

- **SC0 — census.** Per bundle: self-creating shared functions, split by (a) created on
  several scopes of one creator (copies, like F71548), (b) on scopes of several creators,
  (c) only on its own scope beyond one creator (plain recursion through an inlined helper).
  Count the undeclared names and `closure_L_S` placeholders each one's family prints. A
  function in (c) with one outside creation scope needs no copy at all: the outside scope is
  its parent and the inner creation reads the same chain.
- **SC1 — the parent for (c).** Install the single outside creation scope as the parent
  where every inner creation reaches the same scope one level up. Exit: (c) reads by scope,
  not by the level walk; undeclared not rising on any bundle.
- **SC2 — copies that recurse.** A copy of F107458 for block B creates, inside itself, the
  copy for its own block (the inlined `inspect` builds a fresh block per call, so the inner
  creation is the function's own scope, not B). The rename table becomes per creation site
  (A2c1's key): the outer creation sites in F71548 map to the two copies, the inner site in
  the callback maps to the copy itself. Exit: F71548 prints two callbacks, each writing its
  own block's `selected`, with no `closure_130_0`; a round-trip snippet of the jQuery shape
  added to the corpus and passing on v96 and v98.
- **SC3 — (b).** The same with several creators, once SC2's per-site table exists.

## 3. Not in scope

The level cap (`MAX_LEVEL`, many blocks of one function sharing builder level 255) is
A2c's: addressed by creation site there, not here.
