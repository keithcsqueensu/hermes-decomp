// versions: 98 99
// S3.4: a closure reads a captured object and builds its own temporary
// object; the decompiler names both after what they hold, so one spelling
// is a capture and a local in the same body. The local must not assign the
// capture, nor shadow it before the capture is read.
(function () {
  var out = [];
  function make(easing) {
    var config = { easingX: easing + "x", easingY: easing + "y" };
    return function (origin) {
      var first = config.easingX;
      var result = { initialValues: { originX: origin }, easing: config.easingY };
      out.push(first + ":" + result.easing + ":" + result.initialValues.originX);
      return result;
    };
  }
  var f = make("in");
  f(1);
  f(2);
  print(out.join(" "));
})();
