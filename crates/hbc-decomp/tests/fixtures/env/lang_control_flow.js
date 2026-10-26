// Labeled break and continue across nested loops, switch fall-through with a
// block-scoped case, and a finally that overrides a return.
var out = [];
outer: for (var i = 0; i < 4; i++) {
  for (var j = 0; j < 4; j++) {
    if (j === 2) continue outer;
    if (i === 3) break outer;
    out.push(i + "" + j);
  }
}
function sw(x) {
  var r = [];
  switch (x) {
    case 1: r.push("one");
    case 2: { let t = "two"; r.push(t); break; }
    case 3: r.push("three");
    default: r.push("dflt");
  }
  return r.join("+");
}
function fin() { try { return "try"; } finally { out.push("finally-ran"); } }
function over() { try { return "try"; } finally { return "finally"; } }
out.push(sw(1), sw(3), sw(9), fin(), over());
print(out.join(","));
