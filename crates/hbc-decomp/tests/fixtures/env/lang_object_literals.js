// versions: 98 99
// `super` in an object literal method, `__proto__` in a literal, computed
// accessors, spread with getters, and shorthand methods.
var out = [];
var base = { greet() { return "base:" + this.name; } };
var k = "comp";
var obj = {
  __proto__: base,
  name: "obj",
  greet() { return "obj>" + super.greet(); },
  get [k + "Get"]() { return k.length; },
  ["key" + 1]: "v1",
};
var src = { get lazy() { return "evaluated"; }, plain: 1 };
var copy = { ...src, extra: true };
out.push(obj.greet(), obj.compGet, obj.key1, Object.getPrototypeOf(obj) === base, copy.lazy, Object.keys(copy).join("+"));
print(out.join(","));
