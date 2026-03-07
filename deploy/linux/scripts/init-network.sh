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

CONFIG_DIR="/etc/ng-gateway"
OPT_DIR="/opt/ng-gateway"
SYSTEMD_DIR="/lib/systemd/system"

# ─── Defaults (can be overridden via /etc/ng-gateway/gateway.toml in future) ───
DEFAULT_AP_SSID_TEMPLATE="NG-Gateway-{MAC4}"
DEFAULT_AP_PASSWORD="ng-gateway"
DEFAULT_AP_CHANNEL=6
DEFAULT_AP_COUNTRY_CODE="CN"
DEFAULT_AP_IP="10.47.0.1"
DEFAULT_AP_PREFIX=24
DEFAULT_AP_DHCP_START="10.47.0.10"
DEFAULT_AP_DHCP_END="10.47.0.200"
DEFAULT_AP_DHCP_LEASE="12h"

# ─── Helper Functions ───

log() { echo "[init-network] $*"; }

# Detect the available package manager for runtime dependency installation.
detect_package_manager() {
  if command -v apt-get >/dev/null 2>&1; then
    echo "apt"
    return 0
  fi
  if command -v dnf >/dev/null 2>&1; then
    echo "dnf"
    return 0
  fi
  if command -v yum >/dev/null 2>&1; then
    echo "yum"
    return 0
  fi
  if command -v zypper >/dev/null 2>&1; then
    echo "zypper"
    return 0
  fi
  return 1
}

# Best-effort package installation across major Linux distributions.
install_packages() {
  local manager="$1"
  shift
  local packages=("$@")

  [[ ${#packages[@]} -gt 0 ]] || return 0

  case "$manager" in
    apt)
      apt-get update -qq &&
      apt-get install -y -qq "${packages[@]}"
      ;;
    dnf)
      dnf install -y "${packages[@]}"
      ;;
    yum)
      yum install -y "${packages[@]}"
      ;;
    zypper)
      zypper --non-interactive install --no-confirm "${packages[@]}"
      ;;
    *)
      return 1
      ;;
  esac
}

# Find the primary Wi-Fi interface using `iw dev` (reliable across naming schemes).
find_wifi_interface() {
  local iface
  iface=$(iw dev 2>/dev/null | awk '/Interface/{print $2; exit}')
  if [[ -n "$iface" ]]; then
    echo "$iface"
    return 0
  fi
  return 1
}

# Get the phy name for a wireless interface.
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
#      removing it. This is the only reliable way to detect concurrency
#      for drivers that don't advertise it via nl80211.
phy_supports_sta_ap() {
  local phy="$1"
  local sta_iface="${2:-}"
  local info
  info=$(iw phy "$phy" info 2>/dev/null) || return 1

  # Method 1: parse `valid interface combinations`.
  if echo "$info" | grep -q "valid interface combinations:"; then
    local combo_section
    combo_section=$(echo "$info" | awk '/valid interface combinations:/,/^[^ \t]/')
    if echo "$combo_section" | grep -q "managed" &&
       echo "$combo_section" | grep -q "AP"; then
      return 0
    fi
    # Combinations section present but no managed+AP combo — not supported.
    return 1
  fi

  # Method 2: `interface combinations are not supported` or no section at all.
  # Probe by creating a temporary virtual AP interface.
  if [[ -n "$sta_iface" ]]; then
    log "No interface combinations advertised — probing virtual AP support..."
    local probe_iface="${sta_iface}_ap_probe"
    if iw dev "$sta_iface" interface add "$probe_iface" type __ap 2>/dev/null; then
      iw dev "$probe_iface" del 2>/dev/null || true
      log "  Probe succeeded: virtual AP interface creation supported"
      return 0
    else
      log "  Probe failed: virtual AP interface creation not supported"
      return 1
    fi
  fi

  return 1
}

# Get the last 4 hex digits of a MAC address (e.g. "E708").
mac_suffix() {
  local iface="$1"
  local mac
  mac=$(cat "/sys/class/net/${iface}/address" 2>/dev/null || echo "00:00:00:00:00:00")
  echo "$mac" | tr -d ':' | grep -oE '.{4}$' | tr '[:lower:]' '[:upper:]'
}

# Find the interface carrying the default route (uplink for NAT).
find_uplink_interface() {
  ip route show default 2>/dev/null | awk '{print $5; exit}'
}

# Disable and mask conflicting system services.
#
# System-wide dnsmasq.service conflicts with systemd-resolved (port 53) and with
# our ng-gateway-dnsmasq.service. System hostapd.service conflicts with ours.
# We mask them to prevent accidental re-enablement by package upgrades.
sanitize_system_services() {
  log "Sanitizing conflicting system services..."

  if systemctl is-enabled dnsmasq.service 2>/dev/null | grep -q enabled; then
    systemctl disable --now dnsmasq.service 2>/dev/null || true
    systemctl mask dnsmasq.service 2>/dev/null || true
    log "  Disabled and masked system dnsmasq.service"
  fi

  if systemctl is-enabled hostapd.service 2>/dev/null | grep -q enabled; then
    systemctl disable --now hostapd.service 2>/dev/null || true
    systemctl mask hostapd.service 2>/dev/null || true
    log "  Disabled and masked system hostapd.service"
  fi
}

# ─── Main Logic ───

