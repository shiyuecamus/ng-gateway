#!/usr/bin/env bash
set -euo pipefail

# init-network.sh
#
# First-boot network initialization for NG Gateway AP hotspot.
#
# This script is called by postinstall.sh on first install. It:
# 1. Detects wireless hardware (via `iw dev` / `iw phy`)
# 2. Generates default AP configuration files (ap-env, hostapd.conf, dnsmasq-ap.conf)
# 3. Sanitizes conflicting system services (dnsmasq, hostapd)
# 4. Deploys systemd unit files and enables/starts AP services
#
# If no wireless hardware is detected, the script exits silently (graceful degradation).
# The script is idempotent — safe to re-run.
#
# Configuration directory: /etc/ng-gateway/
# Runtime directory:       /var/lib/ng-gateway/
#
# Environment knobs:
#   FORCE_REGENERATE=1            Regenerate AP config files even if they exist.
#   DEFER_SERVICE_ACTIVATION=1    Do not start/restart AP services in this run;
#                                 only deploy/enable them for later boot stages.

LOG_TAG="[init-network]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "${SCRIPT_DIR}/_common.sh" ]]; then
  source "${SCRIPT_DIR}/_common.sh"
else
  source "${SCRIPT_DIR}/../shared/_common.sh"
fi

CONFIG_DIR="/etc/ng-gateway"
OPT_DIR="/opt/ng-gateway"
SYSTEMD_DIR="$(systemd_unit_dir)"

# ─── Defaults (can be overridden via /etc/ng-gateway/gateway.toml in future) ───
DEFAULT_AP_SSID_TEMPLATE="NG-Gateway-{MAC4}"
DEFAULT_AP_PASSWORD="ng-gateway"
DEFAULT_AP_CHANNEL=0          # 0 = auto-select based on hardware (5 GHz preferred)
DEFAULT_AP_COUNTRY_CODE="CN"
DEFAULT_AP_IP="10.47.0.1"
DEFAULT_AP_PREFIX=24
DEFAULT_AP_DHCP_START="10.47.0.10"
DEFAULT_AP_DHCP_END="10.47.0.200"
DEFAULT_AP_DHCP_LEASE="12h"

# ─── Wi-Fi Detection Helpers ───

get_phy_name() {
  local iface="$1"
  local phy_path="/sys/class/net/${iface}/phy80211/name"
  [[ -f "$phy_path" ]] && cat "$phy_path" | tr -d '\n'
}

# Check if a phy supports AP mode.
#
# Looks for "AP" in `Supported interface modes:` section only. We must avoid
# matching "AP" strings elsewhere (e.g. "HE Iftypes: AP", "Supported TX
# frame types: ... AP"). The reliable approach is to parse line-by-line,
# entering the section on "Supported interface modes:" and exiting on
# the next non-"* ..." line.
phy_supports_ap() {
  local phy="$1"
  local info
  info=$(iw phy "$phy" info 2>/dev/null) || return 1

  local in_section=0
  while IFS= read -r line; do
    local trimmed="${line#"${line%%[![:space:]]*}"}"
    if [[ "$trimmed" == "Supported interface modes:"* ]]; then
      in_section=1
      continue
    fi
    if [[ $in_section -eq 1 ]]; then
      if [[ "$trimmed" == "* "* ]]; then
        local mode="${trimmed#\* }"
        [[ "$mode" == "AP" ]] && return 0
      else
        break
      fi
    fi
  done <<< "$info"

  return 1
}

# Check if a phy supports STA+AP concurrency.
#
# Strategy (ordered by reliability):
#   1. Parse `valid interface combinations:` — if both `managed` and `AP`
#      appear in the same combination block, concurrent mode is confirmed.
#   2. If the driver reports `interface combinations are not supported`
#      (common with Realtek RTL8852BE and similar), try to **probe** by
#      actually creating a temporary virtual AP interface and immediately
#      removing it.
phy_supports_sta_ap() {
  local phy="$1"
  local sta_iface="${2:-}"
  local info
  info=$(iw phy "$phy" info 2>/dev/null) || return 1

  if echo "$info" | grep -q "valid interface combinations:"; then
    local combo_section
    combo_section=$(echo "$info" | awk '/valid interface combinations:/,/^[^ \t]/')
    if echo "$combo_section" | grep -q "managed" &&
       echo "$combo_section" | grep -q "AP"; then
      return 0
    fi
    return 1
  fi

  if [[ -n "$sta_iface" ]]; then
    log "No interface combinations advertised — probing virtual AP support..."
    local probe_iface="${sta_iface}_ap_probe"
    if iw dev "$sta_iface" interface add "$probe_iface" type __ap 2>/dev/null; then
      # The interface was created — now verify it can actually be brought up.
      # Some drivers (Realtek RTL8852BE / rtw89) allow creation but refuse
      # activation with EBUSY, making concurrent STA+AP impossible.
      if ip link set "$probe_iface" up 2>/dev/null; then
        ip link set "$probe_iface" down 2>/dev/null || true
        iw dev "$probe_iface" del 2>/dev/null || true
        log "  Probe succeeded: virtual AP create + bring-up confirmed"
        return 0
      else
        iw dev "$probe_iface" del 2>/dev/null || true
        log "  Probe partial: interface created but bring-up failed (EBUSY) — exclusive mode only"
        return 1
      fi
    else
      log "  Probe failed: virtual AP interface creation not supported"
      return 1
    fi
  fi

  return 1
}

