// versions: 96 98 99
// A `for ... of` variable the body reassigns must be `let`.
(function () {
  var seen = [];
  for (var item of [1, 2, 3]) {
    if (item === 2) item = 20;
    seen.push(item);
  }
  print(seen.join(","));
})();
