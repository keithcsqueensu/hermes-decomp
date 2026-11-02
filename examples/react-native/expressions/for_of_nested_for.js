function a(v) { return v + 1; }
function b(v) { return v + 2; }
for (const F of [a, b]) {
  for (const v of [0, 1]) {
    const x = F(v);
    const out = [];
    for (let n = 0; n < 2; n++) {
      out.push(x + n);
    }
    print(F.name, v, out.join());
  }
}
