// A trailing `return undefined` in nested code, with code after it.
function evt(a, b) {
  print("start");
  if (a) {
    if (!b()) {
      return;
    }
    print("after inner");
  }
  print("end");
}
function loopTail(n) {
  while (n > 0) { print("once"); return; }
  print("no loop");
}
function sw(x) {
  switch (x) {
    case 1: print("one"); return;
    case 2: print("two"); break;
    default: print("other"); return;
  }
  print("after switch");
}
function inTry(a) {
  try { if (a) { print("t"); return; } print("u"); } catch (e) { print("c"); }
  print("after try");
}
evt(1, function () { print("called"); return false; }); print("-");
evt(1, function () { print("called"); return true; }); print("-");
evt(0); print("-");
loopTail(1); print("-"); loopTail(0); print("-");
sw(1); print("-"); sw(2); print("-"); sw(3); print("-");
inTry(1); print("-"); inTry(0);
