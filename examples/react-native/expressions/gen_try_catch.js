function* g(){ try { yield 1; yield 2; } catch(e){ yield "caught " + e; } yield 3; } var it=g(); var a=it.next().value; var b=it.throw("x").value; var c=it.next().value; print(a, b, c);
