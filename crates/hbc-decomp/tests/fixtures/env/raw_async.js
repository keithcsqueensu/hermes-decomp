// versions: 98 99
// An async method whose machine the lift cannot read (an await in a loop):
// printed raw inside `spawnAsync`, its loop counter and sum must survive from
// one step to the next, and `this` must reach it.
var out = [];
class Q {
  constructor() { this.start_ = 2; this.items = [1, 2, 3]; }
  async drain(limit) {
    var sum = 0;
    for (var i = this.start_; i < limit; i++) {
      sum += await globalThis.Promise.resolve(this.items[i % 3] * i);
    }
    return sum;
  }
}
new Q().drain(5).then(function (v) { out.push(v); print(out.join(",")); });
