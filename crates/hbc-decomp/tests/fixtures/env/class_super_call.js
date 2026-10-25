// versions: 98 99
// An explicit `super(a, 1)` (Reflect.construct of the parent with new.target),
// constructed through a function hermesc cannot see into, so the constructor is
// not inlined at the `new` site (class_super covers that case).
class Base {
  constructor(a, b) {
    this.sum = (a || 0) + (b || 0);
  }
  who() { return "base" + this.sum; }
}
class Own extends Base {
  #scale;
  constructor(a) {
    super(a, 1);
    this.#scale = a * 10;
  }
  scaled() { return this.sum * this.#scale; }
}
class Plain extends Base {
  constructor(a, b) {
    super(b, a);
    this.tag = "p" + a;
  }
}
var make = [function (x, y, C) { return new C(x, y); }][0];
var o = make(4, undefined, Own);
var p = make(2, 3, Plain);
print([o.scaled(), o.who(), p.who(), p.tag].join(" "));
