var log=[]; function* g(){ try { yield 1; yield 2; } finally { log.push("fin"); } } var it=g(); it.next(); var r=it.return(9); print(r.value, r.done, log.join());
