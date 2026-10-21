// A capture beside a local of the same name (closure model K4, I2): `a` is
// named `items` after its value, and so is the array `read` builds, which cut
// the capture: `a[1]` read the fresh local.
function outer(seed) {
  var a = [seed, seed + 1];
  var b = { n: seed * 2 };
  function inner(xs) {
    var first, setter;
    function grab() { first = xs[0]; setter = function (v) { first = v; }; }
    function read() { return [first, a[1], b.n]; }
    grab();
    var before = read();
    setter(first + 10);
    return [before, read(), first];
  }
  return inner;
}
var f = outer(3);
print(JSON.stringify(f([5, 6])));
print(JSON.stringify(f([7])));
