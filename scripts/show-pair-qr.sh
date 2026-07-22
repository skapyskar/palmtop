#!/usr/bin/env bash
# Put the pairing QR code on screen, big enough for a phone camera to read.
#
#   ./scripts/show-pair-qr.sh          # fullscreen, close with Esc or q
#   ./scripts/show-pair-qr.sh --path   # just print the file path, open it yourself
#
# Why this exists rather than "look at the terminal QR palmtopd printed":
# the pairing URI carries a 64-hex-character Noise public key, so the code is
# roughly a 57x57 module grid, and the terminal renderer packs two vertical
# modules into one character cell. At a normal terminal font that lands each
# module on a couple of physical pixels, non-square -- unreadable by a camera
# at any sane holding distance. The in-app scanner detected *nothing* against
# it, with no error, because nothing was broken; the decoder just never had
# the detail. Fullscreen vector output fixes that outright.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The daemon writes this on every start (see pairing.rs::write_qr_svg): 0600 on
# a user-private tmpfs, because it embeds the pairing token.
QR_FILE="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/palmtop-pair.svg"

if [ ! -f "$QR_FILE" ]; then
  # Fall back to generating it from the config, so this still works if the
  # daemon isn't running (or is an older build).
  source "$REPO_ROOT/scripts/device.sh"
  if [ -z "${PAIRING_TOKEN:-}" ] || [ -z "${PAIRING_PUBKEY:-}" ]; then
    echo "error: no pairing token/pubkey in config -- start palmtopd once to generate them." >&2
    exit 1
  fi
  command -v qrencode >/dev/null || {
    echo "error: $QR_FILE is missing and qrencode isn't installed to regenerate it." >&2
    echo "       Start palmtopd (it writes the file), or: pacman -S qrencode" >&2
    exit 1
  }
  echo "note: $QR_FILE missing (daemon not running?) -- regenerating from config" >&2
  ( umask 077 && qrencode -o "$QR_FILE" -t SVG -s 12 -m 4 -l M \
      "palmtop://${HOST_IP}:${HOST_PORT}/${PAIRING_TOKEN}?pubkey=${PAIRING_PUBKEY}" )
fi

if [ "${1:-}" = "--path" ]; then
  echo "$QR_FILE"
  exit 0
fi

echo "Showing $QR_FILE fullscreen. Close it with Esc (or q) when you're done."
echo "It contains the pairing token, so don't leave it up on a shared screen."

if command -v swayimg >/dev/null; then
  exec swayimg -F "$QR_FILE"
elif command -v xdg-open >/dev/null; then
  exec xdg-open "$QR_FILE"
else
  echo "No image viewer found. Open this file manually: $QR_FILE" >&2
  exit 1
fi
