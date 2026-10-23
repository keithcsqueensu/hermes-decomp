// Block environments: a loop `let` captured per iteration, a captured catch
// binding, shadowing in nested blocks, and a block inside a switch case.
var fns = [];
function run(limit) {
  var tag = "t" + limit;
  for (let i = 0; i < limit; i++) {
    let sq = i * i;
    fns.push(() => tag + i + "/" + sq);
  }
  try {
    throw new Error("boom" + limit);
  } catch (e) {
    fns.push(function () { return tag + e.message; });
  }
  {
    let tag = "inner";
    {
      let tag2 = tag + "2";
      fns.push(() => tag + tag2);
    }
  }
  switch (limit) {
    case 2: {
      let c = limit * 10;
      fns.push(() => c + tag);
      break;
    }
  }
}
run(2);
run(3);
print(fns.map((f) => f()).join(","));
