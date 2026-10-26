// versions: 98 99
// S3: a machine parks its parent's scope and its own in its wrapper's slots
// on the first step, and later steps reach `this`, a parameter and the loop
// variable through them. The await and the yield sit in a loop, so the lift
// leaves both machines raw, and each later state creates a closure reading
// the parked scopes.
var out = [];
class Box {
  constructor(base) { this.base = base; }
  async collect(n) {
    var fns = [];
    for (var i = 0; i < n; i++) {
      var got = await globalThis.Promise.resolve(i * 10);
      fns.push(() => this.base + ":" + got + ":" + i + ":" + n);
    }
    return fns.map((f) => f()).join(",");
  }
}
function* ticks(tag, n) {
  for (var i = 0; i < n; i++) {
    var seen = yield i;
    out.push(((k) => tag + k + ":" + seen + ":" + i)(i));
  }
}
var g = ticks("t", 3);
g.next();
g.next("a");
g.next("b");
g.next("c");
new Box("b").collect(3).then(function (s) { out.push(s); print(out.join(" | ")); });
