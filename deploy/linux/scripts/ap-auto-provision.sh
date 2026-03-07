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

log() { echo "[ap-auto] $*"; }

# ─── Gate: only run in exclusive mode ───

if [ "${AP_EXCLUSIVE:-}" != "true" ]; then
  log "Not in exclusive mode — skipping (concurrent mode uses ap-setup.service)"
  exit 0
fi

# ─── Allow NM time to auto-connect to known WiFi networks ───
#
# NetworkManager.service may be "active" but the WiFi supplicant hasn't
# finished scanning/connecting yet.  A short grace period avoids false
# negatives on slow-to-connect networks.
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
  # nmcli -t -f TYPE,STATE device: "wifi:connected" if STA is active.
  wifi_state=$(nmcli -t -f TYPE,STATE device 2>/dev/null | grep '^wifi:' | head -1)
  if echo "$wifi_state" | grep -q "connected"; then
    log "WiFi is connected — not starting AP"
    exit 0
  fi
fi

# ─── No management channel — start AP ───

log "WiFi module present but not connected — starting AP hotspot"

# Release the wireless interface from NetworkManager so hostapd can use it.
if command -v nmcli >/dev/null 2>&1; then
  nmcli device set "${AP_IFACE}" managed no 2>/dev/null || true
  nmcli device disconnect "${AP_IFACE}" 2>/dev/null || true
  log "Released ${AP_IFACE} from NetworkManager"
fi

# Switch the interface to AP mode and assign the static IP.
ip link set "${AP_IFACE}" down 2>/dev/null || true
iw dev "${AP_IFACE}" set type __ap 2>/dev/null || true
sleep 0.5
ip link set "${AP_IFACE}" up
ip addr flush dev "${AP_IFACE}"
ip addr add "${AP_IP}/${AP_PREFIX}" dev "${AP_IFACE}"

# Best-effort NAT setup (may not have an uplink in exclusive mode).
sysctl -w net.ipv4.ip_forward=1 > /dev/null 2>&1 || true
UPLINK=$(ip route show default 2>/dev/null | awk '{print $5; exit}')
if [ -n "$UPLINK" ]; then
  iptables -t nat -C POSTROUTING -o "$UPLINK" -j MASQUERADE 2>/dev/null ||
    iptables -t nat -A POSTROUTING -o "$UPLINK" -j MASQUERADE 2>/dev/null || true
fi

# Start hostapd and dnsmasq (they depend on this unit via Requires=).
systemctl start ng-gateway-hostapd.service 2>/dev/null || {
  log "WARN: hostapd failed to start. Check: journalctl -u ng-gateway-hostapd -n 20"
}
systemctl start ng-gateway-dnsmasq.service 2>/dev/null || {
  log "WARN: dnsmasq failed to start. Check: journalctl -u ng-gateway-dnsmasq -n 20"
}

log "AP hotspot started on ${AP_IFACE} at ${AP_IP}/${AP_PREFIX}"
