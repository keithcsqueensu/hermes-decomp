function s(k, n) { var o = "s"; switch (k) { case "A": o += "a"; break; case "0": if (n) break; default: return "bad " + k; } return o + "e"; }
function c(xs) { var o = []; for (var i = 0; i < xs.length; i++) { var x = xs[i]; if (x.a) { if (x.b) continue; if (x.c) throw new Error("c" + i); } else if (x.d) { throw new Error("c" + i); } o.push(i); } return o.join(); }
function r(x) { try { return c(x); } catch (e) { return e.message; } }
print(s("A"), s("0", 1), s("0", 0), s("Z"));
print(r([{ a: 1, b: 1 }, { a: 1 }, {}]));
print(r([{ a: 1, c: 1 }]));
print(r([{}, { d: 1 }]));
