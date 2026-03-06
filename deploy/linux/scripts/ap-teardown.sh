#!/usr/bin/env bash
set -euo pipefail

# ap-teardown.sh
#
# Called by ng-gateway-ap-setup.service on stop.
# Removes NAT rules and tears down the AP interface.
#
# Environment variables (read from /etc/ng-gateway/ap-env via systemd EnvironmentFile):
#   AP_IFACE       - AP interface name
#   STA_IFACE      - Station interface name
#   UPLINK_IFACE   - Fallback uplink interface
#   AP_EXCLUSIVE   - "true" if AP uses the primary interface

# ─── Remove NAT rules ───

UPLINK=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
[ -z "$UPLINK" ] && UPLINK="${UPLINK_IFACE}"

iptables -t nat -D POSTROUTING -o "$UPLINK" -j MASQUERADE 2>/dev/null || true
iptables -D FORWARD -i "${AP_IFACE}" -o "$UPLINK" -j ACCEPT 2>/dev/null || true
iptables -D FORWARD -i "$UPLINK" -o "${AP_IFACE}" -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true

# ─── Tear down AP interface ───

ip link set "${AP_IFACE}" down 2>/dev/null || true

if [ "${AP_EXCLUSIVE}" != "true" ]; then
  # Concurrent mode: remove the virtual interface.
  iw dev "${AP_IFACE}" del 2>/dev/null || true
fi

echo "[ap-teardown] AP interface ${AP_IFACE} torn down, NAT rules removed"
