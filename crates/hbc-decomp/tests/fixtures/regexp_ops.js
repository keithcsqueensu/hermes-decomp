// Fixture: one pattern per regex opcode the production bundles do not use, plus
// the common ones, so the regex decoder's test sees all 29 (RegexOpcodes.def).
// Unicode mode (u) selects the U16* forms; s selects MatchAny. Compiled by
// scripts/build_hermes_vm.ps1 -Fixtures.
var all = [
  /^$/, /./, /./s, /./u, /./su,
  /a/, /é/, /😀/u, /abc/, /abc/i, /a/i, /é/i, /𐐀/iu,
  /a|b/, /[a-z]/, /[a-z]/u, /(a)\1/, /\bx\B/, /(?=a)(?!b)(?<=c)/,
  /(a)*/, /(?:ab)*/, /a*/, /a{2,5}?/
];
print(all.length);
