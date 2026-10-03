// Fixture: a function with a real exception-handler table (try/catch/finally),
// exercised on BOTH paths so a broken handler table is visible in the output.
// Compiled by scripts/build_hermes_vm.ps1 -Fixtures.
function risky(a, b) {
  try {
    if (a > b) { throw new Error("boom"); }
    return a + b;
  } catch (e) {
    return -1;
  } finally {
    b = 0;
  }
}
print("no-throw:", risky(1, 2));
print("throw:", risky(3, 2));
