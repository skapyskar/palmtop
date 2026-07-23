#!/usr/bin/env bash
# Pick which hardware encoder Palmtop streams through.
#
#   ./choose-encoder.sh          # show the menu and choose
#   ./choose-encoder.sh --list   # just show what works, change nothing
#
# ## Why this is a choice and not just auto-detection
#
# palmtopd can already find *a* working encoder by itself, and does. But it
# picks the first one that works, and "first that works" is not "best". On a
# hybrid-GPU laptop the iGPU's VA-API and the dGPU's NVENC both work, and
# they differ in latency, power draw, fan noise, and picture quality --
# differences no probe can rank, because the right trade-off depends on
# whether you are on battery, whether the dGPU is already busy, and what you
# actually notice. Trying the other one takes ten seconds; this makes that
# ten seconds easy.
#
# The probing and the config edit both live in palmtopd itself
# (--list-encoders / --set-encoder), so this script is only the menu. That
# split is deliberate: hand-editing TOML from shell is exactly how the
# pairing section once ended up with duplicate keys that crash-looped the
# daemon.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Same dual-layout detection as install.sh: a release tarball has palmtopd
# sitting right beside this script; a git checkout has it in target/release
# (or on PATH after an install).
if [ -x "$SCRIPT_DIR/palmtopd" ]; then
  PALMTOPD="$SCRIPT_DIR/palmtopd"
elif [ -x "$SCRIPT_DIR/../target/release/palmtopd" ]; then
  PALMTOPD="$SCRIPT_DIR/../target/release/palmtopd"
elif command -v palmtopd >/dev/null; then
  PALMTOPD="$(command -v palmtopd)"
else
  echo "error: palmtopd not found (looked beside this script, in target/release, and on PATH)" >&2
  exit 1
fi

if [ "${1:-}" = "--list" ]; then
  exec "$PALMTOPD" --list-encoders
fi

"$PALMTOPD" --list-encoders

# Offer only what actually works here, plus auto. Parsed from the same
# --list-encoders output shown above, so the menu can never offer something
# the probe just reported as broken.
#
# Extracted with an explicit sed capture rather than `awk '{print $N}'`:
# the "[  ok  ]" marker is itself whitespace-separated, so awk sees it as
# three fields and the column index silently shifts. That produced a menu
# listing "]" three times -- wrong in a way that still looked like a menu.
mapfile -t OPTIONS < <("$PALMTOPD" --list-encoders 2>/dev/null \
  | sed -n 's/^\[  ok  \][[:space:]]*\([A-Za-z0-9_]\{1,\}\).*/\1/p')
OPTIONS+=("auto")

if [ "${#OPTIONS[@]}" -le 1 ]; then
  echo "Nothing to choose between -- no encoder works on this machine yet." >&2
  echo "Run '$PALMTOPD --doctor' for the specific cause." >&2
  exit 1
fi

echo "Choose one:"
for i in "${!OPTIONS[@]}"; do
  printf '  %d) %s\n' "$((i + 1))" "${OPTIONS[$i]}"
done
printf '  q) quit without changing anything\n'
echo

printf 'Which? [1-%d/q] ' "${#OPTIONS[@]}"
read -r reply

case "$reply" in
  q|Q|"") echo "Nothing was changed."; exit 0 ;;
esac

if ! [[ "$reply" =~ ^[0-9]+$ ]] || [ "$reply" -lt 1 ] || [ "$reply" -gt "${#OPTIONS[@]}" ]; then
  echo "error: '$reply' is not one of the options above." >&2
  exit 1
fi

CHOICE="${OPTIONS[$((reply - 1))]}"
"$PALMTOPD" --set-encoder "$CHOICE"

# Restarting is the whole point of the change taking effect, so do it rather
# than leaving the user with a config that does not match what is running.
if command -v systemctl >/dev/null && systemctl --user list-unit-files palmtopd.service >/dev/null 2>&1; then
  echo
  printf 'Restart palmtopd now so it takes effect? [Y/n] '
  read -r restart
  case "$restart" in
    n|N) echo "Not restarted -- run 'systemctl --user restart palmtopd' when ready." ;;
    *)   systemctl --user restart palmtopd && echo "Restarted. Reconnect the phone and see how it feels." ;;
  esac
fi
