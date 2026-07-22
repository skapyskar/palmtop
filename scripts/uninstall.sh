#!/usr/bin/env bash
# Remove everything Palmtop installed on this machine.
#
#   ./uninstall.sh                  # ask first, then remove everything
#   ./uninstall.sh --yes            # no prompt
#   ./uninstall.sh --keep-pairing   # keep host.toml, so phones stay paired
#
# What gets removed:
#   - the systemd --user service (stopped, disabled, unit file deleted)
#   - the palmtopd binary in ~/.local/bin
#   - the config directory, which holds the pairing token and the host's
#     private Noise key
#   - the pairing QR written to the runtime directory, which embeds the token
#
# ## Why the pairing secrets go by default
# They are the whole of this machine's identity to every phone that paired
# with it. Leaving them behind after an "uninstall" would mean a later
# reinstall silently resurrects credentials the user believed they had
# deleted -- which is the wrong default for anything holding a private key.
# `--keep-pairing` exists for the upgrade case, where re-pairing every phone
# is a real cost and nobody asked for a key rotation.
#
# Nothing here needs root, because nothing Palmtop installs does.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="${HOME}/.local/bin"
UNIT_DIR="${HOME}/.config/systemd/user"
UNIT_NAME="palmtopd.service"

say()  { [ "${QUIET:-0}" = "1" ] || printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }

ASSUME_YES=0
KEEP_PAIRING=0
QUIET=0
for arg in "$@"; do
  case "$arg" in
    --yes|-y)       ASSUME_YES=1 ;;
    --keep-pairing) KEEP_PAIRING=1 ;;
    --quiet)        QUIET=1 ;;
    *) printf 'error: unknown argument: %s\n' "$arg" >&2; exit 1 ;;
  esac
done

# Same dual-layout detection the other scripts use: a release tarball has
# palmtopd sitting beside this script, a git checkout has config/ one level up.
# Only ever used to locate the *installed* config -- the extracted tarball's
# own files are never touched, since the user is standing in that directory.
if [ -x "$SCRIPT_DIR/palmtopd" ]; then
  CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/palmtop"
else
  CONFIG_DIR="$(cd "$SCRIPT_DIR/.." && pwd)/config"
fi

QR_FILE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/palmtop-pair.svg"
HOST_TOML="$CONFIG_DIR/host.toml"

# --- confirm -----------------------------------------------------------------
if [ "$ASSUME_YES" != "1" ]; then
  say "This will remove Palmtop from this machine:"
  say "  service   $UNIT_DIR/$UNIT_NAME"
  say "  binary    $BIN_DIR/palmtopd"
  if [ "$KEEP_PAIRING" = "1" ]; then
    say "  config    $HOST_TOML  (KEPT -- phones stay paired)"
  else
    say "  config    $HOST_TOML  (including the pairing token and host key)"
    say ""
    say "Every phone paired with this laptop will have to pair again."
  fi
  say ""
  printf 'Continue? [y/N] '
  read -r reply
  case "$reply" in
    [yY]|[yY][eE][sS]) ;;
    *) say "Nothing was changed."; exit 0 ;;
  esac
fi

# --- 1. service --------------------------------------------------------------
# `|| true` throughout: every one of these is a no-op on a machine where the
# service was never installed, and an uninstall that fails because there was
# nothing to remove is useless as the first half of a reinstall.
if command -v systemctl >/dev/null; then
  if systemctl --user list-unit-files "$UNIT_NAME" >/dev/null 2>&1; then
    systemctl --user stop "$UNIT_NAME" >/dev/null 2>&1 || true
    systemctl --user disable "$UNIT_NAME" >/dev/null 2>&1 || true
  fi
  # Clears any lingering failed state, so a reinstall does not inherit the
  # previous install's failure counter.
  systemctl --user reset-failed "$UNIT_NAME" >/dev/null 2>&1 || true
fi

if [ -f "$UNIT_DIR/$UNIT_NAME" ]; then
  rm -f "$UNIT_DIR/$UNIT_NAME"
  say "removed  $UNIT_DIR/$UNIT_NAME"
fi
# The enable symlink normally goes with `disable`, but is removed explicitly in
# case the unit file was deleted by hand first, which leaves it dangling.
rm -f "$UNIT_DIR/default.target.wants/$UNIT_NAME"
command -v systemctl >/dev/null && systemctl --user daemon-reload >/dev/null 2>&1 || true

# --- 2. binary ---------------------------------------------------------------
if [ -f "$BIN_DIR/palmtopd" ]; then
  rm -f "$BIN_DIR/palmtopd"
  say "removed  $BIN_DIR/palmtopd"
fi

# --- 3. secrets --------------------------------------------------------------
# Removed even when --keep-pairing is set: it is a world-readable-by-mistake
# waiting to happen, it is regenerated on every daemon start, and keeping it
# serves nothing.
if [ -f "$QR_FILE" ]; then
  rm -f "$QR_FILE"
  say "removed  $QR_FILE"
fi

if [ "$KEEP_PAIRING" = "1" ]; then
  [ -f "$HOST_TOML" ] && say "kept     $HOST_TOML (phones stay paired)"
else
  if [ -f "$HOST_TOML" ]; then
    rm -f "$HOST_TOML"
    say "removed  $HOST_TOML"
  fi
  # Only if empty, and only ever this exact directory. A user may keep their
  # own files here, and an uninstaller that deletes a directory it did not
  # create is a much worse bug than one that leaves an empty folder behind.
  if [ -d "$CONFIG_DIR" ] && [ -z "$(ls -A "$CONFIG_DIR" 2>/dev/null)" ]; then
    rmdir "$CONFIG_DIR" 2>/dev/null && say "removed  $CONFIG_DIR/"
  fi
fi

say ""
say "Palmtop has been removed from this machine."
if [ "$KEEP_PAIRING" != "1" ]; then
  say "The app on your phone will still list this laptop -- forget it there"
  say "(long-press the entry) or just pair again after reinstalling."
fi
