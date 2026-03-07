#!/usr/bin/env bash
# _common.sh
#
# Shared utility library for NG Gateway deployment scripts.
# Source this file at the top of any script that needs common helpers:
#
#   SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
#   source "${SCRIPT_DIR}/_common.sh"
#
# The caller MUST set LOG_TAG before sourcing (or it defaults to the script name).
#
# Provided functions:
#   Logging:       log, warn, die
#   Guards:        require_root, require_commands
#   Block device:  parse_block_device, partition_path
#   Network:       find_uplink_iface, release_iface_from_nm, setup_ap_interface,
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

# Release a wireless interface from NetworkManager control.
release_iface_from_nm() {
  local iface="$1"
  if command -v nmcli >/dev/null 2>&1; then
    nmcli device set "${iface}" managed no 2>/dev/null || true
    nmcli device disconnect "${iface}" 2>/dev/null || true
  fi
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

# Disable and mask conflicting system services (dnsmasq, hostapd).
sanitize_conflicting_services() {
  command -v systemctl >/dev/null 2>&1 || return 0

  for svc in dnsmasq.service hostapd.service; do
    if systemctl is-enabled "${svc}" 2>/dev/null | grep -q enabled; then
      systemctl disable --now "${svc}" 2>/dev/null || true
      systemctl mask "${svc}" 2>/dev/null || true
      log "Disabled and masked conflicting ${svc}"
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
