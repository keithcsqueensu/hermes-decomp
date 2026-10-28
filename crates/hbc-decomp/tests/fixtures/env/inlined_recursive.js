// versions: 98 99
// S3.4: a shared function that creates itself: hermesc inlines `chainable`
// into `optional` and into its callers, so `optional`'s method is created by
// every creator and by itself, each time on a new `pattern`. One declaration
// serves every creation; it cannot read each creation's own `pattern`.
(function () {
  var chainable = function (pattern) {
    return Object.assign(pattern, {
      optional: function () { return optional(pattern); },
      and: function (q) { return { both: [pattern.tag, q.tag] }; }
    });
  };
  function optional(pattern) {
    return chainable({ tag: "opt(" + pattern.tag + ")", kind: "optional" });
  }
  function startsWith(s) { return chainable({ tag: "starts:" + s }); }
  function endsWith(s) { return chainable({ tag: "ends:" + s }); }
  var x = startsWith("a").optional();
  var y = endsWith("b").optional().optional();
  print([x.tag, y.tag, startsWith("c").and(endsWith("d")).both.join("&")].join(" "));
})();
