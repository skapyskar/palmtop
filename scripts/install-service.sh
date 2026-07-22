#!/usr/bin/env bash
# Registers palmtopd as a systemd --user service: substitutes real paths into
# the unit template, and enables+starts it.
#
#   ./scripts/install-service.sh --status                # just show status
#   ./scripts/install-service.sh                          # repo checkout: builds from source
#   ./scripts/install-service.sh --bin <path> --config-dir <dir> --unit-template <path>
#                                                           # explicit paths (used by install.sh)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_NAME="palmtopd.service"

if [ "${1:-}" = "--status" ]; then
  systemctl --user status "$UNIT_NAME" --no-pager || true
  exit 0
fi

BIN=""
CONFIG_DIR=""
UNIT_TEMPLATE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --config-dir) CONFIG_DIR="$2"; shift 2 ;;
    --unit-template) UNIT_TEMPLATE="$2"; shift 2 ;;
    *) echo "error: unknown argument: $1" >&2; exit 1 ;;
  esac
done

# Called with no arguments: assume a repo checkout and build from source, as
# this script always did before install.sh learned to pass explicit paths
# (needed once install.sh also had to run from an extracted release tarball,
# where there is no Cargo.toml to build from).
if [ -z "$BIN" ]; then
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
  echo "[..] building release binary"
  cargo build --release -q -p palmtopd --manifest-path "$REPO_ROOT/Cargo.toml"
  BIN="$REPO_ROOT/target/release/palmtopd"
  CONFIG_DIR="${CONFIG_DIR:-$REPO_ROOT/config}"
  UNIT_TEMPLATE="${UNIT_TEMPLATE:-$REPO_ROOT/systemd/palmtopd.service}"
fi
[ -x "$BIN" ] || { echo "error: no executable at $BIN" >&2; exit 1; }
[ -f "$UNIT_TEMPLATE" ] || { echo "error: no unit template at $UNIT_TEMPLATE" >&2; exit 1; }

for var in WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR; do
  if ! systemctl --user show-environment 2>/dev/null | grep -q "^${var}="; then
    echo "warning: \$${var} not in the systemd --user environment." >&2
    echo "  Portal/PipeWire capture will likely fail under the service even if it" >&2
    echo "  works fine when run interactively. Most compositors import this" >&2
    echo "  automatically; if yours doesn't, add to its startup:" >&2
    echo "    systemctl --user import-environment $var" >&2
  fi
done

if [ ! -f "$CONFIG_DIR/host.toml" ]; then
  echo "error: $CONFIG_DIR/host.toml missing -- run ./install.sh first" >&2
  exit 1
fi

mkdir -p "$UNIT_DIR"
sed -e "s|__PALMTOPD_BIN__|$BIN|g" \
    -e "s|__PALMTOP_WORKDIR__|$CONFIG_DIR|g" \
    -e "s|__PALMTOP_CONFIG_DIR__|$CONFIG_DIR|g" \
  "$UNIT_TEMPLATE" > "$UNIT_DIR/$UNIT_NAME"

echo "[..] installed $UNIT_DIR/$UNIT_NAME"
systemctl --user daemon-reload
systemctl --user enable "$UNIT_NAME"
# `enable --now`/`start` are no-ops on an already-active unit, which silently
# left a stale binary running after a rebuild here more than once -- restart
# unconditionally so a freshly built binary always actually takes effect.
systemctl --user restart "$UNIT_NAME"

sleep 1
echo "[..] status:"
systemctl --user status "$UNIT_NAME" --no-pager || true
echo
echo "logs: journalctl --user -u $UNIT_NAME -f"
