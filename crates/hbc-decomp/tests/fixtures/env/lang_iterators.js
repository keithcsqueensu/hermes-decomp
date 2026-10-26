// A custom iterable, for-of with an early break (the iterator is closed),
// spreading a generator, and the return and throw of a generator.
var out = [];
var closed = 0;
var iterable = {
  [Symbol.iterator]() {
    var i = 0;
    return {
      next() { i++; return { value: i, done: i > 5 }; },
      return() { closed++; return { done: true }; },
    };
  },
};
for (var v of iterable) { if (v === 3) break; out.push(v); }
function* gen() { try { yield 1; yield 2; yield 3; } finally { out.push("fin"); } }
out.push([...gen()].join("+"), Array.from(gen(), (x) => x * 2).join("+"));
var g = gen();
g.next();
out.push(JSON.stringify(g.return(42)), closed);
var h = gen();
h.next();
try { h.throw(new Error("boom")); } catch (e) { out.push(e.message); }
print(out.join(","));
