#!/usr/bin/env bash
# _common.sh
#
# Shared utility library for NG Gateway deployment scripts.
# Source this file at the top of any script that needs common helpers.
#
# Runtime scripts example:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "${SCRIPT_DIR}/../shared/_common.sh"
#
# Factory scripts example:
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "${SCRIPT_DIR}/../shared/_common.sh"
#
# The caller MUST set LOG_TAG before sourcing (or it defaults to the script name).
#
# Provided functions:
#   Logging:       log, warn, die
#   Guards:        require_root, require_commands
#   Block device:  parse_block_device, partition_path
#   Network:       find_uplink_iface, find_managed_wifi_iface, nm_device_state,
#                  release_iface_from_nm, configure_nm_ap_unmanaged, setup_ap_interface,
#                  setup_nat_rules, remove_nat_rules
#   Systemd:       sanitize_conflicting_services
#   Package mgmt:  detect_package_manager, install_packages

# Prevent double-sourcing.
[[ -n "${_NG_COMMON_LOADED:-}" ]] && return 0
_NG_COMMON_LOADED=1

LOG_TAG="${LOG_TAG:-[$(basename "${BASH_SOURCE[1]:-$0}")]}"

# ─────────────────────────────────────────────
# Logging
# ─────────────────────────────────────────────

log()  { echo "${LOG_TAG} $*"; }
warn() { echo "${LOG_TAG} WARN: $*" >&2; }
die()  { echo "${LOG_TAG} FATAL: $*" >&2; exit 1; }

# ─────────────────────────────────────────────
# Guards
# ─────────────────────────────────────────────

require_root() {
  [[ $EUID -eq 0 ]] || die "Must run as root"
}

# require_commands cmd1 cmd2 ...
# Exits with a message listing all missing commands.
require_commands() {
  local missing=()
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null 2>&1 || missing+=("$cmd")
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    die "Missing required tool(s): ${missing[*]}"
  fi
}

# ─────────────────────────────────────────────
# Block-device helpers
# ─────────────────────────────────────────────

# parse_block_device <partition-dev>
#
# Splits a partition device path into disk + partition number.
# Sets two global variables: _PBD_DISK and _PBD_PARTNUM.
#
# Example:
#   parse_block_device /dev/mmcblk1p2   → _PBD_DISK=/dev/mmcblk1  _PBD_PARTNUM=2
#   parse_block_device /dev/sda3        → _PBD_DISK=/dev/sda      _PBD_PARTNUM=3
#
# Returns 1 if the path cannot be parsed.
parse_block_device() {
  local dev="$1"
  _PBD_DISK=""
  _PBD_PARTNUM=""

  if [[ "${dev}" =~ ^(/dev/mmcblk[0-9]+)p([0-9]+)$ ]]; then
    _PBD_DISK="${BASH_REMATCH[1]}"
    _PBD_PARTNUM="${BASH_REMATCH[2]}"
  elif [[ "${dev}" =~ ^(/dev/nvme[0-9]+n[0-9]+)p([0-9]+)$ ]]; then
    _PBD_DISK="${BASH_REMATCH[1]}"
    _PBD_PARTNUM="${BASH_REMATCH[2]}"
  elif [[ "${dev}" =~ ^(/dev/[a-z]+)([0-9]+)$ ]]; then
    _PBD_DISK="${BASH_REMATCH[1]}"
    _PBD_PARTNUM="${BASH_REMATCH[2]}"
  else
    return 1
  fi
}

# partition_path <disk-dev> <partnum>
#
# Returns the partition device path for the given disk and partition number.
# Handles both mmcblk-style (p-suffix) and sd-style naming.
partition_path() {
  local disk="$1" partnum="$2"
  if [[ "${disk}" =~ (mmcblk|nvme) ]]; then
    echo "${disk}p${partnum}"
  else
    echo "${disk}${partnum}"
  fi
}

# ─────────────────────────────────────────────
# Network helpers
# ─────────────────────────────────────────────

# Returns the interface carrying the default route (uplink for NAT).
find_uplink_iface() {
  ip route show default 2>/dev/null | awk '{print $5; exit}'
}

# Returns 0 if the interface exists and is backed by cfg80211/mac80211.
is_wireless_iface() {
  local iface="$1"
  [[ -n "${iface}" ]] || return 1
  [[ -e "/sys/class/net/${iface}" ]] || return 1
  [[ -d "/sys/class/net/${iface}/wireless" || -e "/sys/class/net/${iface}/phy80211" ]]
}

# Returns the current nl80211 interface type for a wireless interface.
wifi_iface_type() {
  local iface="$1"
  iw dev "${iface}" info 2>/dev/null | awk '/type/{print $2; exit}'
}

