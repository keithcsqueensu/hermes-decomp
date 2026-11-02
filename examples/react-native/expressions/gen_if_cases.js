function* a(x) { if (x) { yield 1; } else { yield 2; } yield 3; }
function* b(x) { if (x) { yield 1; yield 2; } yield 3; return 4; }
function* c(x) { if (x) { yield 1; } else if (x === 0) { yield 2; } else { yield 5; } yield 3; }
function* d(x) { var v; if (x) { v = yield 1; } else { v = yield 2; } yield v; }
function* e(x) { for (var i = 0; i < x; i++) { if (i % 2) { yield i; } else { yield -i; } } return x; }
function all(it) { var o = []; var r = it.next("s"); while (!r.done) { o.push(r.value); r = it.next(o.length); } o.push(r.value); return o.join(); }
print(all(a(1)), all(a(0)));
print(all(b(1)), all(b(0)));
print(all(c(1)), all(c(0)), all(c(null)));
print(all(d(1)), all(d(0)));
print(all(e(3)));
var r = a(0); r.next(); print(JSON.stringify(r.return(9)), JSON.stringify(r.next()));
