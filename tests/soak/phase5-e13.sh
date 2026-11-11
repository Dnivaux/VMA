#!/usr/bin/env bash
# Acceptance E13 (Phase 5), one run in the real application (20 min by default): the D11 setup
# (below), plus the MultiView on a full screen (`--fullscreen`, first screen by default) and a
# client of the compat API (`--api-load`: a command every 100 ms, the XML every 250 ms).
#
# D11 setup: the C14 setup
# (6 sync sources 1080p50 with sound, two ±100 ppm off, transitions between their flashes,
# one recording, one RTMP stream read back and beep − flash measured) plus, over the Program,
# 4 overlays: 3 HTML pages (two animated, one taking data every second) and a translucent
# PNG; one transition in three is a stinger (a white ball, 1 s, 1080p, no sound). Everything
# keyed is light: the analyser needs a flash with no pixel below 170 of luma. Report in
# tests/soak/reports/; exit code ≠ 0 on failure.
#
#   VMA_MEDIAMTX=/path/to/mediamtx tests/soak/phase5-e13.sh
#   VMA_SOAK_SECS=60 VMA_MEDIAMTX=… tests/soak/phase5-e13.sh
#   VMA_SCREEN=2                       # the screen of the MultiView (name or number; default 1)     # short trial
#   CEF_PATH=…                         # where CEF is kept (default: ~/.cache/vma-cef)
#   VMA_KEEP_DIR=…                     # where the recording is kept on failure (default: /tmp)
#
# MediaMTX (MIT) is not part of the repository: download v1.21.2 and check its SHA-256
# against .github/workflows/ci.yml. Leave the machine alone while it runs.
set -uo pipefail
cd "$(dirname "$0")/../.."
: "${VMA_MEDIAMTX:?set VMA_MEDIAMTX to the MediaMTX binary}"
cargo build --release || exit 1
CEF_PATH=${CEF_PATH:-$HOME/.cache/vma-cef} \
    cargo build --release -p vma-io-html --features cef --bin vma-html || exit 1

secs=${VMA_SOAK_SECS:-1200}
port=${VMA_RTMP_PORT:-1935}
day=$(date +%F)
report="tests/soak/reports/$day-phase5-e13-linux.md"
work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null; rm -rf "$work"' EXIT

# A translucent white PNG, and the stinger: a white ball on transparent, 50 pictures at 50p.
gst-launch-1.0 -q videotestsrc pattern=white num-buffers=1 \
    ! video/x-raw,width=480,height=120 ! videoconvert ! video/x-raw,format=RGBA \
    ! alpha alpha=0.5 ! pngenc snapshot=true ! filesink location="$work/panel.png" || exit 1
# The background drawn transparent by the source itself: keyed by `alpha method=custom`, it
# stayed opaque black, and every stinger hid a second of the Program (and one white picture
# of a flash when it ended on one).
gst-launch-1.0 -q videotestsrc pattern=ball background-color=0x00000000 num-buffers=50 \
    ! video/x-raw,width=1920,height=1080,framerate=50/1,format=RGBA \
    ! avenc_png ! qtmux ! filesink location="$work/stinger.mov" || exit 1

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
(cd "$work" && exec "$VMA_MEDIAMTX" mediamtx.yml) > "$work/mediamtx.log" 2>&1 &
server=$!
sleep 1

VMA_STREAM_KEY=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
export VMA_STREAM_KEY
mkdir -p "$work/rec"
pages=tests/soak/pages
echo "== E13 : $secs s → $report"
./target/release/vma --soak "$secs" --no-journal \
    --inputs 6 --sync-inputs --input-drifts 0,0,0,0,100,-100 \
    --html "$pages/bar.html" --html "$pages/clock.html" --html "$pages/score.html" \
    --overlay-image "$work/panel.png" --overlays --html-data \
    --stinger "$work/stinger.mov" \
    --record "$work/rec" \
    --stream "rtmp://127.0.0.1:$port/live" \
    --sync-check "rtmp://127.0.0.1:$port/live/$VMA_STREAM_KEY" \
    --fullscreen "${VMA_SCREEN:-1}" --fullscreen-source multiview \
    --api-load \
    --report "$report"
status=$?
grep -m1 '^\*\*Verdict' "$report" || status=1
ls -l "$work/rec"
if [ "$status" -ne 0 ]; then
    keep="${VMA_KEEP_DIR:-${TMPDIR:-/tmp}}/vma-e13-$(date +%F-%H%M%S)"
    mv "$work/rec" "$keep" && echo "Enregistrement gardé : $keep"
fi
exit $status
