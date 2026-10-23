#!/usr/bin/env bash
# How a bundle reaches its environments: the opcode shapes the closure model
# has to understand (docs/plan_guides/03_analysis/closure_model/V98_ENV_PLAN.md §2).
#
# usage: bash scripts/measure/env_census.sh <bundle> [hermes-decomp.exe]
#
# Prints each environment opcode by operand count, then the uses of a
# "slot-held" environment: a register written by LoadFromEnvironment and then
# used as an environment operand. That count is a linear scan of each
# function with no control flow, so it is approximate in both directions; it
# is a census, not a resolver. v96 has no slot-held environments at all.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bundle=${1:?bundle}
hd=${2:-$here/../../target/release/hermes-decomp.exe}
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT

"$hd" disasm "$bundle" | tr -d '\r' > "$tmp"
echo "functions: $(grep -c '^function ' "$tmp")"
grep -oE '^\s*(Create(Function|Top[A-Za-z]*|Inner)?Environment|GetEnvironment|GetParentEnvironment|GetClosureEnvironment|CreateGenerator[A-Za-z]*|CreateAsyncClosure[A-Za-z]*)\b.*' "$tmp" \
  | awk '{ op = $1; n = gsub(/,/, ","); print op " (" n + 1 " operands)" }' \
  | LC_ALL=C sort | uniq -c | LC_ALL=C sort -rn
awk -f "$here/env_census.awk" "$tmp"
