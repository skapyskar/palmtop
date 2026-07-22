#!/usr/bin/env bash
# Connect adb to the phone, over whichever transport is actually available.
#
#   ./scripts/connect-device.sh                        # try each in turn
#   ./scripts/connect-device.sh usb                    # cable
#   ./scripts/connect-device.sh hotspot                # phone is the Wi-Fi AP
#   ./scripts/connect-device.sh wireless IP            # same router, port 5555
#   ./scripts/connect-device.sh pair IP:PORT CODE      # Android 11+ pairing
#   ./scripts/connect-device.sh wireless-port IP:PORT  # ...then connect
#   ./scripts/connect-device.sh status                 # what's connected now
#
# Several transports rather than one because which is correct changes with how
# the two machines happen to be networked, and getting it wrong looks identical
# to the device being broken -- `adb devices` prints an empty list either way.
#
# ## The three, and when each applies
#
# **usb** -- cable. The only one that works with no network at all, and the one
# to reach for when the others are misbehaving, since it removes Wi-Fi from the
# equation entirely. Also the only way to *enable* the other two: Android needs
# `adb tcpip` issued over USB once before it will listen on TCP.
#
# **hotspot** -- the phone is the access point and the laptop is its client. The
# phone is therefore the default gateway, which is how this script finds it
# without being told. Worth preferring on an unfamiliar network: Phase 0 hit
# real AP client-isolation on an ordinary Wi-Fi network (ARP to the phone stuck
# at FAILED while the gateway resolved fine), and a phone acting as its own AP
# cannot isolate a client from itself.
#
# **wireless** -- both joined to some third router. Convenient, and the setup
# most likely to be silently blocked by client isolation.
#
# Whichever you pick, the *stream* is separate from adb: palmtopd listens on
# 0.0.0.0 and the phone dials the laptop's address on the shared network. This
# script prints that address, because it changes whenever the network does --
# a stale one in config/host.toml is exactly what made an earlier session look
# like a broken daemon when it was only a moved laptop.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ADB_PORT=5555

# device.sh exits if config is missing; we only need it for the profile path
# and the host IP, so tolerate its absence during a first-time setup.
source "$REPO_ROOT/scripts/device.sh" 2>/dev/null || true

DEVICE_CONFIG="$REPO_ROOT/config/devices/${DEVICE_NAME:-unknown}.toml"

usb_serial() {
  # USB serials never contain a colon; TCP ones are always "ip:port".
  adb devices | awk '/\tdevice$/ {print $1}' | grep -v ':' | head -1
}

tcp_serial() {
  adb devices | awk '/\tdevice$/ {print $1}' | grep ':' | head -1
}

gateway_ip() {
  ip route | awk '/^default/ {print $3; exit}'
}

host_ip() {
  ip route get 8.8.8.8 2>/dev/null | grep -oP 'src \K\S+' | head -1
}

port_open() {
  timeout 2 bash -c "echo > /dev/tcp/$1/$2" 2>/dev/null
}

# Persists whatever we ended up connected to, so device.sh and every script
# downstream of it keep working after the network moves.
remember() {
  local serial="$1" ip="$2"
  [ -f "$DEVICE_CONFIG" ] || return 0
  local tmp
  tmp="$(mktemp)"
  awk -v s="$serial" -v i="$ip" '
    /^serial[[:space:]]*=/ && !seen_s { print "serial = \"" s "\""; seen_s=1; next }
    /^ip[[:space:]]*=/     && !seen_i { print "ip     = \"" i "\"";  seen_i=1; next }
    { print }
  ' "$DEVICE_CONFIG" > "$tmp"
  mv "$tmp" "$DEVICE_CONFIG"
  echo "  recorded in $(basename "$DEVICE_CONFIG"): serial=$serial ip=$ip"
}

# Android only listens on TCP after being told to over USB. Doing it here means
# "hotspot" and "wireless" can succeed on the first try with a cable briefly
# attached, instead of failing with an error nobody can act on.
enable_tcp_via_usb() {
  local usb
  usb="$(usb_serial)"
  [ -n "$usb" ] || return 1
  echo "  enabling adb over TCP via the USB connection..."
  adb -s "$usb" tcpip "$ADB_PORT" >/dev/null 2>&1 || return 1
  sleep 2
  return 0
}

connect_tcp() {
  local ip="$1" label="$2"
  if ! port_open "$ip" "$ADB_PORT"; then
    echo "  $ip:$ADB_PORT is not accepting connections."
    if enable_tcp_via_usb; then
      :
    else
      echo
      echo "  adb isn't listening on the phone, and there's no USB cable attached to fix it."
      echo "  One of these, then re-run:"
      echo "    - plug in USB and run: ./scripts/connect-device.sh $label"
      echo "    - or on the phone: Developer options -> Wireless debugging"
      return 1
    fi
  fi
  adb connect "$ip:$ADB_PORT" 2>&1 | sed 's/^/  /'
  adb devices | grep -q "$ip:$ADB_PORT" || return 1
  remember "$ip:$ADB_PORT" "$ip"
  return 0
}

