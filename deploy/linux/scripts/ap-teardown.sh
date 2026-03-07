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

LOG_TAG="[ap-teardown]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_common.sh"

# ─── Remove NAT rules ───

remove_nat_rules "${AP_IFACE}" "${UPLINK_IFACE}"

# ─── Tear down AP interface ───

ip link set "${AP_IFACE}" down 2>/dev/null || true

if [ "${AP_EXCLUSIVE}" != "true" ]; then
  iw dev "${AP_IFACE}" del 2>/dev/null || true
fi

# ─── Restore STA interface (exclusive mode) ───
#
# In exclusive mode, hostapd used the primary interface in AP mode. We must
# restore it to managed (STA) mode and hand it back to NetworkManager.
#
# This script only handles the low-level interface restoration. The actual
# Wi-Fi reconnection is orchestrated by the Rust gateway process via D-Bus
# ActivateConnection — this avoids race conditions between shell-level
# `nmcli device connect` and Rust-level NM activation.
if [ "${AP_EXCLUSIVE}" = "true" ]; then
  log "Exclusive mode — restoring STA (managed) mode on ${AP_IFACE}"
  iw dev "${AP_IFACE}" set type managed 2>/dev/null || true

  # Flush all IP addresses assigned during AP mode (e.g. 10.47.0.1/24).
  # Without this, the AP static IP lingers as a secondary address after NM
  # re-activates the STA connection via DHCP, causing the backend to report
  # the stale AP IP instead of the real DHCP-assigned address.
  ip addr flush dev "${AP_IFACE}" 2>/dev/null || true

  ip link set "${AP_IFACE}" up 2>/dev/null || true

  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${AP_IFACE}" managed yes 2>/dev/null || true
  fi
fi

log "AP interface ${AP_IFACE} torn down, NAT rules removed"
