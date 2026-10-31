// hermesc inlines a call to a closure it knows, and the inlined body reads the
// closure's own captures through `GetClosureEnvironment`. `make` is inlined
// twice, so one compiled closure sits in two slots over two different blocks.
function main() {
  function make(k) { return function (n) { return n * k; }; }
  var twice = make(2), thrice = make(3);
  function walk(fn, x) { return x > 0 ? fn(twice(x)) + walk(fn, (thrice(x) - 7) / 4) : 0; }
  print(walk(function (a) { return a + 1; }, 9));
}
main();
