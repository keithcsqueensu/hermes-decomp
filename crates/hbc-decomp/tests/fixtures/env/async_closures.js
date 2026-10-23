// An async function: its generator body and the closures created inside it, which
// read the body, the function around it and the factory around that.
var log = [];
function outer(tag) {
  var outerOnly = "o:" + tag;
  async function run(x) {
    var inBody = x * 2;
    await null;
    var later = function () { return outerOnly + "/" + inBody + "/" + x; };
    await null;
    log.push(later());
    return [1, 2].map(function (i) { return tag + i + inBody; }).join(",");
  }
  return run;
}
outer("a")(3).then(function (v) { log.push(v); print(log.join(" | ")); });
