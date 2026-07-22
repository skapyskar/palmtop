#!/usr/bin/env bash
# Build, install, and launch the real Palmtop client against the running
# palmtopd daemon. Assumes palmtopd is already running (start it separately:
# `cargo run --release -p palmtopd`, or ./scripts/install-service.sh) -- this
# script only handles the client.
#
#   ./scripts/run-client.sh
#   PALMTOP_DEVICE=other-phone ./scripts/run-client.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/scripts/device.sh"
[ -f "$HOME/opt/android-env.sh" ] && source "$HOME/opt/android-env.sh"
export JAVA_HOME="${JAVA_HOME:-$HOME/opt/jdk-17.0.19+10}"

echo "device : $DEVICE_NAME ($DEVICE_MODEL) @ $DEVICE_SERIAL"
echo "host   : $HOST_IP:$HOST_PORT"

if ! (echo > "/dev/tcp/$HOST_IP/$HOST_PORT") 2>/dev/null; then
  echo "warning: nothing is listening on $HOST_IP:$HOST_PORT yet." >&2
  echo "  Start the daemon first:  cargo run --release -p palmtopd" >&2
fi

if [ -z "$PAIRING_TOKEN" ] || [ -z "$PAIRING_PUBKEY" ]; then
  echo "error: no pairing token/pubkey in config/host.toml yet." >&2
  echo "  palmtopd generates both on first run -- start it at least once, then retry." >&2
  exit 1
fi

echo "[..] building + installing client (Gradle -- see android/, not android-spike/)"
(cd "$REPO_ROOT/android" && ./gradlew assembleDebug --console=plain > /dev/null)
adb -s "$DEVICE_SERIAL" install -r "$REPO_ROOT/android/app/build/outputs/apk/debug/app-debug.apk" \
  2>&1 | grep -E "Success|Failure"

adb -s "$DEVICE_SERIAL" shell am force-stop dev.palmtop.client
adb -s "$DEVICE_SERIAL" shell input keyevent KEYCODE_WAKEUP
sleep 1
adb -s "$DEVICE_SERIAL" shell am start -n dev.palmtop.client/.MainActivity \
  --es host "$HOST_IP" --ei port "$HOST_PORT" \
  --es token "$PAIRING_TOKEN" --es pubkey "$PAIRING_PUBKEY"

echo "[..] launched -- phone must be UNLOCKED for the SurfaceView to attach"
echo "     watch logs with: adb -s $DEVICE_SERIAL logcat -s PalmtopClient"
