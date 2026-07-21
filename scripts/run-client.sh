#!/usr/bin/env bash
# Build, install, and launch the real Palmtop client against the running
# palmtopd daemon. Assumes palmtopd is already running (start it separately:
# `cargo run --release -p palmtopd`) -- this script only handles the client.
#
#   ./scripts/run-client.sh
#   PALMTOP_DEVICE=other-phone ./scripts/run-client.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/scripts/device.sh"
[ -f "$HOME/opt/android-env.sh" ] && source "$HOME/opt/android-env.sh"

echo "device : $DEVICE_NAME ($DEVICE_MODEL) @ $DEVICE_SERIAL"
echo "host   : $HOST_IP:$HOST_PORT"

if ! (echo > "/dev/tcp/$HOST_IP/$HOST_PORT") 2>/dev/null; then
  echo "warning: nothing is listening on $HOST_IP:$HOST_PORT yet." >&2
  echo "  Start the daemon first:  cargo run --release -p palmtopd" >&2
fi

if [ -z "$PAIRING_TOKEN" ]; then
  echo "error: no pairing token in config/host.toml yet." >&2
  echo "  palmtopd generates one on first run -- start it at least once, then retry." >&2
  exit 1
fi

echo "[..] building + installing client"
(cd "$REPO_ROOT/android-spike" && ./build.sh > /dev/null)
adb -s "$DEVICE_SERIAL" install -r "$REPO_ROOT/android-spike/palmtop-spike.apk" \
  2>&1 | grep -E "Success|Failure"

adb -s "$DEVICE_SERIAL" shell am force-stop dev.palmtop.spike
adb -s "$DEVICE_SERIAL" shell input keyevent KEYCODE_WAKEUP
sleep 1
adb -s "$DEVICE_SERIAL" shell am start -n dev.palmtop.spike/.MainActivity \
  --es host "$HOST_IP" --ei port "$HOST_PORT" --es token "$PAIRING_TOKEN"

echo "[..] launched -- phone must be UNLOCKED for the SurfaceView to attach"
echo "     watch logs with: adb -s $DEVICE_SERIAL logcat -s PalmtopClient"
