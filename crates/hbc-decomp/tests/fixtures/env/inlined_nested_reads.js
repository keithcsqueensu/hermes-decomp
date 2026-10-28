// versions: 98 99
// S3.3: hermesc inlines a factory twice into one function, each copy on its
// own function environment (`arr`, `done`). The shared `start` reads `arr`
// one level up; the callback two functions below it reads and writes `done`
// and reads `arr` two levels up, through `start`'s scope, whose parent the
// scope graph cannot pick (two creation scopes). Each copy must count its own
// steps: `p` finishes after 2, `q` after 3.
(function () {
  function par(arr) {
    var done = 0;
    return {
      start: function (cb) {
        arr.forEach(function (step) {
          step(function () {
            done = done + 1;
            if (done === arr.length) cb(done);
          });
        });
      }
    };
  }
  var run = function (f) { f(); };
  var out = [];
  if (typeof print === "function") {
    var p = par([run, run]);
    var q = par([run, run, run]);
    p.start(function (n) { out.push("p" + n); });
    q.start(function (n) { out.push("q" + n); });
  }
  print(out.join(" "));
})();
