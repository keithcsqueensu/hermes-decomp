// Tagged templates with cooked and raw strings, String.raw, nested templates
// and a template object reused at one site.
var out = [];
function tag(strings, ...vals) {
  return strings.raw.join("|") + "#" + strings.join("|") + "#" + vals.join("+");
}
var n = 2;
out.push(tag`a${n}b\n${n * 3}c`);
out.push(String.raw`x\ty${n}`);
out.push(`outer ${`inner ${n > 1 ? "big" : "small"}`} end`);
function same(s) { return s; }
function site() { return same`k`; }
out.push(site() === site());
print(out.join(" / "));
