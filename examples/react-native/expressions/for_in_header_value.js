// A for-in whose body writes a variable read after the loop. The writes add
// moves around the key fetch, which the for-in matcher did not accept, and the
// loop printed as `Object.keys()` with no argument.
function allFunctions(o) {
  var f = true;
  for (var k in o) {
    if (typeof o[k] === "function") continue;
    f = false;
  }
  return f;
}

// Two variables written in the body, and a break.
function firstBig(o, limit) {
  var count = 0;
  var found = null;
  for (var k in o) {
    count++;
    if (o[k] > limit) {
      found = k;
      break;
    }
  }
  return count + ":" + found;
}

// A nested for-in, the inner one writing a variable of the outer body.
function pairs(a, b) {
  var n = 0;
  var last = "";
  for (var i in a) {
    for (var j in b) {
      if (a[i] === b[j]) {
        n++;
        last = i + j;
      }
    }
  }
  return n + "/" + last;
}

print(allFunctions({ a: function () {}, b: function () {} }));
print(allFunctions({ a: function () {}, b: 1 }));
print(allFunctions({}));
print(firstBig({ x: 1, y: 5, z: 9 }, 4));
print(firstBig({ x: 1 }, 4));
print(pairs({ p: 1, q: 2 }, { r: 2, s: 1, t: 3 }));
