var log = [];
function note(x) { log.push(x); return x; }
function* straight() { try { yield 1; yield 2; } finally { note("fin"); } yield 3; return 4; }
function* two(x) { try { yield x; } finally { note("a"); note(x); } return x + 1; }
function* throwing() { try { yield 1; note(boom()); yield 2; } finally { note("fin"); } }
function* before(x) { note("start"); yield 0; try { yield x; } finally { note("fin"); } }
function boom() { throw 7; }
function step(it, act) {
  try {
    var r = act === 1 ? it.return(9) : act === 2 ? it.throw(8) : it.next();
    return r.value + "," + r.done;
  } catch (e) {
    return "threw," + e;
  }
}
function drive(it, a, b, c, d) {
  var out = [step(it, a), step(it, b), step(it, c), step(it, d), step(it, 0)];
  print(out.join(" "), log.join());
  log.length = 0;
}
drive(straight(), 0, 0, 0, 0); drive(straight(), 0, 1, 0, 0); drive(straight(), 0, 0, 1, 0); drive(straight(), 0, 0, 0, 1);
drive(straight(), 0, 2, 0, 0); drive(straight(), 0, 0, 2, 0); drive(straight(), 1, 0, 0, 0);
drive(two(1), 0, 0, 0, 0); drive(two(1), 0, 1, 0, 0); drive(two(1), 0, 2, 0, 0);
drive(throwing(), 0, 0, 0, 0); drive(throwing(), 0, 1, 0, 0);
drive(before(1), 0, 0, 0, 0); drive(before(1), 0, 1, 0, 0); drive(before(1), 0, 0, 1, 0); drive(before(1), 0, 0, 2, 0);
