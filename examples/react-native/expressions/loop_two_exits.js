function f(n) {
  for (var i = 0; i < n; i++) {
    print("i", i);
    if (i === 1) { print("stop"); return; }
  }
  print("done");
}
f(3); print("-"); f(1); print("-"); f(0);
