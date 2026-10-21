// Fixture: a function that constructs and throws (`thrower`, whose `new Error`
// is a Construct reading its arguments from the end of the frame) and one that
// catches it (`caller`, with a handler table). The log stub needs a 12-register
// frame: where the compiler gave `thrower` less, growing it would move
// Construct's arguments, so the stub must be refused there and nowhere else.
// Compiled by scripts/build_hermes_vm.ps1 -Fixtures.
function thrower(a) { if (a > 1) { throw new Error("boom"); } return a; }
function caller(a) { try { return thrower(a); } catch (e) { return -1; } }
print("ok:", caller(1));
print("throw:", caller(3));
