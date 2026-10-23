// Count identifier references that no scope declares, in decompiled output.
//
// usage: node undeclared.mjs <decompiled.js> [--dump <names.tsv>] [--show <name>] [--per-module]
//
// The output is split at the decompiler's `// === Module` / `// === Orphan`
// markers and each part is parsed as an ES module with JSX (acorn, acorn-jsx)
// and scope-analysed (eslint-scope). A reference left in a module's global scope that is not a
// global of the JS runtime, nor a name some top-level statement of the file
// declares, is counted. Parts that do not parse are skipped and reported:
// compare two outputs only when `unparsedChunks` agrees, or the counts cover
// different code.
//
// A JSX tag is a reference too, which eslint-scope does not see: `<Foo/>` and
// the root of `<a.b/>` are resolved here against the enclosing scopes. A
// lowercase tag is an intrinsic element (a string), not a reference, so a
// lowercase tag that names a binding or a placeholder (`<closure_0/>`,
// `<require/>`) is the decompiler printing a component as a string; those
// are counted as `lowercaseTags`.
//
// --dump writes `name<TAB>count` for every undeclared name, sorted, so two runs
// can be compared by name (compare.sh); a total can fall while a new name
// appears. --show prints the line of each undeclared reference to <name>.
// --per-module ignores declarations from other modules: a reference counts as
// declared only if its own module binds it. Two outputs that differ in which
// module happens to declare a name at top level then compare honestly.
import fs from 'node:fs';
import * as acorn from 'acorn';
import jsx from 'acorn-jsx';
import * as eslintScope from 'eslint-scope';
import estraverse from 'estraverse';

const Parser = acorn.Parser.extend(jsx());

const args = process.argv.slice(2);
const file = args[0];
const opt = (flag) => {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : undefined;
};
const dump = opt('--dump');
const show = opt('--show');
if (!file) {
  console.error('usage: node undeclared.mjs <decompiled.js> [--dump <names.tsv>] [--show <name>]');
  process.exit(2);
}

const src = fs.readFileSync(file, 'utf8');
const chunks = [];
const starts = [];
let cur = [];
let lineNo = 0;
let start = 1;
for (const l of src.split('\n')) {
  if (/^\/\/ === (Module|Orphan)/.test(l) && cur.length) {
    chunks.push(cur.join('\n'));
    starts.push(start);
    start = lineNo + 1;
    cur = [];
  }
  cur.push(l);
  lineNo++;
}
chunks.push(cur.join('\n'));
starts.push(start);

// Runtime globals, plus what Hermes and Metro provide to every module.
const known = new Set(
  Object.getOwnPropertyNames(globalThis).concat([
    'print', 'arguments', 'undefined', 'global', 'require', 'module', 'exports',
    'HermesInternal', '__d', '__r', 'nativeModuleProxy',
  ]),
);
// A top-level declaration anywhere in the file covers a reference in another
// part: the decompiler hoists some declarations out of the module they serve.
const topDecl = new Set();
const perModule = args.includes('--per-module');
if (!perModule) for (const m of src.matchAll(/^(?:export )?(?:async )?(?:function\*? |class |const |let |var )([A-Za-z_$][\w$]*)/gm)) {
  topDecl.add(m[1]);
}

const byName = new Map();
let unparsed = 0;
let unparsedLines = 0;
let lowercaseTags = 0;
const isPlaceholder = (n) => /^closure_\d/.test(n) || /^c\d+$/.test(n);
const count = (n, line) => {
  byName.set(n, (byName.get(n) || 0) + 1);
  if (n === show) console.log('at', line);
};
chunks.forEach((text, i) => {
  let ast;
  try {
    ast = Parser.parse(text, {
      ecmaVersion: 'latest', sourceType: 'module', ranges: true, locations: true,
      allowReturnOutsideFunction: true, allowAwaitOutsideFunction: true,
    });
  } catch {
    unparsed++;
    unparsedLines += text.split('\n').length;
    return;
  }
  const manager = eslintScope.analyze(ast, {
    ecmaVersion: 2022, sourceType: 'module', fallback: 'iteration',
  });
  const lineOf = (node) => starts[i] + node.loc.start.line - 1;
  for (const ref of manager.globalScope.through) {
    const n = ref.identifier.name;
    if (known.has(n) || topDecl.has(n)) continue;
    count(n, lineOf(ref.identifier));
  }
  // JSX tags, resolved through the scopes enclosing them.
  const stack = [];
  const bound = (n) => stack.some((s) => s.set.has(n));
  estraverse.traverse(ast, {
    fallback: 'iteration',
    enter(node) {
      const s = manager.acquire(node, true);
      if (s) stack.push(s);
      node.__scoped = !!s;
      if (node.type !== 'JSXOpeningElement') return;
      let name = node.name;
      while (name.type === 'JSXMemberExpression') name = name.object;
      if (name.type !== 'JSXIdentifier') return;
      const n = name.name;
      const lowercase = node.name.type === 'JSXIdentifier' && /^[a-z]/.test(n);
      if (lowercase) {
        if (isPlaceholder(n) || bound(n)) lowercaseTags++;
        return;
      }
      if (bound(n) || known.has(n) || topDecl.has(n)) return;
      count(n, lineOf(name));
    },
    leave(node) {
      if (node.__scoped) stack.pop();
    },
  });
});

let undeclared = 0;
let placeholder = 0;
for (const [n, c] of byName) {
  undeclared += c;
  if (isPlaceholder(n)) placeholder += c;
}
if (dump) {
  const rows = [...byName].map(([n, c]) => `${n}\t${c}`).sort();
  fs.writeFileSync(dump, rows.join('\n') + '\n');
}
console.log(JSON.stringify({
  undeclared, placeholderUndeclared: placeholder, distinct: byName.size,
  unparsedChunks: unparsed, unparsedLines, lowercaseTags,
}));
