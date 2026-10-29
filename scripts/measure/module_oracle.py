"""Module oracle (IDENTITY_PLAN.md section 3): run a bundle module under hvm and
its decompiled form under node, and compare what each exports.

hvm runs one file, so the bundle is made to run one module: the global
function's last calls (Metro's `__r(startup)` and `__r(entry)`) are replaced,
with `hermes-decomp asm`, by

    m = __r(N); throw typeof m + "|" + Object.keys(m).join(",")

The decompiled side is `hermes-decomp extract`, one file per module, with each
import pointed at its module's file; node imports module N and prints the same
snapshot: the type of the exports and their keys. Values are not compared yet:
serializing them calls the getters Babel's interop defines exports with, and
those read other modules (a getter that threw broke the whole snapshot). The
exports object is the default export or the named exports; both are tried.

A module that fails under hvm (most reach native modules) is not runnable and
is counted, not compared. A runnable module whose decompiled form throws, or
prints a different snapshot, is a finding.

usage: python -I scripts/measure/module_oracle.py <bundle> --hvm <hvm.exe>
           [--decomp <hermes-decomp>] [--work <dir>] [--modules 1,2,3 | --sample N]
           [--report <out.tsv>]
"""

import argparse
import json
import os
import random
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# The patch, in the decompiler's HASM dialect. Property reads go through
# `GetByVal` with the name in a register: no property cache, whose indices the
# function already gives other properties (a shared index answered `Object`
# with `globalThis`'s slot, and index 0 is a cache entry too). `undefined` is
# every call's `this`. The names are strings the bundle already has. The
# snapshot is thrown, not printed: by the time the bundle's global code ends,
# `print` is gone from the global object, and hvm reports an uncaught value as
# `Uncaught <value>` on stderr. It is built with `join` and `+`, not `JSON`:
# the bundle's `JSON.stringify` threw for any argument once its globals ran.
PATCH = """{o0}  LoadConstUndefined r2
{o1}  GetGlobalObject r0
{o2}  LoadConstString r13, "__r"
{o3}  GetByVal r3, r0, r13
{o4}  LoadConstInt r5, {module}
{o5}  Call2 r6, r3, r2, r5
{o6}  LoadConstString r13, "Object"
{o7}  GetByVal r7, r0, r13
{o8}  LoadConstString r13, "keys"
{o9}  GetByVal r8, r7, r13
{o10}  Call2 r8, r8, r7, r6
{o11}  LoadConstString r13, "join"
{o12}  GetByVal r9, r8, r13
{o13}  LoadConstString r13, ","
{o14}  Call2 r9, r9, r8, r13
{o15}  TypeOf r12, r6
{o16}  LoadConstString r13, "|"
{o17}  Add r12, r12, r13
{o18}  Add r12, r12, r9
{o19}  Throw r12
"""

NODE_RUNNER = """
const [, , dir, id] = process.argv;
globalThis.window = globalThis.window || globalThis;
globalThis.global = globalThis;
const url = require('url');
(async () => {
  let ns;
  try {
    ns = await import(url.pathToFileURL(require('path').join(dir, id + '.mjs')).href);
  } catch (e) {
    console.log('THROW\\t' + String(e && e.message || e).slice(0, 200));
    return;
  }
  const out = [];
  const named = {};
  // A module given its own namespace as `default` (no default of its own):
  // that key is the harness's, not the module's.
  const selfDefault = ns.default === ns;
  for (const k of Object.keys(ns)) if (!(selfDefault && k === 'default')) named[k] = ns[k];
  const readings = [named];
  if ('default' in ns && !selfDefault) readings.push(ns.default);
  for (const m of readings) {
    try {
      out.push(typeof m + '|' + (m == null ? [] : Object.keys(m)).join(','));
    } catch (e) {
      out.push(null);
    }
  }
  console.log('OK\\t' + JSON.stringify(out));
})();
"""

SNAP_PREFIXES = tuple(f"Uncaught {t}|" for t in
                      ("object", "function", "string", "number", "boolean", "undefined", "symbol", "bigint"))

