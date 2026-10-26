// Nested patterns with defaults, rest elements and properties, computed keys,
// a swap, and patterns in parameters and in for-of heads.
var out = [];
var { a, b: { c = 3, ...restB } = {}, ...rest } = { a: 1, b: { d: 4, e: 5 }, f: 6 };
var [p, , q = 9, ...more] = [7, 8, undefined, 10, 11];
var k = "zz";
var { [k]: viaKey = "none" } = { zz: "hit" };
var m = 1, n = 2;
[m, n] = [n, m];
function f({ x = 1, y: [y1, y2 = "y2"] = [] } = {}, ...args) {
  return [x, y1, y2, args.length].join("/");
}
for (var [i, { v }] of [[0, { v: "v0" }], [1, { v: "v1" }]]) out.push(i + v);
out.push(a, c, JSON.stringify(restB), JSON.stringify(rest), p, q, more.join("+"), viaKey, m, n, f(), f({ y: [5] }, 1, 2));
print(out.join(","));
