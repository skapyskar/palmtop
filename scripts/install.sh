#!/usr/bin/env bash
# Install the Palmtop host daemon on this Linux machine.
#
#   ./scripts/install.sh              # build from source and install
#   ./scripts/install.sh --from-release  # use a prebuilt binary already on PATH
#
# Installs palmtopd to ~/.local/bin, registers it as a systemd --user service,
# and prints the QR code to pair a phone with.
#
# Runs entirely as your own user. Nothing here needs root: the daemon captures
# the screen through the desktop portal (which asks your permission the first
# time) and injects input through the compositor's own virtual-input protocol,
# so it never needs elevated privileges. A remote-control tool asking for root
# would be a much larger thing to trust than one that cannot.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/palmtop"

say()  { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

FROM_RELEASE=0
[ "${1:-}" = "--from-release" ] && FROM_RELEASE=1

# --- 1. dependencies ---------------------------------------------------------
# Checked up front and all at once. Discovering a missing dependency only when
# the first frame fails to encode produces a confusing runtime error a long
# way from its cause.
missing=()
command -v ffmpeg  >/dev/null || missing+=("ffmpeg")
command -v systemctl >/dev/null || missing+=("systemd")

if [ "${#missing[@]}" -gt 0 ]; then
  say "Missing required packages: ${missing[*]}"
  say ""
  say "Install them with your package manager, for example:"
  say "  Arch      sudo pacman -S ffmpeg"
  say "  Fedora    sudo dnf install ffmpeg"
  say "  Debian    sudo apt install ffmpeg"
  fail "cannot continue without: ${missing[*]}"
fi

# Wayland is what the capture and input paths are built on. X11 is not
# supported yet, and failing here with a clear message beats starting and
# then being unable to capture anything.
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
  warn "WAYLAND_DISPLAY is not set -- this looks like an X11 session."
  warn "Palmtop currently supports Wayland only (see docs/WALKTHROUGH.md)."
  warn "Continuing, but capture will very likely fail."
fi

# --- 2. get the binary -------------------------------------------------------
mkdir -p "$BIN_DIR"

if [ "$FROM_RELEASE" = "1" ]; then
  command -v palmtopd >/dev/null || fail "--from-release given but palmtopd is not on PATH"
  say "Using palmtopd already on PATH: $(command -v palmtopd)"
else
  command -v cargo >/dev/null || fail "cargo not found. Install Rust from https://rustup.rs,
       or download a release build and run:  ./scripts/install.sh --from-release"
  say "Building palmtopd (this takes a few minutes the first time)..."
  ( cd "$REPO_ROOT" && cargo build --release -p palmtopd ) || fail "build failed"
  install -m755 "$REPO_ROOT/target/release/palmtopd" "$BIN_DIR/palmtopd"
  say "Installed to $BIN_DIR/palmtopd"
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) warn "$BIN_DIR is not on your PATH. Add this to your shell profile:"
     warn "  export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac

# --- 3. config ---------------------------------------------------------------
mkdir -p "$CONFIG_DIR"
if [ ! -f "$REPO_ROOT/config/host.toml" ]; then
  say "Creating config/host.toml from the template..."
  cp "$REPO_ROOT/config/host.example.toml" "$REPO_ROOT/config/host.toml"
  # Empty means auto-detect at runtime, which is what keeps this working
  # across network changes -- a hardcoded address goes stale the moment the
  # machine joins a different network, and presents as a daemon that appears
  # to run fine while no phone can reach it.
  say "Host address left blank so it is detected at runtime."
fi

# --- 4. service --------------------------------------------------------------
say "Registering the systemd --user service..."
"$REPO_ROOT/scripts/install-service.sh" >/dev/null || fail "could not install the service"

sleep 2
if systemctl --user is-active --quiet palmtopd; then
  say "palmtopd is running."
else
  warn "palmtopd did not start. Check:  journalctl --user -u palmtopd -n 40"
fi

# --- 5. pair -----------------------------------------------------------------
say ""
say "================================================================"
say " Host ready. Now pair your phone."
say "================================================================"
say ""
say "  Over USB (most reliable, and the pairing secret never touches"
say "  the network):"
say "     1. On the phone, enable Developer options -> USB debugging"
say "     2. Plug in the cable"
say "     3. ./scripts/pair-usb.sh"
say ""
say "  Or wirelessly, by scanning a QR code:"
say "     1. Install the app on the phone"
say "     2. ./scripts/show-pair-qr.sh"
say "     3. In the app: Devices -> Add by scanning QR"
say ""
say "  Logs:  journalctl --user -u palmtopd -f"
say ""
