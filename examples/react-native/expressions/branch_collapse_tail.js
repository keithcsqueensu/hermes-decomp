var has = Object.prototype.hasOwnProperty;
function m(arr, g, t) {
  var out = [];
  for (var i = 0; i < arr.length; i++) {
    var s = arr[i];
    switch (s.type) {
      case "Type":
      case "Property": {
        var k = s.type === "Type" ? "types" : "properties";
        var d = has.call(g, k) ? g[k][s.name] : null;
        if (!d || !d.match) {
          throw new Error("Bad syntax reference: " + (s.type === "Type" ? "<" + s.name + ">" : "<'" + s.name + "'>"));
        }
        if (t !== false && t !== null && s.type === "Type") {
          if (s.name === "custom" && t.kind === 1 || s.name === "length" && t.value === "0") {
            out.push("t" + s.name);
            continue;
          }
        }
        out.push(d.match);
        break;
      }
      case "Keyword":
        out.push(s.name);
        break;
      default:
        throw new Error("unknown " + s.type);
    }
    out.push(";");
  }
  return out.join("");
}
function r(a, g, t) { try { return m(a, g, t); } catch (e) { return e.message; } }
var g = { types: { a: { match: "A" }, b: {}, custom: { match: "C" }, length: { match: "L" } }, properties: { p: { match: "P" } } };
print(r([{ type: "Type", name: "a" }, { type: "Property", name: "p" }, { type: "Keyword", name: "k" }], g, null));
print(r([{ type: "Type", name: "b" }], g, null));
print(r([{ type: "Property", name: "zz" }], g, null));
print(r([{ type: "Type", name: "custom" }], g, { kind: 1 }));
print(r([{ type: "Type", name: "custom" }], g, { kind: 2 }));
print(r([{ type: "Type", name: "length" }], g, { value: "0" }));
print(r([{ type: "Other" }], g, null));
