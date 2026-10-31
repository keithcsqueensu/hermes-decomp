function* g(){ var i=0; while(true){ var v = yield i; if (v === "stop") break; i++; } return i; } var it=g(); it.next(); it.next(); it.next(); var r=it.next("stop"); print(r.value, r.done);
