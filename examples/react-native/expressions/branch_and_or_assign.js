function t(a, b, c) { let r; if (a && b || c) { r = "then"; } else { r = "else"; } return r; }
function u(a, b, c, d) { let r = 0; if ((a && b || c) && d) { r = 1; } else { r = 2; } return r + 10; }
function v(a, b, c) { let x = "x", y = "y"; if (a && b || c) { x = 1; } else { y = 2; } return x + "," + y; }
function w(a, b, c) { const o = []; for (let i = 0; i < 2; i++) { let r; if (a && b || c) { r = i; } else { r = -i; } o.push(r); } return o.join(); }
function z(a, b, c) { let n = 0; if (a && b || c) { n += 1; } else { n += 2; } if (a && c || b) { n += 10; } else { n += 20; } return n; }
function each(a, b, c) { print(a, b, c, t(a, b, c), u(a, b, c, 1), u(a, b, c, 0), v(a, b, c), w(a, b, c), z(a, b, c)); }
each(0,0,0); each(0,0,1); each(0,1,0); each(0,1,1); each(1,0,0); each(1,0,1); each(1,1,0); each(1,1,1);
