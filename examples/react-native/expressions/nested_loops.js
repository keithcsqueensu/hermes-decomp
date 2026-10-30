function grid(a) {
  var t = a.map(function () { return []; });
  for (var j = 0; j < 3; j++) {
    for (var i = 0; i < 3; i++) {
      t[(i + j) % 2].push(i * 10 + j);
    }
  }
  var sizes = t.map(function (x) { return x.length; });
  return [t, sizes];
}
print(JSON.stringify(grid([1, 2])));
