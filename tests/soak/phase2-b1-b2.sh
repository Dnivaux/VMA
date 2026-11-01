#!/usr/bin/env bash
# Acceptance B1 + B2 (Phase 2), one run in the real application (1 h by default):
# 6 synthetic 1080p50 inputs mixed by a transition every 4 s, one recording (MKV) and one
# RTMP stream to a local MediaMTX, sync test on; the stream is read back from MediaMTX and
# beep − flash measured every second. Report in tests/soak/reports/; exit code ≠ 0 on failure.
#
#   VMA_MEDIAMTX=/path/to/mediamtx tests/soak/phase2-b1-b2.sh
#   VMA_SOAK_SECS=60 VMA_MEDIAMTX=… tests/soak/phase2-b1-b2.sh     # short trial
#
# MediaMTX (MIT) is not part of the repository: download v1.21.2 and check its SHA-256
# against .github/workflows/ci.yml. The stream key is random and passed through the
# environment: it never appears on the command line nor in the report.
# Leave the machine alone while it runs (the window must stay visible).
set -uo pipefail
cd "$(dirname "$0")/../.."
: "${VMA_MEDIAMTX:?set VMA_MEDIAMTX to the MediaMTX binary}"
cargo build --release || exit 1

secs=${VMA_SOAK_SECS:-3600}
port=${VMA_RTMP_PORT:-1935}
day=$(date +%F)
report="tests/soak/reports/$day-phase2-b1-b2-linux.md"
work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null; rm -rf "$work"' EXIT

cat > "$work/mediamtx.yml" <<EOF
logLevel: warn
rtsp: false
hls: false
webrtc: false
srt: false
moq: false
api: false
metrics: false
pprof: false
playback: false
rtmp: true
rtmpAddress: 127.0.0.1:$port
paths:
  all_others:
EOF
(cd "$work" && exec "$VMA_MEDIAMTX" mediamtx.yml) > "$work/mediamtx.log" 2>&1 &
server=$!
sleep 1

VMA_STREAM_KEY=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
export VMA_STREAM_KEY
mkdir -p "$work/rec"
echo "== B1 + B2 : $secs s → $report"
./target/release/vma --soak "$secs" --no-journal --inputs 6 \
    --record "$work/rec" \
    --stream "rtmp://127.0.0.1:$port/live" --sync-test \
    --sync-check "rtmp://127.0.0.1:$port/live/$VMA_STREAM_KEY" \
    --report "$report"
status=$?
grep -m1 '^\*\*Verdict' "$report" || status=1
ls -l "$work/rec"
exit $status
