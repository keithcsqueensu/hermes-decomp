var log = [];
function* caught() { try { yield 1; log.push("body"); throw 5; } catch (e) { log.push(e); yield 2; } finally { log.push("fin"); } return 6; }
var a = caught(); a.next(); var r1 = a.return(9); print(r1.value, r1.done, log.join());
log.length = 0;
var b = caught(); b.next(); b.next(); var r2 = b.next(); print(r2.value, r2.done, log.join());
