#!/usr/bin/env bash
# Acceptance A6 (Phase 1): one soak per master rate (2 min by default), in the real
# application, with 4 synthetic 1080p sources: the master rate and three others (slower,
# fractional, faster), mixed by a transition every 4 s. One report per rate in
# tests/soak/reports/. Exit code 1 if any soak failed.
#
#   tests/soak/a6-rates.sh             # 6 × 2 min
#   VMA_SOAK_SECS=30 tests/soak/a6-rates.sh
#
# Leave the machine alone while it runs (the window must stay visible).
set -uo pipefail
cd "$(dirname "$0")/../.."
cargo build --release || exit 1

secs=${VMA_SOAK_SECS:-120}
day=$(date +%F)
status=0
run() {
    local rate=$1 inputs=$2 name=$3
    local report="tests/soak/reports/$day-phase1-a6-$name.md"
    echo "== $rate : sources $inputs → $report"
    ./target/release/vma --soak "$secs" --no-journal --rate "$rate" \
        --inputs 4 --input-rates "$inputs" --report "$report" || status=1
    grep -m1 '^\*\*Verdict' "$report" || status=1
}
run 25    25,29.97,50,60    25p
run 29.97 29.97,25,30,59.94 29.97p
run 30    30,25,29.97,60    30p
run 50    50,25,29.97,60    50p
run 59.94 59.94,25,29.97,60 59.94p
run 60    60,25,29.97,50    60p
exit $status
