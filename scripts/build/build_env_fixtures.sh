#!/usr/bin/env bash
# Compile the environment-model fixtures (crates/hbc-decomp/tests/fixtures/env)
# at every fixture version and record what each prints.
#
# usage: HERMESC_V96=... HERMESC_V98=... HERMESC_V99=... \
#        HERMES_VM_V96=... HERMES_VM_V98=... HERMES_VM_V99=... \
#        bash scripts/build/build_env_fixtures.sh
#
# A fixture compiles at every version unless its first line narrows it
# (`// versions: 98 99`: v96 hermesc has no classes or async arrows; Metro
# transpiles them away for v96 bundles).
#
# For each <name>.js and version N it writes <name>.vN.hbc (`hermesc -O
# -emit-binary`, as a release bundle is built) and <name>.vN.expected, what
# that version's hvm prints. The expected output is the bytecode's behaviour,
# not the source's: v96 Hermes has no block scoping and v98/v99 give a loop
# `let` one binding for all iterations, so a block-scoped fixture prints
# differently per version, and different again under node (noted, not fatal).
# tests/env_model.rs decompiles each .hbc and asserts the decompiled program
# prints the same as its .expected.
set -euo pipefail

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../crates/hbc-decomp/tests/fixtures/env" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for src in "$dir"/*.js; do
  name=$(basename "$src" .js)
  { echo 'globalThis.print = (...a) => console.log(...a);'; cat "$src"; } > "$tmp/run.js"
  node "$tmp/run.js" 2>/dev/null | tr -d '\r' > "$tmp/node.out" || true
  versions=$(sed -n '1s#^// versions: ##p' "$src")
  for v in ${versions:-96 98 99}; do
    hc_var="HERMESC_V$v" vm_var="HERMES_VM_V$v"
    hc=${!hc_var:?set $hc_var}
    vm=${!vm_var:?set $vm_var}
    "$hc" -O -emit-binary -out "$dir/$name.v$v.hbc" "$src"
    "$vm" "$dir/$name.v$v.hbc" | tr -d '\r' > "$dir/$name.v$v.expected"
    cmp -s "$tmp/node.out" "$dir/$name.v$v.expected" || note=" (differs from node)"
    echo "$name v$v: $(head -c 70 "$dir/$name.v$v.expected")${note:-}"
    unset note
  done
done