mac_suffix() {
  local iface="$1"
  local mac
  mac=$(cat "/sys/class/net/${iface}/address" 2>/dev/null || echo "00:00:00:00:00:00")
  echo "$mac" | tr -d ':' | grep -oE '.{4}$' | tr '[:lower:]' '[:upper:]'
}

# Check if a phy supports 5 GHz band (5150–5850 MHz).
phy_supports_5ghz() {
  local phy="$1"
  iw phy "$phy" info 2>/dev/null | grep -qE '^\s+\*\s+5[1-8][0-9]{2}(\.[0-9]+)?\s+MHz'
}

# Find the best AP-usable channel by querying the kernel's regulatory data.
#
# Strategy:
#   1. Parse `iw phy <phy> info` for channels without DFS/RADAR/no-IR/disabled flags.
#   2. Prefer non-DFS 5 GHz UNII-3 (149-165) for best throughput.
#   3. Fall back to non-DFS 5 GHz UNII-1 (36-48) if kernel says they're usable.
#   4. Fall back to 2.4 GHz channel 6 as universal safe default.
#
# Sets global: BEST_AP_CHANNEL (0 if detection fails → caller uses fallback).
find_best_ap_channel() {
  local phy="$1"
  BEST_AP_CHANNEL=0

  local phy_info
  phy_info=$(iw phy "$phy" info 2>/dev/null) || return

  # Extract usable channels: lines with MHz that don't have DFS/radar/no-IR/disabled.
  # Example line: "  * 5745.0 MHz [149] (30.0 dBm)"
  # DFS line:     "  * 5180.0 MHz [36] (23.0 dBm) (no IR, radar detection)"
  local usable_5g_unii3=""
  local usable_5g_unii1=""
  local usable_2g_ch6=""
  local usable_2g_any=""

  while IFS= read -r line; do
    local trimmed="${line#"${line%%[![:space:]]*}"}"
    [[ "$trimmed" == "* "* ]] || continue
    echo "$trimmed" | grep -qi "MHz" || continue

    # Skip channels with DFS/radar/no-IR/disabled flags.
    local lower
    lower=$(echo "$trimmed" | tr '[:upper:]' '[:lower:]')
    echo "$lower" | grep -qE '(disabled|no ir|radar|passive)' && continue

    # Extract channel number from [N].
    local ch
    ch=$(echo "$trimmed" | grep -oP '\[\K[0-9]+(?=\])') || continue

    if [[ "$ch" -ge 149 && "$ch" -le 165 ]]; then
      [[ -z "$usable_5g_unii3" ]] && usable_5g_unii3="$ch"
    elif [[ "$ch" -ge 36 && "$ch" -le 48 ]]; then
      [[ -z "$usable_5g_unii1" ]] && usable_5g_unii1="$ch"
    elif [[ "$ch" -eq 6 ]]; then
      usable_2g_ch6=6
    elif [[ "$ch" -ge 1 && "$ch" -le 14 ]]; then
      [[ -z "$usable_2g_any" ]] && usable_2g_any="$ch"
    fi
  done <<< "$phy_info"

  # Select best channel in priority order.
  if [[ -n "$usable_5g_unii3" ]]; then
    BEST_AP_CHANNEL="$usable_5g_unii3"
  elif [[ -n "$usable_5g_unii1" ]]; then
    BEST_AP_CHANNEL="$usable_5g_unii1"
  elif [[ -n "$usable_2g_ch6" ]]; then
    BEST_AP_CHANNEL=6
  elif [[ -n "$usable_2g_any" ]]; then
    BEST_AP_CHANNEL="$usable_2g_any"
  fi
}

