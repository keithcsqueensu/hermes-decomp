// versions: 98 99
// S3.3: hermesc inlines a small local factory into two callers, so one
// compiled inner function is created by both, each on its own environment.
// The inner body is printed inside each caller and must read that caller's
// variables: `left`'s closures count from `base`, `right`'s from `k * 10`.
(function () {
  function wrap(tag, n) {
    return function (x) { n += x; return tag + ":" + n; };
  }
  globalThis.left = function (base) { return wrap("L" + base, base); };
  globalThis.right = function (k) { return wrap("R" + k, k * 10); };
})();
var a = left(5), b = right(1), c = right(2);
print([a(1), a(2), b(3), c(4), b(5)].join(" "));
