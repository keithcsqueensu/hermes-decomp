// versions: 98 99
// S3.4: hermesc inlines a counter factory twice, each copy on its own
// environment, and the shared closure stores `count + 1` back into the slot.
// The register holding the sum is named after the slot, so the store printed
// as `count = count` and was dropped as a self-assignment: every call saw 1.
(function () {
  var now = function () { return 0; };
  function shortOut(func) {
    var count = 0, lastCalled = 0;
    return function () {
      var stamp = now(), remaining = 16 - (stamp - lastCalled);
      lastCalled = stamp;
      if (remaining > 0) {
        if (++count >= 3) return arguments[0];
      } else {
        count = 0;
      }
      return func.apply(undefined, arguments);
    };
  }
  var a = shortOut(function (x) { return "a" + x; });
  var b = shortOut(function (x) { return "b" + x; });
  print([a(1), a(2), a(3), b(4)].join(" "));
})();
