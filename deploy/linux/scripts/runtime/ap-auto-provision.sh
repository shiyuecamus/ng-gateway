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
if [[ -f "${SCRIPT_DIR}/_common.sh" ]]; then
  source "${SCRIPT_DIR}/_common.sh"
else
  source "${SCRIPT_DIR}/../shared/_common.sh"
fi

NM_AUTOCONNECT_TIMEOUT_SEC="${NM_AUTOCONNECT_TIMEOUT_SEC:-20}"
NM_AUTOCONNECT_POLL_SEC="${NM_AUTOCONNECT_POLL_SEC:-1}"

resolve_sta_iface() {
  local iface="${STA_IFACE:-}"

  if [[ -n "${iface}" ]] && is_wireless_iface "${iface}"; then
    printf "%s\n" "${iface}"
    return 0
  fi

  [[ -n "${iface}" ]] && warn "Configured STA_IFACE=${iface} is not present — probing runtime interface"
  find_managed_wifi_iface
}

wait_for_wifi_connection() {
  local iface="$1"
  local timeout_sec="$2"
  local poll_sec="$3"
  local attempts=1
  local attempt=1
  local state=""
  local last_state="__unset__"

  command -v nmcli >/dev/null 2>&1 || return 1
  (( poll_sec > 0 )) || poll_sec=1
  attempts=$(( (timeout_sec + poll_sec - 1) / poll_sec ))
  (( attempts > 0 )) || attempts=1

  for ((attempt = 1; attempt <= attempts; attempt++)); do
    state=$(nm_device_state "${iface}" 2>/dev/null || true)

    if [[ -z "${state}" ]]; then
      if [[ "${last_state}" != "__missing__" ]]; then
        log "NetworkManager does not yet report a state for ${iface}"
        last_state="__missing__"
      fi
    else
      if [[ "${state}" != "${last_state}" ]]; then
        log "NetworkManager state for ${iface}: ${state} (attempt ${attempt}/${attempts})"
        last_state="${state}"
      fi
      if nm_state_is_connected "${state}"; then
        return 0
      fi
    fi

    (( attempt < attempts )) && sleep "${poll_sec}"
  done

  return 1
}

# ─── Gate: only run in exclusive mode ───

if [ "${AP_EXCLUSIVE:-}" != "true" ]; then
  log "Not in exclusive mode — skipping (concurrent mode uses ap-setup.service)"
  exit 0
fi

# ─── Check: WiFi module exists? ───

if ! command -v iw >/dev/null 2>&1; then
  log "iw not found — cannot detect WiFi module, skipping"
  exit 0
fi

wifi_iface=$(resolve_sta_iface || true)
if [ -z "$wifi_iface" ]; then
  log "No WiFi module detected — skipping"
  exit 0
fi
log "Using station interface: ${wifi_iface}"

if [[ "${AP_IFACE:-}" != "${wifi_iface}" ]]; then
  log "Aligning AP interface with runtime station interface in exclusive mode: ${AP_IFACE:-unset} -> ${wifi_iface}"
  AP_IFACE="${wifi_iface}"
fi

# ─── Safety: ensure interface is in managed mode ───
#
# If a previous AP start failed and left the interface in __ap mode,
# NM cannot manage it and WiFi autoconnect will never happen.
# Restore to managed mode before checking WiFi state.

iface_type=$(wifi_iface_type "$wifi_iface")
if [ "$iface_type" = "__ap" ] || [ "$iface_type" = "AP" ]; then
  log "Interface ${wifi_iface} stuck in ${iface_type} mode — restoring to managed"
  ip link set "$wifi_iface" down 2>/dev/null || true
  iw dev "$wifi_iface" set type managed 2>/dev/null || true
  ip link set "$wifi_iface" up 2>/dev/null || true
  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "$wifi_iface" managed yes 2>/dev/null || true
  fi
fi

# ─── Check: WiFi already connected via NM? ───

if wait_for_wifi_connection "${wifi_iface}" "${NM_AUTOCONNECT_TIMEOUT_SEC}" "${NM_AUTOCONNECT_POLL_SEC}"; then
  log "WiFi management channel is available on ${wifi_iface} — not starting AP"
  exit 0
fi

# ─── No management channel — start AP ───

log "No active WiFi management channel on ${wifi_iface} — starting AP hotspot"

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
