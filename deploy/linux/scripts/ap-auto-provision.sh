#!/usr/bin/env bash
set -euo pipefail

# ap-auto-provision.sh
#
# Boot-time AP auto-provision for EXCLUSIVE-mode hardware.
#
# Decision rule:
#   WiFi module exists  +  no active WiFi connection  →  start AP hotspot
#   Otherwise                                         →  do nothing
#
# Called by ng-gateway-ap-auto.service (oneshot, After=NetworkManager.service).
# By the time this script runs, NetworkManager has already had a chance to
# auto-connect to any previously-known Wi-Fi network.
#
# Environment variables (from /etc/ng-gateway/ap-env via systemd EnvironmentFile):
#   AP_IFACE, STA_IFACE, AP_EXCLUSIVE, AP_IP, AP_PREFIX, etc.

LOG_TAG="[ap-auto]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_common.sh"

# ─── Gate: only run in exclusive mode ───

if [ "${AP_EXCLUSIVE:-}" != "true" ]; then
  log "Not in exclusive mode — skipping (concurrent mode uses ap-setup.service)"
  exit 0
fi

# ─── Allow NM time to auto-connect to known WiFi networks ───
sleep 5

# ─── Check: WiFi module exists? ───

if ! command -v iw >/dev/null 2>&1; then
  log "iw not found — cannot detect WiFi module, skipping"
  exit 0
fi

wifi_iface=$(iw dev 2>/dev/null | awk '/Interface/{print $2; exit}')
if [ -z "$wifi_iface" ]; then
  log "No WiFi module detected — skipping"
  exit 0
fi
log "WiFi module detected: ${wifi_iface}"

# ─── Check: WiFi already connected via NM? ───

if command -v nmcli >/dev/null 2>&1; then
  wifi_state=$(nmcli -t -f TYPE,STATE device 2>/dev/null | grep '^wifi:' | head -1)
  if echo "$wifi_state" | grep -q "connected"; then
    log "WiFi is connected — not starting AP"
    exit 0
  fi
fi

# ─── No management channel — start AP ───

log "WiFi module present but not connected — starting AP hotspot"

setup_ap_interface_exclusive "${AP_IFACE}" "${AP_IP}" "${AP_PREFIX}"
setup_nat_rules "${AP_IFACE}" "${UPLINK_IFACE:-}"

# Start hostapd and dnsmasq. If hostapd fails, the interface is already
# released from NM in __ap mode — we must restore it or WiFi is unusable.
if ! systemctl start ng-gateway-hostapd.service 2>/dev/null; then
  warn "hostapd failed to start — rolling back interface to managed mode"
  warn "Check: journalctl -u ng-gateway-hostapd -n 20"
  remove_nat_rules "${AP_IFACE}" "${UPLINK_IFACE:-}"
  ip link set "${AP_IFACE}" down 2>/dev/null || true
  iw dev "${AP_IFACE}" set type managed 2>/dev/null || true
  ip link set "${AP_IFACE}" up 2>/dev/null || true
  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${AP_IFACE}" managed yes 2>/dev/null || true
  fi
  exit 1
fi

systemctl start ng-gateway-dnsmasq.service 2>/dev/null || {
  warn "dnsmasq failed to start. Check: journalctl -u ng-gateway-dnsmasq -n 20"
}

log "AP hotspot started on ${AP_IFACE} at ${AP_IP}/${AP_PREFIX}"
