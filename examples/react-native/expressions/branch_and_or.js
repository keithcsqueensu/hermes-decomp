function it(x) { if (typeof Symbol !== "undefined" && x[Symbol.iterator] != null || x.alt != null) return "yes"; return "no"; }
print(it([1]), it({}), it({ alt: 1 }));