# Returns the NetworkManager device state string for a specific interface.
nm_device_state() {
  local iface="$1"
  local state=""
  command -v nmcli >/dev/null 2>&1 || return 1
  state=$(nmcli -t -f DEVICE,STATE device status 2>/dev/null | awk -F: -v dev="${iface}" '$1 == dev {print $2; exit}')
  [[ -n "${state}" ]] || return 1
  printf "%s\n" "${state}"
}

# Returns 0 if the NetworkManager state represents an active connection.
nm_state_is_connected() {
  local state="${1:-}"
  [[ "${state}" == connected* ]]
}

# Returns the best station-capable Wi-Fi interface for AP provisioning.
#
# Priority:
#   1. NetworkManager-managed Wi-Fi devices already connected
#   2. NetworkManager-managed Wi-Fi devices still connecting
#   3. NetworkManager-managed Wi-Fi devices in disconnected/unavailable states
#   4. Fallback to the first `iw dev` interface whose type is `managed`
#
# Important: we intentionally do NOT filter by interface name patterns such as
# "P2p" because some Realtek drivers expose the primary STA interface with such
# names (for example `wlP2p33s0` on Orange Pi boards).
find_managed_wifi_iface() {
  local preferred_states=("connected" "connecting" "disconnected" "unavailable")
  local desired_state=""
  local device=""
  local type=""
  local state=""
  local iface_type=""

  if command -v nmcli >/dev/null 2>&1; then
    for desired_state in "${preferred_states[@]}"; do
      while IFS=: read -r device type state; do
        [[ "${type}" == "wifi" ]] || continue
        [[ "${state}" == "${desired_state}" ]] || continue
        is_wireless_iface "${device}" || continue
        iface_type=$(wifi_iface_type "${device}")
        [[ -z "${iface_type}" || "${iface_type}" == "managed" ]] || continue
        printf "%s\n" "${device}"
        return 0
      done < <(nmcli -t -f DEVICE,TYPE,STATE device status 2>/dev/null)
    done

    while IFS=: read -r device type state; do
      [[ "${type}" == "wifi" ]] || continue
      is_wireless_iface "${device}" || continue
      iface_type=$(wifi_iface_type "${device}")
      [[ -z "${iface_type}" || "${iface_type}" == "managed" ]] || continue
      printf "%s\n" "${device}"
      return 0
    done < <(nmcli -t -f DEVICE,TYPE,STATE device status 2>/dev/null)
  fi

  while IFS= read -r device; do
    [[ -n "${device}" ]] || continue
    iface_type=$(wifi_iface_type "${device}")
    [[ "${iface_type}" == "managed" ]] || continue
    printf "%s\n" "${device}"
    return 0
  done < <(iw dev 2>/dev/null | awk '/Interface/{print $2}')

  return 1
}

# Release a wireless interface from NetworkManager control.
release_iface_from_nm() {
  local iface="$1"
  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${iface}" managed no 2>/dev/null || true
    nmcli device disconnect "${iface}" 2>/dev/null || true
  fi
}

# Persistently exclude the AP virtual interface from NetworkManager control.
#
# Why this exists:
# Concurrent STA+AP mode uses a virtual interface (for example `wlan0_ap`)
# whose IPv4 address is managed directly by our scripts. If NM auto-discovers
# that interface as a normal Wi-Fi device, it may transition it through
# disconnected states and clear the static AP address, which in turn breaks
# dnsmasq DHCP with "interface ... has no address".
#
# Exclusive mode must NOT keep such an unmanaged rule because the primary Wi-Fi
# interface needs to return to NetworkManager control for STA reconnection.
configure_nm_ap_unmanaged() {
  local ap_iface="$1"
  local ap_exclusive="${2:-false}"
  local nm_conf_dir="/etc/NetworkManager/conf.d"
  local nm_conf_file="${nm_conf_dir}/90-ng-gateway-ap-unmanaged.conf"
  local p2p_iface="p2p-dev-${ap_iface}"

  [[ -n "${ap_iface}" ]] || return 0

  if [[ "${ap_exclusive}" == "true" ]]; then
    if [[ -f "${nm_conf_file}" ]]; then
      rm -f "${nm_conf_file}"
      log "Removed stale NetworkManager AP unmanaged rule for exclusive mode"
    fi
    return 0
  fi

  mkdir -p "${nm_conf_dir}"
  cat > "${nm_conf_file}" <<EOF
# Auto-generated by NG Gateway AP setup.
# Keep the virtual AP interface outside of NetworkManager so its static AP
# address remains owned by ap-setup.sh/hostapd/dnsmasq.
[keyfile]
unmanaged-devices=interface-name:=${ap_iface};interface-name:=${p2p_iface}
EOF

  log "Persisted NetworkManager unmanaged rule for AP interface ${ap_iface}"
}

