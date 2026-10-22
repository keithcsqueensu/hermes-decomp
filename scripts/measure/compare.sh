#!/usr/bin/env bash
# Compare two decompiled outputs of the same bundle: how much changed, what the
# changed identifiers became, and which names became undeclared.
#
# usage: bash scripts/measure/compare.sh <base.js> <new.js> [top-N]
#        PER_MODULE=1 counts a name declared only by its own module (see
#        undeclared.mjs --per-module); use it when the two outputs differ in
#        which module declares a name at top level.
#
# Needs node with this directory's packages installed (`npm ci` here once) and
# python. Writes <new>.diff and <base|new>.undeclared.tsv beside the inputs.
#
# Read it in this order. `worse:` lists every name with more undeclared
# references than in the base: each is a reference the new output broke, and
# is the first thing to chase (`node undeclared.mjs <new.js> --show <name>`).
# Then the `name -> name` substitutions, which say whether renames improved
# names or swapped one real name for another. The totals come last: a total
# can fall while a new name breaks.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
base=${1:?base.js}
new=${2:?new.js}
top=${3:-25}

git diff --no-index -U1 "$base" "$new" > "$new.diff"
echo "hunks: $(grep -c '^@@' "$new.diff")"
for f in "$base" "$new"; do
  echo "$(basename "$f"): $(node --max-old-space-size=8192 "$here/undeclared.mjs" "$f" --dump "$f.undeclared.tsv" ${PER_MODULE:+--per-module})"
done
echo -n "worse: "
join -t $'\t' -a2 -e0 -o 0,1.2,2.2 \
  <(LC_ALL=C sort "$base.undeclared.tsv") <(LC_ALL=C sort "$new.undeclared.tsv") \
  | awk -F'\t' '$3 > $2 { printf "%s:%d>%d ", $1, $2, $3 } END { print "" }'
python -I "$here/substitutions.py" "$new.diff" "$top"
