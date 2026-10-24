// versions: 98 99
// A derived class with its own constructor: `super(a, 1)` lowers to a
// construct of the parent with new.target, and hermesc inlines the
// constructor at the `new` site.
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
var o = new Own(4);
print([o.scaled(), o.who()].join(" "));
