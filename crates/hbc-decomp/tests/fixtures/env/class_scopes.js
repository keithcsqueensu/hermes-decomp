// versions: 98 99
// Classes: a static block, a private field and method, methods closing over
// the constructor's locals and the function the class expression is made in.
function factory(seed) {
  var outer = seed * 2;
  const K = class {
    static made = 0;
    static {
      this.made = seed + 1;
    }
    #secret;
    constructor(x) {
      var local = x + outer;
      this.#secret = local;
      this.read = () => local + this.#secret;
    }
    #hidden() { return this.#secret + outer; }
    peek() { return [this.#hidden(), K.made].join("/"); }
  };
  return K;
}
var C1 = factory(1);
var C2 = factory(5);
var a = new C1(10);
var b = new C2(20);
print([a.read(), a.peek(), b.read(), b.peek()].join(" "));
