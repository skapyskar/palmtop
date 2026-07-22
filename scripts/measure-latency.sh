#!/usr/bin/env bash
# Measure streaming latency across all four quality modes.
#
#   ./scripts/measure-latency.sh --content "static terminal"
#   ./scripts/measure-latency.sh --content "1080p video playback" --duration 45
#   ./scripts/measure-latency.sh --content "static terminal" --modes sync,balanced
#
# Why --content is required rather than optional: video latency depends heavily
# on what is on screen. A static terminal and a playing video are different
# workloads, and comparing a run of one against a run of the other tells you
# nothing. Making it mandatory puts that fact in front of you before the run
# instead of after, and it gets recorded in the output so two result tables can
# be checked for comparability later.
#
# What the numbers mean, precisely:
#   e2e     host capture -> frame released to the surface. Excludes the panel's
#           own response time, which no software on the device can measure
#           (unchanged since Phase 0). It is also derived through a clock offset
#           that assumes symmetric network delay, so treat it as an estimate
#           carrying a few ms of uncertainty -- not a measurement.
#   rtt     Ping/Pong round trip, with host processing time subtracted out.
#   decode  queued to MediaCodec -> output buffer available.
#   drop    frames skipped for exceeding the mode's staleness budget.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$REPO_ROOT/scripts/device.sh"

PKG="dev.palmtop.spike"
DURATION=30
CONTENT=""
MODES="sync,balanced,quality,battery"

while [ $# -gt 0 ]; do
  case "$1" in
    --duration) DURATION="$2"; shift 2 ;;
    --content)  CONTENT="$2"; shift 2 ;;
    --modes)    MODES="$2"; shift 2 ;;
    -h|--help)  sed -n '2,26p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

if [ -z "$CONTENT" ]; then
  echo "error: --content \"<description>\" is required." >&2
  echo "       Latency depends on what is on screen; runs are only comparable" >&2
  echo "       when the content matches, so it has to be recorded." >&2
  exit 1
fi

mode_index() {
  case "$1" in
    sync) echo 0 ;; balanced) echo 1 ;; quality) echo 2 ;; battery) echo 3 ;;
    *) echo "unknown mode: $1" >&2; exit 1 ;;
  esac
}

if ! adb devices | grep -q "device$"; then
  echo "error: no adb device. Enable wireless debugging and run:" >&2
  echo "         adb connect <phone-ip>:5555" >&2
  exit 1
fi

echo "content : $CONTENT"
echo "device  : ${DEVICE_NAME:-unknown}"
echo "host    : ${HOST_IP:-unknown}:${HOST_PORT:-unknown}"
echo "duration: ${DURATION}s per mode"
echo "date    : $(date -Iseconds)"
echo
printf '%-10s %9s %9s %9s %9s %8s %s\n' MODE e2e_p50 e2e_p95 rtt_p50 decode_p50 drop% FORMAT
printf '%-10s %9s %9s %9s %9s %8s %s\n' ---- ------- ------- ------- ---------- ----- ------

for mode_name in ${MODES//,/ }; do
  idx="$(mode_index "$mode_name")"

  adb logcat -c
  # Force-stop first. MainActivity has the default "standard" launch mode, so
  # `am start` on a running instance stacks a second one rather than
  # re-running onCreate -- the --ei mode extra would be silently ignored and
  # every row in the table would report the same preset. Stopping also gives
  # each mode a fresh connection and fresh counters, so the drop percentage
  # is per-mode rather than cumulative across the whole run.
  adb shell am force-stop "$PKG" >/dev/null 2>&1
  sleep 1
  adb shell am start -n "$PKG/.MainActivity" --ei mode "$idx" >/dev/null 2>&1

  # Discard the settling period: the portal handshake, the clock-offset window
  # filling, and the decoder starting up all happen here, and none of it
  # represents the steady state this is trying to characterise.
  sleep 12
  adb logcat -c
  sleep "$DURATION"

  # The client emits one machine-readable `stats key=value ...` line per 30
  # frames -- parsing that beats screen-scraping the on-device HUD.
  line="$(adb logcat -d -s PalmtopClient 2>/dev/null | grep -F 'stats mode=' | grep -F 'valid=true' | tail -1 || true)"

  if [ -z "$line" ]; then
    printf '%-10s %9s %9s %9s %9s %8s %s\n' "$mode_name" - - - - - "no valid samples"
    continue
  fi

  get() { echo "$line" | grep -oE "$1=[0-9.]+" | cut -d= -f2; }
  e2e50=$(( $(get e2e_p50_us) / 1000 ))
  e2e95=$(( $(get e2e_p95_us) / 1000 ))
  rtt50=$(( $(get rtt_p50_us) / 1000 ))
  dec50=$(( $(get decode_p50_us) / 1000 ))
  drop=$(get drop_pct)
  fmt="$(get w)x$(get h)@$(get fps)"

  printf '%-10s %8sms %8sms %8sms %8sms %7s%% %s\n' \
    "$mode_name" "$e2e50" "$e2e95" "$rtt50" "$dec50" "$drop" "$fmt"
done

echo
echo "e2e excludes panel response time (needs external hardware to measure) and"
echo "carries a few ms of uncertainty from the clock-offset symmetry assumption."
