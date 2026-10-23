// Parameter scopes: defaults that read earlier parameters and create closures,
// which Hermes may give an environment of their own.
var seen = [];
function withDefaults(a, b = a + 1, get = () => a + b) {
  var a2 = a * 10;
  seen.push(function () { return a2 + get(); });
  return get();
}
function rest(first, ...more) {
  return () => first + more.length + arguments.length;
}
seen.push(() => withDefaults(1));
seen.push(() => withDefaults(2, 5));
seen.push(rest(3, 4, 5));
print(seen.map((f) => f()).join(","));
