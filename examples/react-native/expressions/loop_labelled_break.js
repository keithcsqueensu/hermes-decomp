// `break outer` and `continue outer` from an inner loop. The outer limits are far past
// the break, so a plain `break` runs on and prints something else, or does not end.
function both(n) {
  var o = [];
  outer: for (var i = 0; i < n; i++) {
    for (var j = 0; j < 4; j++) {
      if (j === 2) continue outer;
      if (i === 3) break outer;
      o.push(i + "" + j);
    }
    o.push("end" + i);
  }
  return o.join();
}
function exitAfterInner(n) {
  var o = [];
  outer: for (var i = 0; i < n; i++) {
    var j = 0;
    while (j < 3) {
      if (i * j === 4) break outer;
      if (j === i) break;
      j++;
    }
    o.push(i + ":" + j);
  }
  o.push("after");
  return o.join();
}
function threeDeep(n) {
  var o = [];
  a: for (var i = 0; i < n; i++) {
    for (var j = 0; j < n; j++) {
      for (var k = 0; k < n; k++) {
        if (k === 1) continue a;
        if (i === 2) break a;
        o.push("" + i + j + k);
      }
    }
  }
  return o.join();
}
function innerFinishes(n, m) {
  var o = [];
  a: for (var i = 0; i < n; i++) {
    for (var j = 0; j < m; j++) {
      for (var k = 0; k < m; k++) {
        if (k === 5) continue a;
        if (i === 2) break a;
        o.push("" + i + j + k);
      }
      o.push("j" + j);
    }
    o.push("i" + i);
  }
  return o.join();
}
function inForIn(o) {
  var r = [];
  a: for (var k in o) {
    var v = o[k];
    for (var i = 0; i < v.length; i++) {
      if (v[i] === 0) {
        r.push("zero" + k);
        continue a;
      }
      r.push(v[i]);
    }
    r.push("end" + k);
  }
  return r.join();
}
function bareLoops(n) {
  var i = 0;
  var o = [];
  outer: while (true) {
    var j = 0;
    while (true) {
      o.push(i + "" + j);
      if (i === n) break outer;
      j++;
      if (j > 1) break;
    }
    i++;
  }
  return o.join();
}
print(both(1000));
print(exitAfterInner(1000));
print(threeDeep(1000));
print(innerFinishes(1000, 2));
print(innerFinishes(1000, 7).length);
print(inForIn({ p: [1, 0, 2], q: [3, 4], s: [] }));
print(bareLoops(3));