# Resolve channel and hw_mode based on hardware capabilities.
#   - If channel=0 (auto): query kernel for best non-DFS channel.
#     Prefers 5 GHz UNII-3 (149-165), then UNII-1 (36-48), then 2.4 GHz ch 6.
#     Falls back to ch 6 if kernel query fails or no usable channels found.
#   - Returns via globals: RESOLVED_CHANNEL, RESOLVED_HW_MODE, RESOLVED_BAND_CAPS.
resolve_channel_and_band() {
  local phy="$1"
  local requested_channel="$2"

  if [[ "$requested_channel" -eq 0 ]]; then
    find_best_ap_channel "$phy"
    if [[ "$BEST_AP_CHANNEL" -gt 0 ]]; then
      RESOLVED_CHANNEL="$BEST_AP_CHANNEL"
      log "Auto-selected AP channel ${RESOLVED_CHANNEL} (kernel-verified non-DFS)"
    else
      RESOLVED_CHANNEL=6
      log "No kernel channel data available — using safe default channel 6"
    fi
  else
    RESOLVED_CHANNEL="$requested_channel"
  fi

  if [[ "$RESOLVED_CHANNEL" -ge 32 ]]; then
    RESOLVED_HW_MODE="a"
    # 5 GHz: 802.11n + 802.11ac, WMM required
    RESOLVED_BAND_CAPS="ieee80211n=1\nieee80211ac=1\nwmm_enabled=1"
  else
    RESOLVED_HW_MODE="g"
    # 2.4 GHz: 802.11n, WMM off for IoT compatibility
    RESOLVED_BAND_CAPS="ieee80211n=1\nwmm_enabled=0"
  fi
}

# ─── Main Logic ───

