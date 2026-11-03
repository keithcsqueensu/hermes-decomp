// `n++` read after `n` is written: the old value has to be the one read. An object
// literal fold moved the read above its value, and the inliner re-read `n` after the
// write.
function counter() {
  var n = 0;
  return function () {
    var o = {};
    o.done = n++ > 1;
    return o;
  };
}
function pair() {
  var n = 5;
  return function () {
    var a = n++;
    var b = n;
    return a + ":" + b + ":" + (a > 5);
  };
}
var c = counter();
print(c().done, c().done, c().done);
var p = pair();
print(p(), p());
