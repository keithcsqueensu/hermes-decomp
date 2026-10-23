// versions: 98 99
// Async functions with nothing of their own to capture (the v98 wrapper has no
// environment), async arrows, and async methods, each reading outer scopes.
var out = [];
var base = 10;
async function plain(n) {
  await null;
  return function () { return base + n; };
}
var arrow = async (k) => {
  var local = k * 3;
  await null;
  return () => base + local + k;
};
var obj = {
  bonus: 7,
  async method(m) {
    var self = this;
    await null;
    return function () { return self.bonus + m + base; };
  },
};
class Box {
  constructor(v) { this.v = v; }
  async get(d) {
    var me = this;
    await null;
    return () => me.v + d;
  }
}
plain(1)
  .then(function (f) { out.push(f()); return arrow(2); })
  .then(function (f) { out.push(f()); return obj.method(3); })
  .then(function (f) { out.push(f()); return new Box(4).get(5); })
  .then(function (f) { out.push(f()); print(out.join(",")); });
