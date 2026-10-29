// versions: 98 99
// S3.4: a closure created inside a loop body (on the loop's block
// environment, which holds `i`) writes a variable of the enclosing function.
// The declaration pass counts a nested function's writes only through the
// function tree, and a closure on a block has the block as its parent: the
// owner declared the counter `const` and the closure's write threw.
(function () {
  var hits = 0;
  var fns = [];
  for (let i = 0; i < 3; i++) {
    fns.push(function () { hits = hits + i; return i; });
  }
  fns.forEach(function (fn) { fn(); });
  print("hits " + hits);
})();
