// Optional chaining (member, index, call), nullish coalescing and the logical
// assignments, each against null, undefined, 0 and "".
var out = [];
var o = { a: { b: 0, f() { return "f" + this.b; } }, n: null, arr: [1, 2] };
out.push(o?.a?.b, o.n?.b, o.missing?.x.y, o.arr?.[1], o.a.f?.(), o.nofn?.());
out.push(o.n ?? "dflt", o.a.b ?? "zero-kept", "" ?? "e", undefined ?? null ?? "last");
var x = 0, y = 1, z = null, w = "s";
x ||= 5; y &&= 7; z ??= 9; w ??= "no";
var obj = { k: 0 };
obj.k ||= 3; obj.m ??= 4; obj.k &&= obj.k + 1;
out.push(x, y, z, w, obj.k, obj.m);
print(out.map(String).join(","));
