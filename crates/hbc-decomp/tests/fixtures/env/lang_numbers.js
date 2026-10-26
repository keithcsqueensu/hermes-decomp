// BigInt arithmetic and comparison, numeric separators, exponentiation with a
// negative base, `**=`, and bit operations on negatives.
var out = [];
var big = 2n ** 64n + 1_000n;
var x = -(2 ** 3);
var y = (-2) ** 3;
var z = 3;
z **= 2;
out.push(String(big), String(big % 7n), big > 18446744073709551615n, typeof big);
out.push(1_000_000, 0xff, x, y, z, (-7) >>> 28, -7 >> 1, 5 & ~1);
print(out.join(","));
