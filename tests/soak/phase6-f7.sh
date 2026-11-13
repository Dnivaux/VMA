#!/usr/bin/env bash
# Acceptance F7 (Phase 6), one run in the real application (20 min by default): « le direct
# n'est pas touché ». 4 sync sources 1080p50 with sound (the cameras), recorded for the replay
# (100 Mb/s each, 5 min kept: old segments deleted during the run) and as ISO files; every
# 30 s « −5 s » then « Rejouer » (50, 25, 75 %), another angle, backwards one cycle in three
# (`--replay-load`); the Program recorded and streamed to a local RTMP server. Verdict: 0 missed
# and 0 repeated frame at the Program, every « Rejouer » back on its camera, no picture lost by
# a recorder or the player; disk and processor measured. Report in tests/soak/reports/; exit
# code ≠ 0 on failure.
#
#   tests/soak/phase6-f7.sh
#   VMA_SOAK_SECS=120 tests/soak/phase6-f7.sh      # short trial
#   VMA_MEDIAMTX=…                  # the MediaMTX binary (default: ~/.cache/vma-mediamtx/mediamtx)
#   VMA_REPLAY_DIR=…                # where the replay and the ISO files go: a fast disk, about
#                                   # 1 Go a minute and a camera (default: ~/.cache/vma-soak-f7)
#   VMA_KEEP_DIR=…                  # where the recordings are kept on failure (default: /tmp)
#
# MediaMTX (MIT) is not part of the repository: download v1.21.2 and check its SHA-256
# against .github/workflows/ci.yml. Leave the machine alone while it runs.
set -uo pipefail
cd "$(dirname "$0")/../.."
mediamtx=${VMA_MEDIAMTX:-$HOME/.cache/vma-mediamtx/mediamtx}
[ -x "$mediamtx" ] || { echo "MediaMTX introuvable : $mediamtx (VMA_MEDIAMTX)"; exit 1; }
cargo build --release || exit 1

secs=${VMA_SOAK_SECS:-1200}
port=${VMA_RTMP_PORT:-1935}
report="tests/soak/reports/$(date +%F)-phase6-f7-linux.md"
replay=${VMA_REPLAY_DIR:-$HOME/.cache/vma-soak-f7}
work=$(mktemp -d)
rm -rf "$replay"
trap 'kill "$server" 2>/dev/null; rm -rf "$work"' EXIT

cat > "$work/mediamtx.yml" <<YML
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
YML
(cd "$work" && exec "$mediamtx" mediamtx.yml) > "$work/mediamtx.log" 2>&1 &
server=$!
sleep 1

VMA_STREAM_KEY=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
export VMA_STREAM_KEY
mkdir -p "$work/rec"
echo "== F7 : $secs s → $report (replay et ISO dans $replay)"
./target/release/vma --soak "$secs" --no-journal --no-autosave \
    --rate 50 --resolution 1080p --inputs 4 --sync-inputs \
    --replay-load "$replay" \
    --record "$work/rec" \
    --stream "rtmp://127.0.0.1:$port/live" \
    --report "$report"
status=$?
grep -m1 '^\*\*Verdict' "$report" || status=1
ls -l "$work/rec"
du -sh "$replay"/* 2>/dev/null
if [ "$status" -ne 0 ]; then
    keep="${VMA_KEEP_DIR:-${TMPDIR:-/tmp}}/vma-f7-$(date +%F-%H%M%S)"
    mkdir -p "$keep" && mv "$work/rec" "$keep/program" && echo "Program gardé : $keep"
    echo "Replay et ISO gardés : $replay"
else
    rm -rf "$replay"
fi
exit $status
