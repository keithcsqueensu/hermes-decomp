function t(xs) { var o = []; for (var i = 0; i < xs.length; i++) { switch (xs[i].k) { case "A": o.push("a"); break; case "0": if (xs[i].n == "_Null") break; default: throw new Error("bad " + xs[i].k); } o.push("end"); } return o.join(); }
function r(x) { try { return t(x); } catch (e) { return e.message; } }
print(r([{ k: "A" }, { k: "0", n: "_Null" }, { k: "A" }]));
print(r([{ k: "0", n: "x" }]));
print(r([{ k: "Z" }]));
