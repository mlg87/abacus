#!/usr/bin/env bash
# Coverage ratchet (v0.2): fails when line coverage of the working tree is below
# that of <base>, so every PR keeps or raises it. Measuring <base> in the same
# run, rather than reading a stored baseline, leaves no file for parallel PRs to
# fight over and nothing to bump by hand.
#
#   scripts/coverage-ratchet.sh <base>   # e.g. origin/main
#
# Needs cargo-llvm-cov (with the llvm-tools-preview component) and jq.
set -euo pipefail

base=${1:?usage: $0 <base>}

line_percent() {
  cargo llvm-cov --locked --summary-only --json | jq '.data[0].totals.lines.percent'
}

head_pct=$(line_percent)

base_dir=$(mktemp -d)
trap 'git worktree remove --force "$base_dir"' EXIT
git worktree add --quiet --detach "$base_dir" "$base"
# A target dir under this tree's keeps CI's cache of ./target warm for both
# builds, and separate from this tree's instrumented binaries.
base_target="$PWD/target/coverage-base"
base_pct=$(cd "$base_dir" && CARGO_TARGET_DIR="$base_target" line_percent)
report=$(printf '| | Line coverage |\n|---|---|\n| base (%s) | %.2f%% |\n| this tree | %.2f%% |\n' \
  "$(git rev-parse --short "$base")" "$base_pct" "$head_pct")
echo "$report"
if [[ -n ${GITHUB_STEP_SUMMARY-} ]]; then
  echo "$report" >>"$GITHUB_STEP_SUMMARY"
fi

if awk -v h="$head_pct" -v b="$base_pct" \
  'BEGIN { exit !(sprintf("%.2f", h) + 0 < sprintf("%.2f", b) + 0) }'; then
  msg=$(printf 'line coverage fell from %.2f%% to %.2f%%; cover the new code' "$base_pct" "$head_pct")
  if [[ -n ${GITHUB_ACTIONS-} ]]; then
    echo "::error::$msg"
  else
    echo "error: $msg" >&2
  fi
  exit 1
fi
