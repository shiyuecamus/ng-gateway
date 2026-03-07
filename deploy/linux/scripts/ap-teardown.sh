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

# ─── Restore STA connection (exclusive mode) ───
#
# In exclusive mode, hostapd changes the interface to AP mode, breaking any
# existing STA (managed) connection. After teardown, we must bring the
# interface back to managed mode so the system can rejoin the previous
# Wi-Fi network (via NetworkManager, wpa_supplicant, or netplan).
if [ "${AP_EXCLUSIVE}" = "true" ]; then
  echo "[ap-teardown] Exclusive mode — restoring STA (managed) mode on ${AP_IFACE}"
  iw dev "${AP_IFACE}" set type managed 2>/dev/null || true
  ip link set "${AP_IFACE}" up 2>/dev/null || true

  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${AP_IFACE}" managed yes 2>/dev/null || true
    nmcli networking off 2>/dev/null || true
    sleep 1
    nmcli networking on 2>/dev/null || true
  elif command -v wpa_supplicant >/dev/null 2>&1 && [ -f /etc/wpa_supplicant/wpa_supplicant.conf ]; then
    wpa_supplicant -B -i "${AP_IFACE}" -c /etc/wpa_supplicant/wpa_supplicant.conf 2>/dev/null || true
  fi
fi

echo "[ap-teardown] AP interface ${AP_IFACE} torn down, NAT rules removed"
