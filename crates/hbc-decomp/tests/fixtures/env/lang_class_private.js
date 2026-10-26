// versions: 98 99
// Private methods, accessors, static private members, a static block, and
// `#x in obj` against a foreign object.
var out = [];
class Counter {
  #n = 0;
  static #made = 0;
  static #label() { return "C" + Counter.#made; }
  static { Counter.#made = 100; }
  constructor() { Counter.#made++; }
  #bump(k) { this.#n += k; return this; }
  get #twice() { return this.#n * 2; }
  set #value(v) { this.#n = v; }
  run() { this.#bump(2).#bump(3); this.#value = this.#twice + 1; return this.#n; }
  static has(o) { return #n in o; }
  static label() { return Counter.#label(); }
}
var c = new Counter();
out.push(c.run(), Counter.has(c), Counter.has({}), Counter.label());
print(out.join(","));
