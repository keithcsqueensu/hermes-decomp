// versions: 98 99
// S3.3: hermesc inlines a small local factory at each call, so each inlined
// copy builds its own block environment for the factory's parameter and
// creates the one compiled inner function on it. The copies are one source
// scope but live at once: every closure keeps its own `tag`.
(function () {
  function wrap(tag) {
    return function (x) { return tag + ":" + x; };
  }
  var table = { a: wrap("A"), b: wrap("B"), c: wrap("C") };
  print([table.a(1), table.b(2), table.c(3)].join(" "));
})();
