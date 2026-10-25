// Spread calls and literals: `f(1, 2, ...s)` (HermesBuiltin.apply with the
// arguments built by arraySpread), a method applied to its own object, and an
// element stored at the index a spread returns (`[0, ...s, 5]`), and a
// spread call whose arguments are loaded before the spread.
var out = [];
function f() { return Array.prototype.join.call(arguments, "."); }
var s = [3, 4];
var tag = { name: "T" };
out.push(f(1, 2, ...s));
out.push(f(tag.name, "x", ...s));
var o = { k: 7, m() { return this.k + ":" + Array.prototype.join.call(arguments, "."); } };
out.push(o.m(1, ...s));
out.push(o.m(...s));
out.push([0, ...s, 5].join("."));
// The argument array built from loads the register-level fold stops at.
var logger = { name: "L", enabled: true };
var nativeLog = function () { out.push(Array.prototype.join.call(arguments, ".")); };
function info(msg, ...rest) {
  if (logger.enabled) {
    if (nativeLog != null) {
      nativeLog(logger.name, "info", msg, ...rest);
    }
  }
}
info("m", 1, 2);
print(out.join(" | "));
