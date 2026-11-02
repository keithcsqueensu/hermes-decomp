function e(n) { var o = []; var i = 0; var r; for (;;) { r = n[i]; o.push(r); if (r === 0) break; i++; } return o.join() + r; }
function c(n) { var s = 0; var i = 0; do { s += n[i]; i++; if (s > 5) break; s += 1; } while (i < n.length); return s; }
print(e([4, 0]));
print(c([1, 2]), c([9, 2]));
