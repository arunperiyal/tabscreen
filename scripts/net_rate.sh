#!/bin/sh
# Average network rate of one interface over a period, from /proc/net/dev.
# Usage: scripts/net_rate.sh IFACE [SECONDS]   (default 10 s)
# Prints Mbit/s received and sent by the host. Use it to compare the bitrate of
# TabScreen and AnyDesk in the same scene; nothing needs installing.
set -eu
iface=${1:?usage: net_rate.sh IFACE [SECONDS]}
secs=${2:-10}
bytes() { awk -v i="$iface:" '$1 == i { print $2, $10 }' /proc/net/dev; }
set -- $(bytes)
[ -n "${1:-}" ] || { echo "no interface $iface in /proc/net/dev" >&2; exit 1; }
rx0=$1 tx0=$2
sleep "$secs"
set -- $(bytes)
awk -v rx="$(($1 - rx0))" -v tx="$(($2 - tx0))" -v s="$secs" \
  'BEGIN { printf "rx %.2f Mbit/s, tx %.2f Mbit/s over %d s\n", rx * 8 / s / 1e6, tx * 8 / s / 1e6, s }'
