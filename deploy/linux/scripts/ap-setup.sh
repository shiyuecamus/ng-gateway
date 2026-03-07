#!/usr/bin/env bash
set -euo pipefail

# ap-setup.sh
#
# Called by ng-gateway-ap-setup.service (oneshot).
# Sets up the AP interface, assigns IP, and configures NAT forwarding.
#
# Environment variables (read from /etc/ng-gateway/ap-env via systemd EnvironmentFile):
#   AP_IFACE       - AP interface name (virtual or primary)
#   STA_IFACE      - Station interface name (primary Wi-Fi)
#   UPLINK_IFACE   - Fallback uplink interface for NAT
#   AP_EXCLUSIVE   - "true" if AP uses the primary interface (no STA+AP concurrency)
#   AP_IP          - Static IP for the AP interface (e.g. 10.47.0.1)
#   AP_PREFIX      - CIDR prefix length (e.g. 24)

# ─── Step 1: Create / prepare AP interface ───

if [ "${AP_EXCLUSIVE}" != "true" ]; then
  # Concurrent mode: create a virtual AP interface on the same phy.
  iw dev "${STA_IFACE}" interface add "${AP_IFACE}" type __ap 2>/dev/null || true

  # Derive a locally-administered MAC to avoid conflicts with STA.
  BASE_MAC=$(cat "/sys/class/net/${STA_IFACE}/address" 2>/dev/null || echo "02:00:00:00:00:00")
  OCTET1=$(echo "$BASE_MAC" | cut -d: -f1)
  LAST_OCTET=$(echo "$BASE_MAC" | cut -d: -f6)
  DERIVED_MAC=$(printf "%02x" $(( 0x${OCTET1} | 0x02 ))):$(echo "$BASE_MAC" | cut -d: -f2-5):$(printf "%02x" $(( 0x${LAST_OCTET} ^ 0x01 )))
  ip link set "${AP_IFACE}" address "${DERIVED_MAC}" 2>/dev/null || true
else
  # Exclusive mode: the primary interface must be taken from NetworkManager
  # before hostapd can use it.  Without this, NM races with hostapd and wins,
  # causing "key not allowed / Failed to set beacon parameters".
  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${AP_IFACE}" managed no 2>/dev/null || true
    nmcli device disconnect "${AP_IFACE}" 2>/dev/null || true
    echo "[ap-setup] Exclusive: released ${AP_IFACE} from NetworkManager"
  fi

  # Bring the interface down, switch to AP type, then back up.
  ip link set "${AP_IFACE}" down 2>/dev/null || true
  iw dev "${AP_IFACE}" set type __ap 2>/dev/null || true
  sleep 0.5
fi

# Bring up the AP interface and assign the static IP.
ip link set "${AP_IFACE}" up
ip addr flush dev "${AP_IFACE}"
ip addr add "${AP_IP}/${AP_PREFIX}" dev "${AP_IFACE}"

# ─── Step 2: NAT / IP Forwarding ───

# In exclusive mode there is no separate uplink — AP clients cannot be NATed
# to the internet (no STA connection exists).  We still set up forwarding
# rules so that NAT works immediately if an ethernet uplink appears.
UPLINK=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
[ -z "$UPLINK" ] && UPLINK="${UPLINK_IFACE}"

sysctl -w net.ipv4.ip_forward=1 > /dev/null

iptables -t nat -C POSTROUTING -o "$UPLINK" -j MASQUERADE 2>/dev/null ||
  iptables -t nat -A POSTROUTING -o "$UPLINK" -j MASQUERADE

iptables -C FORWARD -i "${AP_IFACE}" -o "$UPLINK" -j ACCEPT 2>/dev/null ||
  iptables -A FORWARD -i "${AP_IFACE}" -o "$UPLINK" -j ACCEPT

iptables -C FORWARD -i "$UPLINK" -o "${AP_IFACE}" -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null ||
  iptables -A FORWARD -i "$UPLINK" -o "${AP_IFACE}" -m state --state RELATED,ESTABLISHED -j ACCEPT

echo "[ap-setup] AP interface ${AP_IFACE} is up at ${AP_IP}/${AP_PREFIX}, NAT via ${UPLINK}"
