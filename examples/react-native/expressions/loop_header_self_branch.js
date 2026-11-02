function b(n) { var o = []; var i = 0; while (true) { var r = n[i++]; o.push(r * 2); if (r > 2) { continue; } if (r === 0) { break; } o.push("y"); } return o.join(); }
print(b([1, 3, 0]));
