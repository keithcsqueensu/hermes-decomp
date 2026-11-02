function k(n) { var o = []; for (var i = 0; i < n; i++) { try { if (i === 1) { continue; } o.push(i); } catch (x) { } o.push("t"); } return o.join(); }
print(k(3));
