// Fixture: donor regexes for regexp-set (P4a), in table order. Entry 0 fits the
// host's `a` slot and keeps its one capture group and `i` flag; entry 1 is larger
// than the host's `b` slot and has the same (empty) flags; entry 2 has no capture
// group, so putting it over `a` must be refused. Compiled by
// scripts/build_hermes_vm.ps1 -Fixtures.
var d0 = /^nope-(\d+)$/i;
var d1 = /^nope-\d+-and-a-longer-tail$/;
var d2 = /^zzz$/i;
print(d0.test("") + " " + d1.test("") + " " + d2.test(""));
