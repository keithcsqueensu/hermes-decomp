function t(n) { var o = []; for (var i = 0; i < n; i++) { try { o.push(i); } catch (e) { o.push("x"); } } return o.join(); }
print(t(3));