# An import with no `/* id */` comment (and not already rewritten).
BARE_IMPORT = re.compile(r'(from\s+)"(?!\./)([^"]+)"(?!\s*/\*)')

IMPORT = re.compile(r'(from\s+)"[^"]*"(\s*/\*\s*(\d+)\s*\*/)')


def run(cmd, timeout=600, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8",
                          errors="replace", timeout=timeout, **kw)


def patched_hasm(hasm_lines, tail_start, module):
    offs = {f"o{i}": f"{0x7f000 + i:05x}" for i in range(20)}
    # The exception table (`.handler` lines, emitted last) still applies: the
    # patch only replaces the tail, after every handler range.
    handlers = [l for l in hasm_lines[tail_start:] if l.startswith(".handler")]
    return "".join(hasm_lines[:tail_start]) + PATCH.format(module=module, **offs) + "".join(handlers)


def find_tail(hasm_lines):
    # The first of the trailing `TryGetById ..., "__r"` / Call2 pairs.
    for i in range(len(hasm_lines) - 1, -1, -1):
        line = hasm_lines[i]
        if '"__r"' in line and "TryGetById" in line:
            j = i
            while j - 2 >= 0 and '"__r"' in hasm_lines[j - 2] and "TryGetById" in hasm_lines[j - 2]:
                j -= 2
            return j
    raise SystemExit("no trailing __r call in the global function")


def prepare_node(decomp, bundle, work):
    ext = os.path.join(work, "ext")
    mjs = os.path.join(work, "mjs")
    if not os.path.isdir(ext):
        r = run([decomp, "extract", bundle, "-o", ext], timeout=3600)
        if r.returncode != 0:
            raise SystemExit("extract failed: " + r.stderr[-500:])
    os.makedirs(mjs, exist_ok=True)
    names = {}
    # Module names from the extract headers (`// Name: Cache`), for an import
    # the decompiler wrote by name with no `/* id */` comment.
    by_name = {}
    for f in os.listdir(ext):
        m = re.match(r"(?:module_)?(\d+)(?:_.*)?\.js$", f)
        if not m:
            continue
        with open(os.path.join(ext, f), encoding="utf-8") as fh:
            head = fh.read(400)
        n = re.search(r"^// Name: (.+)$", head, re.M)
        by_name.setdefault(f"module_{m.group(1)}", m.group(1))
        if n:
            by_name.setdefault(n.group(1).strip(), m.group(1))
    for f in os.listdir(ext):
        # `<id>_<name>.js`, or `module_<id>.js` for a module with no name.
        m = re.match(r"(?:module_)?(\d+)(?:_.*)?\.js$", f)
        if not m:
            continue
        mid = m.group(1)
        names[mid] = f
        src = open(os.path.join(ext, f), encoding="utf-8").read()
        src = IMPORT.sub(lambda g: f'{g.group(1)}"./{g.group(3)}.mjs"{g.group(2)}', src)
        src = BARE_IMPORT.sub(
            lambda g: f'{g.group(1)}"./{by_name[g.group(2)]}.mjs"' if g.group(2) in by_name else g.group(0),
            src,
        )
        # The decompiler's `import X from "m"` is Metro's require: the module's
        # exports, which node gives only as a `default` export. A module with
        # none exports its own namespace as one.
        if not re.search(r"^export default\b|^export \{[^}]*\bas default\b", src, re.M):
            src += f'\nimport * as __self from "./{mid}.mjs";\nexport default __self;\n'
        open(os.path.join(mjs, mid + ".mjs"), "w", encoding="utf-8").write(src)
    runner = os.path.join(work, "runner.cjs")
    open(runner, "w", encoding="utf-8").write(NODE_RUNNER)
    return mjs, runner, names