main() {
  # Root check.
  if [[ $EUID -ne 0 ]]; then
    log "ERROR: This script must be run as root"
    exit 1
  fi

  log "Starting network initialization..."

  # 0. Check prerequisites.
  if ! command -v iw >/dev/null 2>&1; then
    local pkg_manager
    pkg_manager=$(detect_package_manager) || {
      log "WARN: 'iw' missing and no supported package manager detected. Skipping AP setup."
      exit 0
    }
    log "WARN: 'iw' not installed. Installing via ${pkg_manager}..."
    install_packages "$pkg_manager" iw >/dev/null 2>&1 || {
      log "ERROR: Failed to install 'iw' with ${pkg_manager}. Skipping AP setup."
      exit 0
    }
  fi

  # 1. Detect Wi-Fi hardware.
  local wifi_iface
  wifi_iface=$(find_wifi_interface) || {
    log "No wireless interface detected. AP hotspot will not be configured."
    log "This is normal for devices without Wi-Fi hardware (pure wired gateways)."
    exit 0
  }
  log "Found wireless interface: ${wifi_iface}"

  local phy_name
  phy_name=$(get_phy_name "$wifi_iface") || {
    log "WARN: Cannot determine phy for ${wifi_iface}. Skipping AP setup."
    exit 0
  }
  log "PHY: ${phy_name}"

  # 2. Check AP mode support.
  if ! phy_supports_ap "$phy_name"; then
    log "WARN: ${phy_name} does not support AP mode. Skipping AP setup."
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

  # 4. Detect uplink interface for NAT.
  local uplink_iface
  uplink_iface=$(find_uplink_interface) || true
  if [[ -z "$uplink_iface" ]]; then
    uplink_iface="$wifi_iface"
    log "WARN: No default route found, using ${wifi_iface} as uplink fallback"
  else
    log "Uplink interface: ${uplink_iface}"
  fi

  # 5. Generate SSID from template.
  local mac4
  mac4=$(mac_suffix "$wifi_iface")
  local ssid="${DEFAULT_AP_SSID_TEMPLATE/\{MAC4\}/$mac4}"
  log "AP SSID: ${ssid}"

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
hw_mode=g
channel=${DEFAULT_AP_CHANNEL}
wmm_enabled=0
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
      log "WARN: Missing AP packages (${missing_pkgs[*]}) and no supported package manager detected."
      log "WARN: Please install required packages manually. AP hotspot may not work."
      pkg_manager=""
    }
    if [[ -n "$pkg_manager" ]]; then
      log "Installing missing packages via ${pkg_manager}: ${missing_pkgs[*]}"
      install_packages "$pkg_manager" "${missing_pkgs[@]}" >/dev/null 2>&1 || {
        log "WARN: Failed to install ${missing_pkgs[*]} via ${pkg_manager}. AP hotspot may not work."
      }
    fi
  fi

  # 8. Sanitize conflicting system services.
  sanitize_system_services

  # 9. Deploy systemd unit files.
  for unit in ng-gateway-ap-setup.service ng-gateway-hostapd.service ng-gateway-dnsmasq.service; do
    local src="${OPT_DIR}/systemd/${unit}"
    local dst="${SYSTEMD_DIR}/${unit}"
    if [[ -f "$src" ]]; then
      cp -f "$src" "$dst"
      log "Deployed ${dst}"
    else
      log "WARN: ${src} not found, skipping"
    fi
  done

  systemctl daemon-reload || true

  # 10. Enable and start AP services.
  #
  # EXCLUSIVE mode: The single wireless interface is shared between STA and AP.
  #   - Do NOT enable (would auto-start on boot and fight NetworkManager).
  #   - Do NOT start (would disconnect the current Wi-Fi / SSH session).
  #   - The user starts/stops AP on demand via the Web UI, which calls
  #     `systemctl start/stop` directly without enabling the units.
  #
  # CONCURRENT mode: A dedicated virtual AP interface coexists with STA.
  #   - Enable all three units so the AP survives reboots.
  #   - Start them immediately.
  if [[ "$ap_exclusive" == "true" ]]; then
    # Ensure the manual AP units are *not* enabled — a previous install or
    # mode change may have enabled them.
    systemctl disable ng-gateway-ap-setup.service 2>/dev/null || true
    systemctl disable ng-gateway-hostapd.service 2>/dev/null || true
    systemctl disable ng-gateway-dnsmasq.service 2>/dev/null || true

    # Enable the boot-time auto-provision service instead.
    # It probes network state on every boot and starts AP only when needed
    # (WiFi module present + no active WiFi connection).
    systemctl enable ng-gateway-ap-auto.service 2>/dev/null || true

    log ""
    log "EXCLUSIVE MODE: AP auto-provision enabled (ng-gateway-ap-auto.service)."
    log "On boot: if WiFi module exists and no WiFi is connected, AP starts automatically."
    log "Manual control: use the NG Gateway Web UI to start/stop the AP hotspot."
  else
    systemctl enable ng-gateway-ap-setup.service 2>/dev/null || true
    systemctl enable ng-gateway-hostapd.service 2>/dev/null || true
    systemctl enable ng-gateway-dnsmasq.service 2>/dev/null || true

    log "Starting AP services..."
    systemctl start ng-gateway-ap-setup.service 2>/dev/null || {
      log "WARN: ap-setup failed (virtual interface may not be available). Continuing..."
    }
    systemctl start ng-gateway-hostapd.service 2>/dev/null || {
      log "WARN: hostapd failed to start. Check: journalctl -u ng-gateway-hostapd -n 20"
    }
    systemctl start ng-gateway-dnsmasq.service 2>/dev/null || {
      log "WARN: dnsmasq failed to start. Check: journalctl -u ng-gateway-dnsmasq -n 20"
    }
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
