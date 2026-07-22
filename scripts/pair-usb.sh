#!/usr/bin/env bash
# Pair a phone with this laptop over a USB cable.
#
#   ./scripts/pair-usb.sh
#
# Detects the phone, installs the app if it is missing, and hands it this
# laptop's address and pairing secret. After this the cable is optional --
# the phone remembers the laptop and reconnects over Wi-Fi or the phone's
# hotspot from its Devices list.
#
# ## Why the laptop runs this and not the phone
# USB debugging is a protocol by which a computer inspects a device, never
# the reverse. An app on the phone cannot enumerate ADB connections, cannot
# talk to the laptop across the cable, and cannot detect that a laptop has
# detected it. So the detecting necessarily happens here. The app's part is
# to show whether its own preconditions are met and to react when this script
# pushes the credentials across.
#
# ## Why pair over the cable at all
# The pairing secret and the host's public key travel over a physical wire
# rather than over the network. That is genuinely out-of-band: an attacker on
# the same Wi-Fi cannot see or interfere with it. The QR path is comparably
# safe (the key is read optically), but the mDNS "find on this network" path
# is not -- there the key is broadcast, and a hostile peer could answer
# first. USB pairing closes that gap for anyone who wants it closed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG="dev.palmtop.client"

# Same dual-layout detection as install.sh: a release tarball has this script
# sitting flat next to a downloadable .apk (if the user placed one there) and
# no repo around it; a git checkout has a debug build under android/.
if [ -x "$SCRIPT_DIR/palmtopd" ]; then
  CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/palmtop"
  APK="$(ls "$SCRIPT_DIR"/*.apk 2>/dev/null | head -1 || true)"
else
  REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
  CONFIG_DIR="$REPO_ROOT/config"
  APK="$REPO_ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
  [ -f "$APK" ] || APK=""
fi

source "$SCRIPT_DIR/host-config.sh"

say() { printf '%s\n' "$*"; }
fail() { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- 1. find the phone -------------------------------------------------------
# A USB serial never contains a colon; a TCP one is always "ip:port". Pairing
# has to happen over the cable specifically, so a device already connected
# wirelessly is not a substitute here.
usb_serial="$(adb devices | awk '/\tdevice$/ {print $1}' | grep -v ':' | head -1 || true)"

if [ -z "$usb_serial" ]; then
  unauthorized="$(adb devices | awk '/\tunauthorized$/ {print $1}' | head -1 || true)"
  if [ -n "$unauthorized" ]; then
    fail "phone is connected but has not authorised this computer.
       Unlock the phone -- there should be an 'Allow USB debugging?' prompt.
       Tick 'Always allow from this computer', then re-run this."
  fi
  fail "no phone found over USB.
       On the phone: Settings -> About phone -> tap 'Build number' 7 times,
       then Settings -> Developer options -> enable 'USB debugging',
       then plug in the cable and accept the prompt."
fi

model="$(adb -s "$usb_serial" shell getprop ro.product.model 2>/dev/null | tr -d '\r')"
say "Detected: ${model:-unknown device} ($usb_serial)"

# --- 2. make sure the app is installed ---------------------------------------
if adb -s "$usb_serial" shell pm list packages 2>/dev/null | grep -q "^package:$PKG$"; then
  say "App already installed."
elif [ -n "$APK" ]; then
  say "Installing $APK ..."
  adb -s "$usb_serial" install -r "$APK" >/dev/null || fail "install failed"
  say "Installed."
else
  fail "app is not installed on the phone, and no .apk was found next to this script.
       Download the release .apk and either:
         - place it in this directory and re-run this script, or
         - install it on the phone manually, then re-run this script to pair."
fi

# --- 3. check the daemon is actually running ---------------------------------
[ -n "${PAIRING_TOKEN:-}" ] || fail "no pairing token in $CONFIG_DIR/host.toml.
       Start palmtopd once so it can generate one -- run ./install.sh (or
       ./scripts/install-service.sh in a repo checkout)."
[ -n "${PAIRING_PUBKEY:-}" ] || fail "no host key in $CONFIG_DIR/host.toml.
       Start palmtopd once so it can generate one -- run ./install.sh (or
       ./scripts/install-service.sh in a repo checkout)."

if ! (echo > "/dev/tcp/${HOST_IP}/${HOST_PORT}") 2>/dev/null; then
  say "warning: nothing is listening on ${HOST_IP}:${HOST_PORT} yet."
  say "         Pairing will still be saved, but start the daemon before connecting."
fi

# --- 4. hand over the credentials --------------------------------------------
# Sent as Intent extras, which is the one channel that reaches the app
# directly over the cable without it needing any special permission.
hostname_label="$(hostname 2>/dev/null || echo 'This laptop')"

say "Pairing with ${HOST_IP}:${HOST_PORT} ..."
adb -s "$usb_serial" shell am force-stop "$PKG" >/dev/null 2>&1 || true
adb -s "$usb_serial" shell am start -n "$PKG/.MainActivity" \
  --es host "$HOST_IP" \
  --ei port "$HOST_PORT" \
  --es token "$PAIRING_TOKEN" \
  --es pubkey "$PAIRING_PUBKEY" \
  --es name "$hostname_label" >/dev/null 2>&1 \
  || fail "could not launch the app on the phone"

say ""
say "Paired. '$hostname_label' is now saved in the phone's Devices list."
say ""
say "You can unplug the cable. To reconnect later, open the app and tap"
say "'$hostname_label' -- over Wi-Fi, or over the phone's own hotspot if the"
say "laptop is joined to it."
