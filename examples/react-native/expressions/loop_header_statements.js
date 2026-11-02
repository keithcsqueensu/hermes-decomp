function a(n) { var o = []; var i = 0; for (var r = n[i]; ; r = n[++i]) { o.push(r); if (r === 0) break; } return o.join(); }
function d(n) { var o = []; var i = 0; for (;;) { var x = n[i]; o.push(x); if (x === 0) { o.push("end"); break; } i++; } return o.join() + i; }
function f(n) { var c = 0; for (var i = 0; n[i] !== 0; i++) { c += n[i]; } return c; }
function g(m) { var o = []; for (var i = 0; ; i++) { var row = m[i]; o.push(row.length); if (row.length === 0) break; for (var j = 0; ; j++) { o.push(row[j]); if (j >= row.length - 1) break; } } return o.join(); }
print(a([3, 2, 0]));
print(d([4, 0]));
print(f([4, 5, 0]));
print(g([[1, 2], [3], []]));
