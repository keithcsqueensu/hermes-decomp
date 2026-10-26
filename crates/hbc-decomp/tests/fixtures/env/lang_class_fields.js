// versions: 98 99
// Public fields with initialisers reading `this`, arrow fields, static fields,
// computed keys, accessors, `super.method()` and static inheritance.
var out = [];
var key = "dyn";
class Base {
  static kind = "base";
  tag = "B";
  hello() { return "hello " + this.tag; }
  static make() { return new this(); }
}
class Child extends Base {
  tag = "C";
  len = this.tag.length + 1;
  arrow = () => this.tag + this.len;
  [key + "Method"]() { return "computed"; }
  get upper() { return this.tag.toLowerCase(); }
  set upper(v) { this.tag = v.toUpperCase(); }
  hello() { return super.hello() + "!"; }
  static kind = Base.kind + "+child";
}
var c = Child.make();
var a = c.arrow;
c.upper = "z";
out.push(c.hello(), a(), c.dynMethod(), c.upper, Child.kind, c instanceof Base);
print(out.join(","));
