// versions: 98 99
// A derived class with only fields gets the compiler's constructor:
// `super(...arguments)`, then the field installs, a private one and an arrow
// closing over `this` among them.
class Base {
  constructor(a, b) {
    this.sum = (a || 0) + (b || 0);
  }
  who() { return "base" + this.sum; }
}
class Fields extends Base {
  tag = "f";
  #count = 3;
  bump = () => ++this.#count;
  count() { return this.#count; }
}
var f = new Fields(2, 5);
f.bump();
print([f.tag, f.sum, f.count(), f.who()].join(" "));
