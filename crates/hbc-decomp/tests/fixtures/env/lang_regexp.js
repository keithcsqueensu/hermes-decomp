// Named groups, the sticky, unicode and dotAll flags, a replacer function and
// matchAll.
var out = [];
var m = /(?<y>\d{4})-(?<m>\d{2})/.exec("on 2024-06");
out.push(m.groups.y, m.groups.m);
var sticky = /a/y;
sticky.lastIndex = 1;
out.push(sticky.test("ba"), sticky.lastIndex);
out.push(/^.$/su.test("\n"), /\u{1F600}/u.test("\u{1F600}"));
out.push("a1b22".replace(/\d+/g, (d) => "<" + d.length + ">"));
out.push([..."x1y2".matchAll(/[a-z](\d)/g)].map((r) => r[1]).join("+"));
print(out.join(","));
