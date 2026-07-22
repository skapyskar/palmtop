#!/usr/bin/env bash
# Installs palmtopd as a systemd --user service: builds a release binary,
# substitutes real paths into the unit template, and enables+starts it.
#
#   ./scripts/install-service.sh          # install/update and start
#   ./scripts/install-service.sh --status # just show current status
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_NAME="palmtopd.service"

if [ "${1:-}" = "--status" ]; then
  systemctl --user status "$UNIT_NAME" --no-pager || true
  exit 0
fi

for var in WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR; do
  if ! systemctl --user show-environment 2>/dev/null | grep -q "^${var}="; then
    echo "warning: \$${var} not in the systemd --user environment." >&2
    echo "  Portal/PipeWire capture will likely fail under the service even if it" >&2
    echo "  works fine when run interactively. Most compositors import this" >&2
    echo "  automatically; if yours doesn't, add to its startup:" >&2
    echo "    systemctl --user import-environment $var" >&2
  fi
done

echo "[..] building release binary"
cargo build --release -q -p palmtopd --manifest-path "$REPO_ROOT/Cargo.toml"
BIN="$REPO_ROOT/target/release/palmtopd"
[ -x "$BIN" ] || { echo "error: build did not produce $BIN" >&2; exit 1; }

if [ ! -f "$REPO_ROOT/config/host.toml" ]; then
  echo "error: config/host.toml missing -- run ./scripts/probe-host.sh first" >&2
  exit 1
fi

mkdir -p "$UNIT_DIR"
sed -e "s|__PALMTOPD_BIN__|$BIN|g" -e "s|__PALMTOP_REPO__|$REPO_ROOT|g" \
  "$REPO_ROOT/systemd/palmtopd.service" > "$UNIT_DIR/$UNIT_NAME"

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