def snapshot_equal(a, b):
    # `type|key,key,...`; key order differs between a namespace object and
    # `module.exports`, so the keys compare as sets.
    if a is None or b is None:
        return False
    ta, _, ka = a.partition("|")
    tb, _, kb = b.partition("|")
    return ta == tb and set(filter(None, ka.split(","))) == set(filter(None, kb.split(",")))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bundle")
    ap.add_argument("--hvm", required=True)
    ap.add_argument("--decomp", default=os.path.join(ROOT, "target", "release", "hermes-decomp"))
    ap.add_argument("--work", default=os.path.join(ROOT, "target", "oracle"))
    ap.add_argument("--modules")
    ap.add_argument("--sample", type=int, default=50)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--report")
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)

    hasm = os.path.join(a.work, "global.hasm")
    r = run([a.decomp, "emit-hasm", "--function", "0", a.bundle, "-o", hasm])
    if r.returncode != 0:
        raise SystemExit("emit-hasm failed: " + r.stderr[-500:])
    lines = open(hasm, encoding="utf-8").readlines()
    tail = find_tail(lines)

    mjs, runner, names = prepare_node(a.decomp, a.bundle, a.work)
    if a.modules:
        ids = [m.strip() for m in a.modules.split(",") if m.strip()]
    else:
        pool = sorted(names, key=int)
        random.Random(a.seed).shuffle(pool)
        ids = pool[: a.sample]

    rows = []
    counts = {"not runnable": 0, "match": 0, "mismatch": 0, "decompiled throws": 0}
    for mid in ids:
        ph = os.path.join(a.work, "patch.hasm")
        open(ph, "w", encoding="utf-8").write(patched_hasm(lines, tail, mid))
        hbc = os.path.join(a.work, "patched.hbc")
        r = run([a.decomp, "asm", "--function", "0", "--allow-stale-debug-info", "-o", hbc,
                 a.bundle, ph])
        if r.returncode != 0:
            raise SystemExit("asm failed: " + (r.stderr or r.stdout)[-800:])
        h = run([a.hvm, hbc], timeout=120)
        err = (h.stderr or "") + (h.stdout or "")
        snap = next((l[len("Uncaught "):] for l in err.splitlines() if l.startswith(SNAP_PREFIXES)), None)
        if snap is None:
            counts["not runnable"] += 1
            first = next((l for l in err.splitlines() if l.strip()), "")
            rows.append((mid, names.get(mid, "?"), "not runnable", first.strip()[:120]))
            continue
        expected = snap
        n = run(["node", runner, mjs, mid], timeout=120)
        line = (n.stdout.strip().splitlines() or [""])[-1]
        if not line.startswith("OK\t"):
            why = line[len("THROW\t"):] if line.startswith("THROW\t") else line
            kind = "decompiled throws"
            if re.search(r"\b(exports|module) is not defined|require is not a function", why):
                kind = "commonjs left in the output"
            elif "Cannot find package" in why or "Cannot find module" in why:
                kind = "unresolved import"
            elif "Unexpected token '<'" in why:
                # JSX, in this module or one it imports: node runs it only
                # through a transform, which the harness does not have yet.
                kind = "jsx, not run"
            elif "SyntaxError" in (n.stderr or "") or not line.startswith("THROW"):
                kind = "does not parse"
            counts[kind] = counts.get(kind, 0) + 1
            rows.append((mid, names.get(mid, "?"), kind, why[:160]))
            continue
        readings = json.loads(line[3:])
        if any(snapshot_equal(expected, r_) for r_ in readings if r_ is not None):
            counts["match"] += 1
            rows.append((mid, names.get(mid, "?"), "match", ""))
        else:
            counts["mismatch"] += 1
            rows.append((mid, names.get(mid, "?"), "mismatch",
                         "hvm " + expected[:100] + " | node " + str(readings)[:100]))
    for row in rows:
        if row[2] != "not runnable":
            print("\t".join(row))
    print(json.dumps(counts))
    if a.report:
        with open(a.report, "w", encoding="utf-8") as f:
            for row in rows:
                f.write("\t".join(row) + "\n")
    return 1 if any(v for k, v in counts.items() if k not in ("match", "not runnable", "jsx, not run")) else 0


if __name__ == "__main__":
    sys.exit(main())
