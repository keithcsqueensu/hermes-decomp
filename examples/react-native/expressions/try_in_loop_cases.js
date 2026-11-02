function boom(i) { if (i === 1) throw "b" + i; return i; }
function a(n) { var o = []; for (var i = 0; i < n; i++) { try { o.push(boom(i)); } catch (e) { o.push(e); } } return o.join(); }
function b(n) { var o = []; for (var i = 0; i < n; i++) { try { if (i === 1) { continue; } o.push(i); } catch (x) { } o.push("t"); } return o.join(); }
function c(n) { var o = []; for (var i = 0; i < n; i++) { try { if (i === 1) { break; } o.push(boom(i)); } catch (x) { o.push("c"); } o.push("t"); } return o.join(); }
function d(n) { var o = []; var i = 0; while (i < n) { try { o.push(boom(i)); } catch (e) { o.push(e); } i++; } return o.join(); }
function e(n) { var o = []; try { for (var i = 0; i < n; i++) { o.push(boom(i)); } } catch (x) { o.push(x); } return o.join(); }
function f(n) { var o = []; for (var i = 0; i < n; i++) { for (var j = 0; j < 2; j++) { try { o.push(boom(j) + i); } catch (x) { o.push("c" + i); } } } return o.join(); }
function g(n) { for (var i = 0; i < n; i++) { try { if (boom(i) === 2) { return "ret" + i; } } catch (x) { } } return "end"; }
function h(n) { var o = []; for (var i = 0; i < n; i++) { o.push("a"); try { o.push(boom(i)); } catch (x) { o.push("c"); continue; } o.push("z"); } return o.join(); }
print(a(3)); print(b(3)); print(c(3)); print(d(3)); print(e(3)); print(f(2)); print(g(4)); print(g(1)); print(h(3));
