// Fixture: the host for regexp-set (P4a). Two regexes, so one test can replace an
// entry in place (`a`, whose slot the donor fits) and another must append (`b`,
// whose slot is too small). Each line prints what a site's regex matches, so a VM
// run shows which bytecode is live. Compiled by scripts/build_hermes_vm.ps1 -Fixtures.
var a = /^equinox-(\d+)$/i;
var b = /x/;
print("a " + a.test("equinox-42") + " " + a.test("NOPE-7"));
print("b " + b.test("x") + " " + b.test("nope-7-and-a-longer-tail"));
