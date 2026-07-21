#!/usr/bin/env bash
# Build, install and launch the decode spike against the active device,
# with host/device values pulled from config/ rather than hardcoded.
#
#   ./scripts/run-decode-spike.sh [inflight] [fps]
#   PALMTOP_DEVICE=other-phone ./scripts/run-decode-spike.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/scripts/device.sh"
[ -f "$HOME/opt/android-env.sh" ] && source "$HOME/opt/android-env.sh"

INFLIGHT="${1:-1}"
FPS="${2:-$DEVICE_MAX_FPS}"
STREAM="$REPO_ROOT/android-spike/test-1080p.h264"

echo "device : $DEVICE_NAME ($DEVICE_MODEL) @ $DEVICE_SERIAL"
echo "host   : $HOST_IP:$HOST_PORT  vaapi=$VAAPI_RENDER_NODE"
echo "run    : ${FPS}fps inflight=$INFLIGHT"

# Regenerate the test stream if missing (it's gitignored).
if [ ! -f "$STREAM" ]; then
  echo "[..] generating test stream"
  ffmpeg -y -hide_banner -loglevel error \
    -init_hw_device "vaapi=va:$VAAPI_RENDER_NODE" \
    -f lavfi -i "testsrc=size=1920x1080:rate=$FPS:duration=10" \
    -vf 'format=nv12,hwupload' -c:v h264_vaapi -qp 24 -bf 0 -g "$FPS" \
    -f h264 "$STREAM"
fi

echo "[..] building + installing app"
(cd "$REPO_ROOT/android-spike" && ./build.sh > /dev/null)
adb -s "$DEVICE_SERIAL" install -r "$REPO_ROOT/android-spike/palmtop-spike.apk" \
  2>&1 | grep -E "Success|Failure"

echo "[..] starting stream server"
cargo build --release -q -p spike-h264-server --manifest-path "$REPO_ROOT/Cargo.toml"
"$REPO_ROOT/target/release/spike-h264-server" "$STREAM" "$FPS" "$HOST_PORT" \
  > /tmp/palmtop-server.log 2>&1 &
SERVER_PID=$!
trap 'kill $SERVER_PID 2>/dev/null || true' EXIT
sleep 2

adb -s "$DEVICE_SERIAL" shell am force-stop dev.palmtop.spike
adb -s "$DEVICE_SERIAL" logcat -c
adb -s "$DEVICE_SERIAL" shell input keyevent KEYCODE_WAKEUP
sleep 1
adb -s "$DEVICE_SERIAL" shell am start -n dev.palmtop.spike/.MainActivity \
  --es host "$HOST_IP" --ei port "$HOST_PORT" --ei inflight "$INFLIGHT" > /dev/null

echo "[..] measuring (phone must be UNLOCKED -- SurfaceView needs a visible surface)"
sleep 14
adb -s "$DEVICE_SERIAL" logcat -d -s PalmtopSpike | grep -E "frames=|RESULT" | tail -4
