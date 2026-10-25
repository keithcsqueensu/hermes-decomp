// versions: 98 99
// Generator state machines the lift cannot read (a yield in a loop, a yield in
// each arm of an if): printed raw, their status, label and loop counter must
// survive from one step to the next.
var out = [];
function* counter(n) {
  for (var i = 0; i < n; i++) {
    yield i * 10;
  }
  return "end";
}
function* branchy(x) {
  if (x > 1) {
    yield "big";
  } else {
    yield "small";
  }
  yield "after";
}
var g = counter(3);
var r;
while (!(r = g.next()).done) out.push(r.value);
out.push(r.value);
var b = branchy(2);
var b1 = b.next().value;
var b2 = b.next().value;
var b3 = b.next().done;
out.push(b1, b2, b3);
print(out.join(","));
