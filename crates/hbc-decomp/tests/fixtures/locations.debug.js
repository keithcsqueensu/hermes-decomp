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
