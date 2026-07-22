#!/usr/bin/env bash
# Sourced by other scripts to load the active device + host profile into shell
# variables. Keeps device-specific values out of the scripts themselves.
#
#   source scripts/device.sh
#   adb -s "$DEVICE_SERIAL" shell ...
#
# Device selection: $PALMTOP_DEVICE -> config/active -> error.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${PALMTOP_CONFIG_DIR:-$REPO_ROOT/config}"

# --- host ---
source "$(dirname "${BASH_SOURCE[0]}")/host-config.sh"

# --- device ---
DEVICE_NAME="${PALMTOP_DEVICE:-}"
if [ -z "$DEVICE_NAME" ] && [ -f "$CONFIG_DIR/active" ]; then
  DEVICE_NAME="$(tr -d '[:space:]' < "$CONFIG_DIR/active")"
fi
if [ -z "$DEVICE_NAME" ]; then
  echo "error: no device selected." >&2
  echo "Available:" >&2
  ls "$CONFIG_DIR/devices"/*.toml 2>/dev/null \
    | xargs -rn1 basename | sed 's/\.toml$//' | grep -v '^example$' | sed 's/^/  /' >&2
  echo "  echo <name> > config/active     # or PALMTOP_DEVICE=<name>" >&2
  return 1 2>/dev/null || exit 1
fi

DEVICE_CONFIG="$CONFIG_DIR/devices/$DEVICE_NAME.toml"
if [ ! -f "$DEVICE_CONFIG" ]; then
  echo "error: no device profile at $DEVICE_CONFIG" >&2
  return 1 2>/dev/null || exit 1
fi

DEVICE_SERIAL="$(_toml_get "$DEVICE_CONFIG" adb serial)"
DEVICE_IP="$(_toml_get "$DEVICE_CONFIG" adb ip)"
DEVICE_MODEL="$(_toml_get "$DEVICE_CONFIG" device model)"
DEVICE_REFRESH_HZ="$(_toml_get "$DEVICE_CONFIG" display refresh_hz)"
DEVICE_MAX_FPS="$(_toml_get "$DEVICE_CONFIG" limits max_fps)"

export HOST_IP HOST_PORT VAAPI_RENDER_NODE PAIRING_TOKEN PAIRING_PUBKEY
export DEVICE_NAME DEVICE_SERIAL DEVICE_IP DEVICE_MODEL DEVICE_REFRESH_HZ DEVICE_MAX_FPS
