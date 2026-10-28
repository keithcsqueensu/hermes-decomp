// Assignments to a `const` in decompiled output: a `const x = ...` the program
// later writes (`x = ...`, `x++`, `[x] = ...`). Such a file parses and every
// name is declared, so neither undeclared.mjs nor a parse check sees it, but the
// write throws a TypeError when it runs. A write from a nested function (a
// callback assigning a captured variable) is counted apart: that is the shape a
// wrong declaration kind for a captured slot takes.
//
// usage: node const_assign.mjs <decompiled.js> [--list <rows.tsv>]
import fs from 'node:fs';
import * as acorn from 'acorn';
import jsx from 'acorn-jsx';
import * as eslintScope from 'eslint-scope';

const Parser = acorn.Parser.extend(jsx());
const args = process.argv.slice(2);
const file = args[0];
const listAt = args.indexOf('--list');
const listOut = listAt >= 0 ? args[listAt + 1] : undefined;
if (!file) {
  console.error('usage: node const_assign.mjs <decompiled.js> [--list <rows.tsv>]');
  process.exit(2);
}

// Split like undeclared.mjs: one chunk per module, and per orphan function.
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

const fnScope = (s) => {
  while (s && s.type !== 'function' && s.type !== 'module' && s.type !== 'global') s = s.upper;
  return s;
};

let consts = 0;
let sameFunction = 0;
let nested = 0;
let unparsed = 0;
const rows = [];
chunks.forEach((text, i) => {
  let ast;
  try {
    ast = Parser.parse(text, {
      ecmaVersion: 'latest', sourceType: 'module', ranges: true, locations: true,
      allowReturnOutsideFunction: true, allowAwaitOutsideFunction: true,
    });
  } catch {
    unparsed++;
    return;
  }
  const manager = eslintScope.analyze(ast, {
    ecmaVersion: 2022, sourceType: 'module', fallback: 'iteration',
  });
  const header = text.slice(0, text.indexOf('\n'));
  for (const scope of manager.scopes) {
    for (const v of scope.variables) {
      const def = v.defs.find((d) => d.type === 'Variable' && d.parent.kind === 'const');
      if (!def) continue;
      const writes = v.references.filter((r) => r.isWrite() && !r.init);
      if (!writes.length) continue;
      consts++;
      for (const r of writes) {
        const inner = fnScope(r.from) !== fnScope(scope);
        if (inner) nested++;
        else sameFunction++;
        rows.push([
          starts[i] + r.identifier.loc.start.line - 1,
          inner ? 'nested' : 'same',
          v.name,
          header,
        ].join('\t'));
      }
    }
  }
});
if (listOut) fs.writeFileSync(listOut, rows.join('\n') + '\n');
console.log(JSON.stringify({ constsWritten: consts, writesSameFunction: sameFunction, writesNested: nested, unparsedChunks: unparsed }));
