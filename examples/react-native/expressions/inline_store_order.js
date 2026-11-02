var g = { n: 1 };
function a(o) { var s = o.n; o.n = 2; return s + o.n; }
function b(o) { var s = o.n + 1; o.n = 5; print(s); return o.n; }
function c(o, k) { var s = o[k]; o[k] = "new"; return s; }
function d() { var s = g.n; g = { n: 9 }; return s + g.n; }
function e(o) { var f = o.fn; o.fn = null; return f(); }
function h(o) { var s = o.list.length; o.list.push(1); return s + "," + o.list.length; }
function i(o) { var s = o.n; delete o.n; return s + "," + o.n; }
print(a({ n: 1 }));
print(b({ n: 1 }));
var q = { x: "old" }; print(c(q, "x"), q.x);
print(d());
print(e({ fn: function () { return "ran"; } }));
print(h({ list: [] }));
print(i({ n: 3 }));
