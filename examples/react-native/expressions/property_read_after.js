// A value read from a property, used after a store or a call that changes that
// property: the old value has to be the one used. The inliner carried the read past
// the store and printed the new value.
function f(a, i) { var t = a[i]; a[i] = 5; return t + ":" + t + ":" + a[i]; }
function g(a, i, h) { var t = a[i]; h(a); return [t, t, a[i]].join(); }
function k(a) { var t = a[0] + a[1]; a[0] = 7; return t + ":" + t + ":" + a[0]; }
function m(a) { var t = -a.x; a.x = 1; return t + ":" + t + ":" + a.x; }
print(f([1, 2], 0), g([2], 0, function (x) { x[0] = 9; }), k([3, 4]), m({ x: 4 }));
