var seen = [];
var it = { [Symbol.iterator]() { var n = 0; return { next() { seen.push(n); return { done: n++ > 1 }; } }; } };
function m(o) { for (var v of o) { continue; } }
m(it);
print(seen.join());
