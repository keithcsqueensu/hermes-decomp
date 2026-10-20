// Fixture: the host for regexp-set and regexp-repoint (P4a). Two regexes, so one
// test can replace an entry in place (`a`, whose slot the donor fits) and another
// must append (`b`, whose slot is too small), and `b`'s site can be repointed at
// `a`'s entry. Each `test` line shows which bytecode is live; the last line shows
// what `b` *reports* (`.source`, `.flags`), which is not the same thing.
// Compiled by scripts/build_hermes_vm.ps1 -Fixtures.
var a = /^equinox-(\d+)$/i;
var b = /x/;
print("a " + a.test("equinox-42") + " " + a.test("NOPE-7"));
print("b " + b.test("x") + " " + b.test("nope-7-and-a-longer-tail") + " " + b.test("EQUINOX-1"));
print("b.source " + b.source + " /" + b.flags);
