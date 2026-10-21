function factory(seed) {
  var p = seed + 1;
  var q = seed + 2;
  var BB = { call: function (a, b, c) { print("BB", a, b, c, p, q); return "r"; } };
  function genMethod(moduleID, methodID, type) {
    var fn;
    if (type === "promise") {
      fn = function () {
        return new Promise(function (resolve, reject) {
          BB.call(moduleID, methodID, "p");
          resolve(1);
        });
      };
    } else {
      fn = function () {
        if (type === "sync") {
          return BB.call(moduleID, methodID, "s");
        }
        BB.call(moduleID, methodID, "a");
      };
    }
    fn.type = type;
    return fn;
  }
  return genMethod;
}
var g = factory(1);
g(7, 9, "promise")();
print(g(7, 9, "sync")());
g(7, 9, "async")();
print(g(1, 2, "sync").type);
