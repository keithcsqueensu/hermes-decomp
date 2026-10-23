// A generator whose closures are created in later states and read variables
// first assigned in earlier ones, alongside the function around it.
function make(prefix) {
  var count = 0;
  function* gen(a) {
    var first = a + 1;
    yield first;
    var second = first * 2;
    var f1 = function () { return prefix + ":" + first + ":" + second; };
    yield f1();
    var third = second + count;
    count += 100;
    var f2 = () => prefix + ":" + third + ":" + count + ":" + a;
    yield f2();
    yield [f1(), f2()].join("|");
  }
  return gen;
}
var results = [];
for (var v of make("p")(5)) results.push(v);
for (var w of make("q")(1)) results.push(w);
print(results.join(" "));
