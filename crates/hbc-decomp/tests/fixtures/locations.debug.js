// Fixture for the R24 guard: this one is compiled with -g3, so every function
// carries FLAG_HAS_DEBUG_INFO and a source-location stream. The `.debug.js`
// suffix is what tells scripts/build_hermes_vm.ps1 to pass -g3; every other
// fixture is built without it.
//
// Several functions, each with statements on distinct lines, so a location
// stream has more than one entry to be wrong about after a resize.
function classify(n) {
  var label = 'zero';
  if (n > 0) {
    label = 'positive';
  } else if (n < 0) {
    label = 'negative';
  }
  return label;
}

function total(a, b) {
  var sum = a + b;
  var doubled = sum * 2;
  return doubled;
}

print(classify(1), classify(-1), classify(0), total(2, 3));

// A closure, so the scope table has something to name. Hermes records a name in
// the scope descriptor only for a *captured* variable; `label`/`sum` above live in
// registers and never appear, so without this the fixture could not demonstrate
// that debug-driven variable naming works at all (DI1).
function makeCounter(startValue) {
  var count = startValue;
  function bump(amount) {
    count = count + amount;
    return count;
  }
  return bump;
}

var counter = makeCounter(10);
print(counter(1), counter(2));

// Three captured variables, so name resolution is tested past the first one.
// Names inside a scope descriptor are byte *offsets* into the debug string table,
// not indices: only the first string sits at offset 0, so an index-based reader
// resolves `first` and returns empty for `second` and `third` while looking fine.
function threeCaptures(seed) {
  var first = seed + 1;
  var second = seed + 2;
  var third = seed + 3;
  function readsAll(k) {
    return first * k + second * k + third;
  }
  first = first + 1;
  second = second + 1;
  third = third + 1;
  return readsAll;
}

print(threeCaptures(1)(2));
