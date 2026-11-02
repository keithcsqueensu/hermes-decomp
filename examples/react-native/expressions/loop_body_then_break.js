function run(n) { const out = []; let i = 0; for (let r = n[i]; ; r = n[++i]) { out.push(r); if (r === 0) break; } return out; }
print(run([3, 2, 0]).join());
