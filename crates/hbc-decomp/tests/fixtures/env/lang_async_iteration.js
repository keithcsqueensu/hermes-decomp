// versions: 98 99
// for await over an array of promises and over an async iterable, and an
// async arrow awaiting in a loop.
var out = [];
var asyncIterable = {
  [Symbol.asyncIterator]() {
    var i = 0;
    return { next() { i++; return Promise.resolve({ value: i * 10, done: i > 3 }); } };
  },
};
async function main() {
  for await (var v of [Promise.resolve(1), 2, Promise.resolve(3)]) out.push(v);
  for await (var w of asyncIterable) out.push(w);
  var sum = async (xs) => { var t = 0; for (var x of xs) t += await x; return t; };
  out.push(await sum([1, Promise.resolve(2), 3]));
  print(out.join(","));
}
main();
