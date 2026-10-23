// Count identifier references that no scope declares, in decompiled output.
//
// usage: node undeclared.mjs <decompiled.js> [--dump <names.tsv>] [--show <name>] [--per-module]
//        [--unparsed <errors.tsv>]
//
// The output is split at the decompiler's `// === Module` / `// === Orphan`
// markers, and the orphan section again at each top-level function (orphans are
// unrelated functions, so one duplicate name there would otherwise hide them all).
// Each part is parsed as an ES module with JSX (acorn, acorn-jsx)
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
// --unparsed writes `header<TAB>parse error<TAB>offending line` for each part
// that does not parse, the first line of its marker as the header.
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
const unparsedOut = opt('--unparsed');
const unparsedRows = [];
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
let inOrphans = false;
for (const l of src.split('\n')) {
  const marker = /^\/\/ === (Module|Orphan)/.test(l);
  if (marker) inOrphans = l.startsWith('// === Orphan');
  const orphanFn = inOrphans && /^(?:async )?function/.test(l);
  if ((marker || orphanFn) && cur.length) {
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

// Defect counters (OUTPUT_DEFECTS_PLAN D0): text patterns, each a symptom of
// one known defect, so a fix has a number before and after.
const patterns = {
  tmpInTemplate: /\$\{tmp\d+\}/g, // B4: a renamed definition, its use left behind
  emptyRestSlice: /\[\.\.\.arguments\]\.slice\(\)/g, // B3: the rest index lost
  hermesBuiltin: /\bHermesBuiltin\./g, // B1, and the unwritten v98 folding pass
  builtinN: /\bbuiltin\d+\b/g, // B1: an index past the table
  gotoBlock: /\bgoto B/g, // B9: structuring off
  privateTwice: /Cannot initialize private field twice/g, // B8
};
const counters = {};
for (const [k, re] of Object.entries(patterns)) counters[k] = (src.match(re) || []).length;
let intrinsicTags = 0;

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
  } catch (e) {
    unparsed++;
    if (unparsedOut) {
      const lines = text.split('\n');
      const at = e.loc ? (lines[e.loc.line - 1] || '').trim().slice(0, 160) : '';
      unparsedRows.push(`${lines[0]}\t${e.message.replace(/ \(\d+:\d+\)$/, '')}\t${at}`);
    }
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
        else intrinsicTags++;
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
let argUndeclared = 0;
for (const [n, c] of byName) {
  undeclared += c;
  if (isPlaceholder(n)) placeholder += c;
  if (/^arg\d+$/.test(n)) argUndeclared += c; // B2: a dropped parameter
}
if (unparsedOut) fs.writeFileSync(unparsedOut, unparsedRows.join('\n') + '\n');
if (dump) {
  const rows = [...byName].map(([n, c]) => `${n}\t${c}`).sort();
  fs.writeFileSync(dump, rows.join('\n') + '\n');
}
console.log(JSON.stringify({
  undeclared, placeholderUndeclared: placeholder, distinct: byName.size,
  unparsedChunks: unparsed, unparsedLines, lowercaseTags,
}));
// intrinsicTags: lowercase JSX that is not a binding. React Native has no DOM
// elements, so on these bundles each is a `document.createElement` printed as JSX (B6).
console.log(JSON.stringify({ intrinsicTags, argUndeclared, ...counters }));
