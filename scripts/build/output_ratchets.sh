#!/usr/bin/env bash
# Output defects no other gate sees, against per-bundle ceilings
# (scripts/build/output_baseline.tsv, IDENTITY_PLAN.md §3): modules that do not
# parse, `const`s the program assigns, and generator state machines left raw
# (GENERATOR_LIFT_PLAN.md G0: each keeps its executing-generator guard). Fails
# on the way up.
#
# Usage: bash scripts/build/output_ratchets.sh [--bundles]
#   --bundles  also the `large` rows (each takes 10-25 minutes)
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DECOMP="${DECOMP:-$ROOT/target/release/hermes-decomp}"
BASELINE="$ROOT/scripts/build/output_baseline.tsv"
MEASURE="$ROOT/scripts/measure"
LARGE=0
[ "${1:-}" = "--bundles" ] && LARGE=1
failed=0

if [ ! -d "$MEASURE/node_modules" ]; then
  printf 'skip: %s has no node_modules (npm ci there)\n' "$MEASURE"
  exit 0
fi

while IFS=$'\t' read -r label size bundle parse_max consts_max raw_max; do
  case "$label" in ''|'#'*) continue ;; esac
  if [ "$size" = "large" ] && [ "$LARGE" -eq 0 ]; then
    printf 'skip: %s (large; --bundles)\n' "$label"
    continue
  fi
  case "$bundle" in /*|[A-Za-z]:/*) path="$bundle" ;; *) path="$ROOT/$bundle" ;; esac
  if [ ! -f "$path" ]; then
    printf 'skip: %s is not on this machine\n' "$label"
    continue
  fi
  out="$(mktemp -t ratchet_XXXXXX).js"
  "$DECOMP" decompile --no-cache "$path" -o "$out" >/dev/null 2>&1
  parse=$(node --max-old-space-size=12000 "$MEASURE/undeclared.mjs" "$out" --per-module \
    | head -1 | sed -n 's/.*"unparsedChunks":\([0-9]*\).*/\1/p')
  consts=$(node --max-old-space-size=12000 "$MEASURE/const_assign.mjs" "$out" \
    | sed -n 's/.*"constsWritten":\([0-9]*\).*/\1/p')
  raw=$(grep -c 'Generator functions may not be called on executing generators' "$out")
  rm -f "$out"
  if [ -z "$parse" ] || [ -z "$consts" ]; then
    printf 'FAIL %s: could not measure\n' "$label"
    failed=1
    continue
  fi
  status=OK
  if [ "$parse" -gt "$parse_max" ] || [ "$consts" -gt "$consts_max" ] || [ "$raw" -gt "$raw_max" ]; then
    status=FAIL
    failed=1
  fi
  printf '%-4s %s: unparsable %s (ceiling %s), consts written %s (ceiling %s), raw machines %s (ceiling %s)\n' \
    "$status" "$label" "$parse" "$parse_max" "$consts" "$consts_max" "$raw" "$raw_max"
  if [ "$status" = OK ] && { [ "$parse" -lt "$parse_max" ] || [ "$consts" -lt "$consts_max" ] || [ "$raw" -lt "$raw_max" ]; }; then
    printf '     below the ceiling: lower it in %s\n' "$BASELINE"
  fi
done < "$BASELINE"
exit "$failed"