main() {
  require_root

  log "Starting network initialization..."

  local defer_service_activation="${DEFER_SERVICE_ACTIVATION:-0}"

  # 0. Check prerequisites.
  if ! command -v iw >/dev/null 2>&1; then
    local pkg_manager
    pkg_manager=$(detect_package_manager) || {
      warn "'iw' missing and no supported package manager detected. Skipping AP setup."
      exit 0
    }
    log "WARN: 'iw' not installed. Installing via ${pkg_manager}..."
    install_packages "$pkg_manager" iw >/dev/null 2>&1 || {
      warn "Failed to install 'iw' with ${pkg_manager}. Skipping AP setup."
      exit 0
    }
  fi

  # 1. Detect Wi-Fi hardware.
  local wifi_iface
  wifi_iface=$(find_managed_wifi_iface) || {
    log "No wireless interface detected. AP hotspot will not be configured."
    log "This is normal for devices without Wi-Fi hardware (pure wired gateways)."
    exit 0
  }
  log "Found wireless interface: ${wifi_iface}"

  local phy_name
  phy_name=$(get_phy_name "$wifi_iface") || {
    warn "Cannot determine phy for ${wifi_iface}. Skipping AP setup."
    exit 0
  }
  log "PHY: ${phy_name}"

  # 2. Check AP mode support.
  if ! phy_supports_ap "$phy_name"; then
    warn "${phy_name} does not support AP mode. Skipping AP setup."
    exit 0
  fi
  log "AP mode supported on ${phy_name}"

  # 3. Determine AP interface name and mode.
  local ap_iface
  local ap_exclusive
  if phy_supports_sta_ap "$phy_name" "$wifi_iface"; then
    ap_iface="${wifi_iface}_ap"
    ap_exclusive="false"
    log "STA+AP concurrent supported — will use virtual interface: ${ap_iface}"
  else
    ap_iface="${wifi_iface}"
    ap_exclusive="true"
    log "STA+AP NOT supported — exclusive mode, AP uses primary interface: ${ap_iface}"
  fi

  # Concurrent mode uses a virtual AP interface that must stay outside
  # NetworkManager control, otherwise NM can clear the static AP address and
  # break DHCP for hotspot clients. Exclusive mode removes any stale rule.
  configure_nm_ap_unmanaged "${ap_iface}" "${ap_exclusive}"

  # 4. Detect uplink interface for NAT.
  local uplink_iface
  uplink_iface=$(find_uplink_iface) || true
  if [[ -z "$uplink_iface" ]]; then
    uplink_iface="$wifi_iface"
    warn "No default route found, using ${wifi_iface} as uplink fallback"
  else
    log "Uplink interface: ${uplink_iface}"
  fi

  # 5. Generate SSID from template.
  local mac4
  mac4=$(mac_suffix "$wifi_iface")
  local ssid="${DEFAULT_AP_SSID_TEMPLATE/\{MAC4\}/$mac4}"
  log "AP SSID: ${ssid}"

  # 5b. Resolve channel and band based on hardware capabilities.
  resolve_channel_and_band "$phy_name" "$DEFAULT_AP_CHANNEL"
  log "Resolved channel: ${RESOLVED_CHANNEL} (hw_mode=${RESOLVED_HW_MODE})"

  # 6. Generate configuration files.
  mkdir -p "${CONFIG_DIR}"

  # ap-env
  if [[ ! -f "${CONFIG_DIR}/ap-env" ]] || [[ "${FORCE_REGENERATE:-}" == "1" ]]; then
    log "Generating ${CONFIG_DIR}/ap-env"
    cat > "${CONFIG_DIR}/ap-env" <<APENV
# Auto-generated by init-network.sh — editable, but changes may be overwritten by Web UI.
AP_IFACE="${ap_iface}"
STA_IFACE="${wifi_iface}"
UPLINK_IFACE="${uplink_iface}"
AP_EXCLUSIVE="${ap_exclusive}"
AP_IP="${DEFAULT_AP_IP}"
AP_PREFIX="${DEFAULT_AP_PREFIX}"
AP_DHCP_START="${DEFAULT_AP_DHCP_START}"
AP_DHCP_END="${DEFAULT_AP_DHCP_END}"
APENV
  else
    log "${CONFIG_DIR}/ap-env already exists, skipping (use FORCE_REGENERATE=1 to overwrite)"
  fi

  # hostapd.conf
  if [[ ! -f "${CONFIG_DIR}/hostapd.conf" ]] || [[ "${FORCE_REGENERATE:-}" == "1" ]]; then
    log "Generating ${CONFIG_DIR}/hostapd.conf"
    cat > "${CONFIG_DIR}/hostapd.conf" <<HOSTAPD
# Auto-generated by init-network.sh — editable, but changes may be overwritten by Web UI.
interface=${ap_iface}
driver=nl80211
ssid=${ssid}
country_code=${DEFAULT_AP_COUNTRY_CODE}
ieee80211d=1
hw_mode=${RESOLVED_HW_MODE}
channel=${RESOLVED_CHANNEL}
$(echo -e "${RESOLVED_BAND_CAPS}")
macaddr_acl=0
auth_algs=1
ignore_broadcast_ssid=0
wpa=2
wpa_passphrase=${DEFAULT_AP_PASSWORD}
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP

# Control interface for status queries
ctrl_interface=/var/run/hostapd
ctrl_interface_group=0
HOSTAPD
  else
    log "${CONFIG_DIR}/hostapd.conf already exists, skipping"
  fi

  # dnsmasq-ap.conf
  if [[ ! -f "${CONFIG_DIR}/dnsmasq-ap.conf" ]] || [[ "${FORCE_REGENERATE:-}" == "1" ]]; then
    log "Generating ${CONFIG_DIR}/dnsmasq-ap.conf"
    cat > "${CONFIG_DIR}/dnsmasq-ap.conf" <<DNSMASQ
# Auto-generated by init-network.sh — editable, but changes may be overwritten by Web UI.
# DHCP + DNS for AP clients only.
interface=${ap_iface}
bind-dynamic
listen-address=${DEFAULT_AP_IP}
dhcp-range=${DEFAULT_AP_DHCP_START},${DEFAULT_AP_DHCP_END},${DEFAULT_AP_DHCP_LEASE}
dhcp-option=6,${DEFAULT_AP_IP}
no-resolv
server=8.8.8.8
server=1.1.1.1
DNSMASQ
  else
    log "${CONFIG_DIR}/dnsmasq-ap.conf already exists, skipping"
  fi

  # 7. Ensure hostapd and dnsmasq packages are installed.
  local pkg_manager
  local missing_pkgs=()
  command -v hostapd >/dev/null 2>&1 || missing_pkgs+=(hostapd)
  command -v dnsmasq >/dev/null 2>&1 || {
    if command -v apt-get >/dev/null 2>&1; then
      missing_pkgs+=(dnsmasq-base)
    else
      missing_pkgs+=(dnsmasq)
    fi
  }
  command -v iptables >/dev/null 2>&1 || missing_pkgs+=(iptables)

  if [[ ${#missing_pkgs[@]} -gt 0 ]]; then
    pkg_manager=$(detect_package_manager) || {
      warn "Missing AP packages (${missing_pkgs[*]}) and no supported package manager detected."
      warn "Please install required packages manually. AP hotspot may not work."
      pkg_manager=""
    }
    if [[ -n "$pkg_manager" ]]; then
      log "Installing missing packages via ${pkg_manager}: ${missing_pkgs[*]}"
      install_packages "$pkg_manager" "${missing_pkgs[@]}" >/dev/null 2>&1 || {
        warn "Failed to install ${missing_pkgs[*]} via ${pkg_manager}. AP hotspot may not work."
      }
    fi
  fi

  # 8. Sanitize conflicting system services.
  sanitize_conflicting_services

  # 9. Deploy systemd unit files.
  for unit in ng-gateway-ap-setup.service ng-gateway-hostapd.service ng-gateway-dnsmasq.service ng-gateway-ap-auto.service; do
    local src="${OPT_DIR}/systemd/${unit}"
    local dst="${SYSTEMD_DIR}/${unit}"
    if [[ -f "$src" ]]; then
      cp -f "$src" "$dst"
      log "Deployed ${dst}"
    else
      warn "${src} not found, skipping"
    fi
  done

  systemctl daemon-reload || true

  # 10. Enable and optionally start AP services.
  if [[ "$ap_exclusive" == "true" ]]; then
    systemctl disable ng-gateway-ap-setup.service 2>/dev/null || true
    systemctl disable ng-gateway-hostapd.service 2>/dev/null || true
    systemctl disable ng-gateway-dnsmasq.service 2>/dev/null || true

    systemctl enable ng-gateway-ap-auto.service 2>/dev/null || true
    if [[ "${defer_service_activation}" == "1" ]]; then
      log "Deferring AP auto-provision start to later boot stage (DEFER_SERVICE_ACTIVATION=1)"
    else
      log "Running AP auto-provision probe now..."
      systemctl restart ng-gateway-ap-auto.service 2>/dev/null || {
        warn "ap-auto-provision failed. Check: journalctl -u ng-gateway-ap-auto -n 50"
      }
    fi

    log ""
    log "EXCLUSIVE MODE: AP auto-provision enabled (ng-gateway-ap-auto.service)."
    log "On boot: if WiFi module exists and no WiFi is connected, AP starts automatically."
    if [[ "${defer_service_activation}" == "1" ]]; then
      log "Install-time probe: deferred because this run occurs inside an earlier boot stage."
    else
      log "Install-time probe: running once now so AP is available immediately when WiFi is not connected."
    fi
    log "Manual control: use the NG Gateway Web UI to start/stop the AP hotspot."
  else
    systemctl enable ng-gateway-ap-setup.service 2>/dev/null || true
    systemctl enable ng-gateway-hostapd.service 2>/dev/null || true
    systemctl enable ng-gateway-dnsmasq.service 2>/dev/null || true

    if [[ "${defer_service_activation}" == "1" ]]; then
      log "Deferring AP service start to later boot stage (DEFER_SERVICE_ACTIVATION=1)"
    else
      log "Starting AP services..."
      systemctl start ng-gateway-ap-setup.service 2>/dev/null || {
        warn "ap-setup failed (virtual interface may not be available). Continuing..."
      }
      systemctl start ng-gateway-hostapd.service 2>/dev/null || {
        warn "hostapd failed to start. Check: journalctl -u ng-gateway-hostapd -n 20"
      }
      systemctl start ng-gateway-dnsmasq.service 2>/dev/null || {
        warn "dnsmasq failed to start. Check: journalctl -u ng-gateway-dnsmasq -n 20"
      }
    fi
  fi

  # 11. Verify.
  log ""
  log "AP Service Status:"
  local ap_status; ap_status=$(systemctl is-active ng-gateway-ap-setup.service 2>/dev/null) || ap_status="unknown"
  local hp_status; hp_status=$(systemctl is-active ng-gateway-hostapd.service 2>/dev/null) || hp_status="unknown"
  local dn_status; dn_status=$(systemctl is-active ng-gateway-dnsmasq.service 2>/dev/null) || dn_status="unknown"
  log "  ap-setup:  ${ap_status}"
  log "  hostapd:   ${hp_status}"
  log "  dnsmasq:   ${dn_status}"
  log ""
  log "Network initialization complete."
  log "AP SSID: ${ssid} | Password: ${DEFAULT_AP_PASSWORD} | IP: ${DEFAULT_AP_IP}"
  if [[ "$ap_exclusive" == "true" ]]; then
    log "Mode: EXCLUSIVE (start AP via Web UI)"
  else
    log "Mode: CONCURRENT (STA+AP)"
  fi
}

main "$@"
