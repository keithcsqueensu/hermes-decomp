// versions: 98 99
// Subclassing Error and Array, `new.target` in a plain function, and
// Reflect.construct with a different new.target.
var out = [];
class MyError extends Error {
  constructor(msg) { super(msg); this.name = "MyError"; }
}
class Stack extends Array {
  peek() { return this[this.length - 1]; }
}
function Who() { return new.target === Who ? "direct" : typeof new.target; }
var e = new MyError("bad");
var s = new Stack();
s.push(1, 2, 3);
out.push(e instanceof Error, e.message, e.name, s.peek(), s.length, s instanceof Array);
out.push(new Who() instanceof Who, Reflect.construct(Who, [], Object) instanceof Who);
print(out.join(","));