try_usb() {
  local usb
  usb="$(usb_serial)"
  [ -n "$usb" ] || { echo "  no device on USB"; return 1; }
  echo "  connected over USB: $usb"
  # IP left as-is: a USB serial says nothing about the phone's address, and
  # blanking it would lose a still-valid one from a previous run.
  remember "$usb" "${DEVICE_IP:-}"
  return 0
}

try_hotspot() {
  local gw
  gw="$(gateway_ip)"
  if [ -z "$gw" ]; then
    echo "  no default gateway -- not on a network"
    return 1
  fi
  echo "  phone should be the gateway of its own hotspot: $gw"
  connect_tcp "$gw" hotspot
}

try_wireless() {
  local ip="${1:-${DEVICE_IP:-}}"
  if [ -z "$ip" ]; then
    echo "  no IP given and none recorded. Usage: $0 wireless <phone-ip>"
    return 1
  fi
  echo "  trying recorded/supplied address: $ip"
  connect_tcp "$ip" wireless
}

report() {
  echo
  echo "adb devices:"
  adb devices | tail -n +2 | sed '/^$/d;s/^/  /'
  local hip
  hip="$(host_ip)"
  echo
  echo "The phone dials palmtopd at: ${hip:-<unknown>}:${HOST_PORT:-9999}"
  if [ -n "${HOST_IP:-}" ] && [ -n "$hip" ] && [ "$HOST_IP" != "$hip" ]; then
    echo "  warning: config/host.toml resolves to $HOST_IP, which is not this address."
    echo "           Set ip = \"\" in config/host.toml to auto-detect and stop this drifting."
  fi
}

# Android 11+ "Wireless debugging" pairing. Distinct from everything above:
# it needs a one-time pairing on a *random* port with a 6-digit code, after
# which the phone listens on a different, also-random port. That is why plain
# `adb connect <ip>:5555` fails on a phone whose only debugging is this --
# nothing is listening on 5555 at all, and the error says only "connection
# refused", which reads like the phone is off.
try_pair() {
  local pair_hostport="${1:-}" code="${2:-}"
  if [ -z "$pair_hostport" ] || [ -z "$code" ]; then
    cat <<'EOF'
  On the phone: Settings -> Developer options -> Wireless debugging
    1. Turn it on
    2. Tap "Pair device with pairing code"
    3. It shows an IP:PORT and a 6-digit code

  Then run, with the values it showed:
    ./scripts/connect-device.sh pair 192.168.1.42:37123 123456

  The pairing port is not the connection port -- after pairing, the same
  screen's top line shows the IP:PORT to connect to.
EOF
    return 1
  fi
  echo "  pairing with $pair_hostport..."
  adb pair "$pair_hostport" "$code" 2>&1 | sed 's/^/  /'
  echo
  echo "  Paired. Now connect using the IP:PORT from the main"
  echo "  'Wireless debugging' screen (not the pairing one):"
  echo "    ./scripts/connect-device.sh wireless-port <ip>:<port>"
}

# Connects to an explicit ip:port, for the Android 11+ random-port case.
try_wireless_port() {
  local hostport="${1:-}"
  if [ -z "$hostport" ]; then
    echo "  usage: $0 wireless-port <ip>:<port>"
    return 1
  fi
  adb connect "$hostport" 2>&1 | sed 's/^/  /'
  adb devices | grep -q "$hostport" || return 1
  remember "$hostport" "${hostport%%:*}"
  return 0
}

MODE="${1:-auto}"
case "$MODE" in
  usb)      echo "USB:";      try_usb      && report ;;
  hotspot)  echo "Hotspot:";  try_hotspot  && report ;;
  wireless) echo "Wireless:"; try_wireless "${2:-}" && report ;;
  pair)     echo "Wireless debugging (pair):"; try_pair "${2:-}" "${3:-}" ;;
  wireless-port) echo "Wireless debugging (connect):"; try_wireless_port "${2:-}" && report ;;
  status)
    report
    ;;
  auto)
    echo "Trying each transport in turn."
    echo "USB:"
    if try_usb; then report; exit 0; fi
    echo "Hotspot:"
    if try_hotspot; then report; exit 0; fi
    echo "Wireless:"
    if try_wireless; then report; exit 0; fi
    echo
    echo "No transport worked. Plug in a USB cable -- it needs no network and"
    echo "is also how the other two get enabled."
    exit 1
    ;;
  *) echo "usage: $0 [usb|hotspot|wireless <ip>|status|auto]" >&2; exit 1 ;;
esac
