function* g1(x) { try { if (x) return 1; yield 2; } catch (e) { yield 3; } yield 4; return 5; }
function* g2(x) { try { if (x) return f(x); yield 2; } catch (e) { return 7; } yield 4; return g(x); }
function* g3(x) { try { yield 1; if (x) return f(x); yield 2; if (x > 1) return g(x); yield 5; } catch (e) { yield 3; } return h(x); }
function f(x) { if (x > 1) throw x; return x + 1; }
function g(x) { return x + 2; }
function h(x) { return x + 3; }
function run(it) {
  const out = [];
  for (let n = 0; n < 6; n++) {
    const r = it.next();
    out.push(r.value, r.done);
  }
  return out.join();
}
print(run(g1(0)), run(g1(1)));
print(run(g2(0)), run(g2(1)), run(g2(2)));
print(run(g3(0)), run(g3(1)), run(g3(2)));