# Prepare the AP interface in exclusive mode:
#   1. Release from NM
#   2. Down → set type __ap → brief sleep
#   3. Up → flush → assign IP
setup_ap_interface_exclusive() {
  local iface="$1" ip="$2" prefix="$3"

  release_iface_from_nm "${iface}"

  ip link set "${iface}" down 2>/dev/null || true
  iw dev "${iface}" set type __ap 2>/dev/null || true
  sleep 0.5

  ip link set "${iface}" up
  ip addr flush dev "${iface}"
  ip addr add "${ip}/${prefix}" dev "${iface}"
}

# Add NAT / IP-forwarding rules for the AP interface.
# Idempotent: uses iptables -C (check) before -A (append).
setup_nat_rules() {
  local ap_iface="$1" uplink_fallback="${2:-}"

  local uplink
  uplink=$(find_uplink_iface)
  [[ -z "${uplink}" ]] && uplink="${uplink_fallback}"
  [[ -z "${uplink}" ]] && { warn "No uplink interface for NAT"; return 0; }

  sysctl -w net.ipv4.ip_forward=1 > /dev/null 2>&1 || true

  iptables -t nat -C POSTROUTING -o "${uplink}" -j MASQUERADE 2>/dev/null ||
    iptables -t nat -A POSTROUTING -o "${uplink}" -j MASQUERADE 2>/dev/null || true

  iptables -C FORWARD -i "${ap_iface}" -o "${uplink}" -j ACCEPT 2>/dev/null ||
    iptables -A FORWARD -i "${ap_iface}" -o "${uplink}" -j ACCEPT 2>/dev/null || true

  iptables -C FORWARD -i "${uplink}" -o "${ap_iface}" -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null ||
    iptables -A FORWARD -i "${uplink}" -o "${ap_iface}" -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true

  log "NAT configured: ${ap_iface} → ${uplink}"
}

# Remove NAT / IP-forwarding rules. Mirror of setup_nat_rules.
remove_nat_rules() {
  local ap_iface="$1" uplink_fallback="${2:-}"

  local uplink
  uplink=$(find_uplink_iface)
  [[ -z "${uplink}" ]] && uplink="${uplink_fallback}"
  [[ -z "${uplink}" ]] && return 0

  iptables -t nat -D POSTROUTING -o "${uplink}" -j MASQUERADE 2>/dev/null || true
  iptables -D FORWARD -i "${ap_iface}" -o "${uplink}" -j ACCEPT 2>/dev/null || true
  iptables -D FORWARD -i "${uplink}" -o "${ap_iface}" -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true
}

# ─────────────────────────────────────────────
# Systemd helpers
# ─────────────────────────────────────────────

systemd_unit_dir() {
  if [[ -d /usr/lib/systemd/system ]]; then
    echo "/usr/lib/systemd/system"
  else
    echo "/lib/systemd/system"
  fi
}

# Disable and mask conflicting system services (dnsmasq, hostapd).
# Handles both:
#   - enabled units that would auto-start later
#   - already-active units that are currently occupying ports/interfaces
sanitize_conflicting_services() {
  command -v systemctl >/dev/null 2>&1 || return 0

  for svc in dnsmasq.service hostapd.service; do
    local is_enabled="false"
    local is_active="false"

    if systemctl is-enabled "${svc}" 2>/dev/null | grep -q '^enabled$'; then
      is_enabled="true"
    fi
    if systemctl is-active "${svc}" 2>/dev/null | grep -q '^active$'; then
      is_active="true"
    fi

    if [[ "${is_enabled}" == "true" || "${is_active}" == "true" ]]; then
      systemctl disable --now "${svc}" 2>/dev/null || true
      systemctl mask "${svc}" 2>/dev/null || true
      log "Disabled and masked conflicting ${svc} (enabled=${is_enabled}, active=${is_active})"
    fi
  done
}

# ─────────────────────────────────────────────
# Package-manager helpers
# ─────────────────────────────────────────────

# Detect the available package manager. Prints one of: apt, dnf, yum, zypper.
# Returns 1 if none found.
detect_package_manager() {
  local managers=(apt-get dnf yum zypper)
  local names=(apt dnf yum zypper)
  for i in "${!managers[@]}"; do
    if command -v "${managers[$i]}" >/dev/null 2>&1; then
      echo "${names[$i]}"
      return 0
    fi
  done
  return 1
}

# Best-effort package installation across major Linux distributions.
install_packages() {
  local manager="$1"
  shift
  local packages=("$@")
  [[ ${#packages[@]} -gt 0 ]] || return 0

  case "${manager}" in
    apt)    apt-get update -qq && apt-get install -y -qq "${packages[@]}" ;;
    dnf)    dnf install -y "${packages[@]}" ;;
    yum)    yum install -y "${packages[@]}" ;;
    zypper) zypper --non-interactive install --no-confirm "${packages[@]}" ;;
    *)      return 1 ;;
  esac
}
