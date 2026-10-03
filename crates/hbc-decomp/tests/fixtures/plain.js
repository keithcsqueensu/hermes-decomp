// Fixture: no exception handlers anywhere, so every size-changing write op is
// legal on every function. Has a "print" string in the table, which inject-stub
// log requires. Compiled by scripts/build_hermes_vm.ps1 -Fixtures.
var tag = "alpha";
function greet(n) { return "hi " + n + " " + tag; }
function twice(x) { return x + x; }
print(greet("bob"));
print(twice(5));
