#!/usr/bin/env bash
set -euo pipefail

# init-network.sh
#
# First-boot network initialization for NG Gateway AP hotspot.
#
# This script is called by postinstall.sh on first install. It:
# 1. Detects wireless hardware (via `iw dev` / `iw phy`)
# 2. Generates default AP configuration files (ap-env, hostapd.conf, dnsmasq-ap.conf)
# 3. Deploys systemd unit files and enables/starts AP services
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

# Find the primary Wi-Fi interface (first wlan* or wl* interface).
find_wifi_interface() {
  for iface in /sys/class/net/wl*; do
    [[ -d "$iface" ]] || continue
    basename "$iface"
    return 0
  done
  return 1
}

# Get the phy name for a wireless interface.
get_phy_name() {
  local iface="$1"
  local phy_path="/sys/class/net/${iface}/phy80211/name"
  [[ -f "$phy_path" ]] && cat "$phy_path" | tr -d '\n'
}

# Check if a phy supports AP mode.
phy_supports_ap() {
  local phy="$1"
  iw phy "$phy" info 2>/dev/null | grep -qE "^\s+\* AP$"
}

# Check if a phy supports STA+AP concurrency.
phy_supports_sta_ap() {
  local phy="$1"
  local info
  info=$(iw phy "$phy" info 2>/dev/null)
  # Look for combinations containing both "managed" and "AP".
  echo "$info" | awk '/valid interface combinations:/,/^[^ \t]/' | grep -q "managed" &&
  echo "$info" | awk '/valid interface combinations:/,/^[^ \t]/' | grep -q "AP"
}

# Get the last 4 hex digits of a MAC address.
mac_suffix() {
  local iface="$1"
  local mac
  mac=$(cat "/sys/class/net/${iface}/address" 2>/dev/null || echo "00:00:00:00:00:00")
  echo "$mac" | tr -d ':' | tail -c 5 | tr '[:lower:]' '[:upper:]'
}

# ─── Main Logic ───

main() {
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

  # 3. Determine AP interface name.
  local ap_iface
  if phy_supports_sta_ap "$phy_name"; then
    ap_iface="${wifi_iface}_ap"
    log "STA+AP concurrent supported — will use virtual interface: ${ap_iface}"
  else
    # No concurrency: use the primary interface for AP (no STA).
    ap_iface="${wifi_iface}"
    log "STA+AP NOT supported — AP will use primary interface: ${ap_iface}"
  fi

  # 4. Generate SSID from template.
  local mac4
  mac4=$(mac_suffix "$wifi_iface")
  local ssid="${DEFAULT_AP_SSID_TEMPLATE/\{MAC4\}/$mac4}"
  log "AP SSID: ${ssid}"

  # 5. Generate configuration files.
  mkdir -p "${CONFIG_DIR}"

  # ap-env
  if [[ ! -f "${CONFIG_DIR}/ap-env" ]] || [[ "${FORCE_REGENERATE:-}" == "1" ]]; then
    log "Generating ${CONFIG_DIR}/ap-env"
    cat > "${CONFIG_DIR}/ap-env" <<APENV
# Auto-generated by init-network.sh — editable, but changes may be overwritten by Web UI.
AP_IFACE="${ap_iface}"
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
hw_mode=g
channel=${DEFAULT_AP_CHANNEL}
wmm_enabled=0
macaddr_acl=0
auth_algs=1
ignore_broadcast_ssid=0
wpa=2
wpa_passphrase=${DEFAULT_AP_PASSWORD}
wpa_key_mgmt=WPA-PSK
wpa_pairwise=TKIP
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
interface=${ap_iface}
bind-interfaces
dhcp-range=${DEFAULT_AP_DHCP_START},${DEFAULT_AP_DHCP_END},${DEFAULT_AP_DHCP_LEASE}
dhcp-option=6,${DEFAULT_AP_IP}
no-resolv
server=8.8.8.8
server=1.1.1.1
DNSMASQ
  else
    log "${CONFIG_DIR}/dnsmasq-ap.conf already exists, skipping"
  fi

  # 6. Ensure hostapd and dnsmasq packages are installed.
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
  # iptables for NAT rules
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

  # Disable system-wide dnsmasq/hostapd if enabled (we use our own units).
  systemctl disable --now dnsmasq.service 2>/dev/null || true
  systemctl disable --now hostapd.service 2>/dev/null || true

  # 7. Deploy systemd unit files (if not already present).
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

  # 8. Enable and start AP services.
  log "Enabling and starting AP services..."
  systemctl enable ng-gateway-ap-setup.service 2>/dev/null || true
  systemctl enable ng-gateway-hostapd.service 2>/dev/null || true
  systemctl enable ng-gateway-dnsmasq.service 2>/dev/null || true

  # Start in order.
  systemctl start ng-gateway-ap-setup.service 2>/dev/null || {
    log "WARN: ap-setup failed (virtual interface may not be available). Continuing..."
  }
  systemctl start ng-gateway-hostapd.service 2>/dev/null || {
    log "WARN: hostapd failed to start. Check: journalctl -u ng-gateway-hostapd -n 20"
  }
  systemctl start ng-gateway-dnsmasq.service 2>/dev/null || {
    log "WARN: dnsmasq failed to start. Check: journalctl -u ng-gateway-dnsmasq -n 20"
  }

  # 9. Verify.
  log ""
  log "AP Service Status:"
  log "  ap-setup:  $(systemctl is-active ng-gateway-ap-setup.service 2>/dev/null || echo 'unknown')"
  log "  hostapd:   $(systemctl is-active ng-gateway-hostapd.service 2>/dev/null || echo 'unknown')"
  log "  dnsmasq:   $(systemctl is-active ng-gateway-dnsmasq.service 2>/dev/null || echo 'unknown')"
  log ""
  log "Network initialization complete."
  log "AP SSID: ${ssid} | Password: ${DEFAULT_AP_PASSWORD} | IP: ${DEFAULT_AP_IP}"
}

main "$@"
