// Three levels: an async function inside a generator inside a function inside
// an async function, each level capturing its own variable and the ones above.
var out = [];
async function top(t) {
  var l1 = "L1" + t;
  function mid(m) {
    var l2 = l1 + "L2" + m;
    function* gen(g) {
      var l3 = l2 + "L3" + g;
      yield async function inner(i) {
        var l4 = l3 + "L4" + i;
        await null;
        return () => [l1, l2, l3, l4, t, m, g, i].join("~");
      };
    }
    return gen;
  }
  await null;
  var innerFn = mid("m").call(null, "g").next().value;
  var f = await innerFn("i");
  out.push(f());
  return out;
}
top("t").then(function (o) { print(o.join("")); });
