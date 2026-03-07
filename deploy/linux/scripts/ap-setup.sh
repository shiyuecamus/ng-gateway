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

LOG_TAG="[ap-setup]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_common.sh"

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

  # Verify the virtual interface can actually be brought up.
  # Some drivers (Realtek RTL8852BE / rtw89) allow creation but refuse
  # activation with EBUSY.  Fall back to exclusive mode if bring-up fails.
  if ! ip link set "${AP_IFACE}" up 2>/dev/null; then
    warn "Virtual AP interface ${AP_IFACE} created but bring-up failed (EBUSY) — falling back to exclusive mode"
    iw dev "${AP_IFACE}" del 2>/dev/null || true
    AP_IFACE="${STA_IFACE}"
    AP_EXCLUSIVE="true"
    setup_ap_interface_exclusive "${AP_IFACE}" "${AP_IP}" "${AP_PREFIX}"
    log "Exclusive fallback: released ${AP_IFACE} from NetworkManager"
  else
    ip addr flush dev "${AP_IFACE}"
    ip addr add "${AP_IP}/${AP_PREFIX}" dev "${AP_IFACE}"
  fi
else
  setup_ap_interface_exclusive "${AP_IFACE}" "${AP_IP}" "${AP_PREFIX}"
  log "Exclusive: released ${AP_IFACE} from NetworkManager"
fi

# ─── Step 2: NAT / IP Forwarding ───

setup_nat_rules "${AP_IFACE}" "${UPLINK_IFACE}"

log "AP interface ${AP_IFACE} is up at ${AP_IP}/${AP_PREFIX}"
