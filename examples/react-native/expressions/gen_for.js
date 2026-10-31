function* g(n){ for (var i=0;i<n;i++){ yield i*2; } return "end"; } var it=g(3), out=[], r; while(!(r=it.next()).done) out.push(r.value); print(out.join(), r.value);
