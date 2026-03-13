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
#   Disk layout:   disk_label_type, detect_root_partition, detect_boot_partition,
#                  detect_disk_layout, print_disk_layout
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
# Allwinner (sunxi) bootloader helpers
# ─────────────────────────────────────────────

# Allwinner SoCs (H616, A733, H6, H5, …) store their bootloader in raw disk
# sectors *outside* (or overlapping with) the first partition:
#
#   boot0 (SPL):       dd bs=8k seek=1    → offset 8KB   (sector 16)
#   boot_package (FIT):dd bs=8k seek=2050 → offset ~16MB (sector 32800)
#
# On boards where the first partition starts at sector 8192 (4MB), the
# boot_package physically overlaps with the ext4 partition area. This is
# intentional: nand-sata-install writes boot_package *after* mkfs, so it
# occupies raw sectors that ext4 considers free. However, if we later run
# resize2fs -M (shrink), ext4 may relocate data into those sectors and
# destroy the bootloader.
#
# The helpers below detect the Allwinner platform, locate the bootloader
# firmware files, and provide backup/restore operations.

# Returns 0 if the running system (or a mounted rootfs) is an Allwinner/sunxi
# platform that uses the boot0 + boot_package scheme.
is_sunxi_platform() {
  # Check running kernel.
  if uname -r 2>/dev/null | grep -qi 'sun[0-9]\+i'; then
    return 0
  fi
  # Check /etc/orangepi-release.
  if [[ -f /etc/orangepi-release ]]; then
    if grep -qi 'sun[0-9]\+i' /etc/orangepi-release 2>/dev/null; then
      return 0
    fi
  fi
  # Check device-tree model.
  local model=""
  model=$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || true)
  if echo "${model}" | grep -qi 'sun[0-9]\+i'; then
    return 0
  fi
  return 1
}

# Returns 0 if a block device contains an Allwinner eGON.BT0 boot0 header
# at the standard sector-16 offset. Use this to detect sunxi layout on a
# target device when the running system itself may not be Allwinner.
has_sunxi_bootloader() {
  local disk="${1:?}"
  [[ -b "${disk}" ]] || [[ -f "${disk}" ]] || return 1
  local magic
  magic=$(dd if="${disk}" bs=512 skip=16 count=1 status=none 2>/dev/null \
    | head -c 12 | strings 2>/dev/null || true)
  [[ "${magic}" == *"eGON.BT0"* ]]
}

# Locate the Allwinner u-boot directory containing boot0_sdcard.fex and
# boot_package.fex. Prints the directory path. Returns 1 if not found.
#
# When called with an optional $1 = mounted rootfs prefix (e.g. /tmp/mnt),
# searches under that prefix instead of the live root.
find_sunxi_uboot_dir() {
  local prefix="${1:-}"
  local dir=""

  # Primary: canonical path from platform_install.sh (set as DIR=...).
  if [[ -f "${prefix}/usr/lib/u-boot/platform_install.sh" ]]; then
    dir=$(grep -oP '^\s*DIR=\K\S+' "${prefix}/usr/lib/u-boot/platform_install.sh" 2>/dev/null || true)
    dir="${prefix}${dir}"
    if [[ -n "${dir}" ]] && [[ -f "${dir}/boot0_sdcard.fex" ]] && [[ -f "${dir}/boot_package.fex" ]]; then
      echo "${dir}"
      return 0
    fi
  fi

  # Fallback: glob search.
  for dir in "${prefix}"/usr/lib/linux-u-boot-*; do
    if [[ -f "${dir}/boot0_sdcard.fex" ]] && [[ -f "${dir}/boot_package.fex" ]]; then
      echo "${dir}"
      return 0
    fi
  done

  return 1
}

# Backup Allwinner bootloader raw regions from a block device to a directory.
#
# Usage: backup_sunxi_bootloader <disk-dev> <output-dir>
#
# Creates:
#   <output-dir>/boot0.bin     (sectors 16..511, 248KB)
#   <output-dir>/boot_pkg.bin  (sectors 32800..35391, ~1.3MB)
#
# The sector ranges are based on the standard Allwinner layout and sized to
# cover the largest known boot0 (240KB) and boot_package (1.3MB) payloads
# with generous safety margins.
backup_sunxi_bootloader() {
  local disk="$1"
  local outdir="$2"

  mkdir -p "${outdir}"

  # boot0: sector 16, max ~240KB → read 496 sectors (248KB).
  dd if="${disk}" of="${outdir}/boot0.bin" \
    bs=512 skip=16 count=496 status=none 2>/dev/null \
    || die "Failed to backup boot0 from ${disk}"

  # boot_package: sector 32800, max ~1.3MB → read 2592 sectors (1296KB).
  dd if="${disk}" of="${outdir}/boot_pkg.bin" \
    bs=512 skip=32800 count=2592 status=none 2>/dev/null \
    || die "Failed to backup boot_package from ${disk}"

  log "  Backed up Allwinner bootloader from ${disk}"
  log "    boot0:        ${outdir}/boot0.bin ($(stat -c%s "${outdir}/boot0.bin" 2>/dev/null || stat -f%z "${outdir}/boot0.bin") bytes)"
  log "    boot_package: ${outdir}/boot_pkg.bin ($(stat -c%s "${outdir}/boot_pkg.bin" 2>/dev/null || stat -f%z "${outdir}/boot_pkg.bin") bytes)"
}

# Restore (rewrite) Allwinner bootloader from backup files into a block
# device or raw image file.
#
# Usage: restore_sunxi_bootloader <target> <backup-dir>
#
# <target> can be a block device (/dev/mmcblk0) or a raw .img file.
restore_sunxi_bootloader() {
  local target="$1"
  local backupdir="$2"

  [[ -f "${backupdir}/boot0.bin" ]] || die "boot0.bin not found in ${backupdir}"
  [[ -f "${backupdir}/boot_pkg.bin" ]] || die "boot_pkg.bin not found in ${backupdir}"

  dd if="${backupdir}/boot0.bin" of="${target}" \
    bs=512 seek=16 conv=notrunc status=none 2>/dev/null \
    || die "Failed to restore boot0 to ${target}"

  dd if="${backupdir}/boot_pkg.bin" of="${target}" \
    bs=512 seek=32800 conv=notrunc status=none 2>/dev/null \
    || die "Failed to restore boot_package to ${target}"

  log "  Restored Allwinner bootloader to ${target}"
}

# Write Allwinner bootloader from firmware files (boot0_sdcard.fex +
# boot_package.fex) to a block device or raw image.
#
# Usage: write_sunxi_bootloader <target> <uboot-dir>
#
# This mirrors the official write_uboot_platform() from platform_install.sh.
write_sunxi_bootloader() {
  local target="$1"
  local uboot_dir="$2"

  [[ -f "${uboot_dir}/boot0_sdcard.fex" ]] || die "boot0_sdcard.fex not found in ${uboot_dir}"
  [[ -f "${uboot_dir}/boot_package.fex" ]] || die "boot_package.fex not found in ${uboot_dir}"

  dd if="${uboot_dir}/boot0_sdcard.fex" of="${target}" \
    bs=8k seek=1 conv=notrunc,fsync status=none 2>/dev/null \
    || die "Failed to write boot0 to ${target}"

  dd if="${uboot_dir}/boot_package.fex" of="${target}" \
    bs=8k seek=2050 conv=notrunc,fsync status=none 2>/dev/null \
    || die "Failed to write boot_package to ${target}"

  log "  Wrote Allwinner bootloader to ${target}"
}

# ─────────────────────────────────────────────
# Disk-layout detection helpers (factory scripts)
# ─────────────────────────────────────────────

# disk_label_type <disk-dev>
#
# Returns the partition table type of a disk: "gpt", "dos", or "unknown".
# Uses blkid PTTYPE which works on both GPT and MBR disks.
disk_label_type() {
  local disk="$1"
  local label
  label=$(blkid -o value -s PTTYPE "${disk}" 2>/dev/null || true)
  if [[ -z "${label}" ]]; then
    label=$(parted -ms "${disk}" print 2>/dev/null | awk -F: 'NR==2{print $6}' || true)
  fi
  case "${label}" in
    gpt)       echo "gpt" ;;
    msdos|dos) echo "dos" ;;
    *)         echo "unknown" ;;
  esac
}

# detect_root_partition <disk-dev>
#
# Auto-detect the root (Linux/ext4) partition number on a disk.
# Strategy:
#   1. If only one Linux partition exists, use it.
#   2. If multiple partitions exist, prefer the one labelled "opi_root",
#      "rootfs", or "ROOT" (common Orange Pi / Armbian conventions).
#   3. Fall back to the highest-numbered Linux/ext4 partition (typically
#      the rootfs in dual-partition layouts where p1=boot, p2=rootfs).
#
# Prints the partition number. Returns 1 if no suitable partition found.
detect_root_partition() {
  local disk="$1"
  local candidates=()
  local partnum=""
  local partdev=""
  local fstype=""
  local label=""

  while IFS= read -r line; do
    [[ "${line}" =~ ^[0-9]+: ]] || continue
    partnum=$(echo "${line}" | cut -d: -f1)
    partdev=$(partition_path "${disk}" "${partnum}")
    [[ -b "${partdev}" ]] || continue

    fstype=$(blkid -o value -s TYPE "${partdev}" 2>/dev/null || true)
    [[ "${fstype}" == "ext4" || "${fstype}" == "ext3" || "${fstype}" == "ext2" ]] || continue
    candidates+=("${partnum}")

    label=$(blkid -o value -s LABEL "${partdev}" 2>/dev/null || true)
    label_lower=$(echo "${label}" | tr '[:upper:]' '[:lower:]')
    if [[ "${label_lower}" == "opi_root" || "${label_lower}" == "rootfs" || "${label_lower}" == "root" ]]; then
      echo "${partnum}"
      return 0
    fi
  done < <(parted -ms "${disk}" unit s print 2>/dev/null || true)

  if [[ ${#candidates[@]} -eq 0 ]]; then
    return 1
  fi

  if [[ ${#candidates[@]} -eq 1 ]]; then
    echo "${candidates[0]}"
    return 0
  fi

  local highest="${candidates[0]}"
  for c in "${candidates[@]}"; do
    (( c > highest )) && highest="${c}"
  done
  echo "${highest}"
}

# detect_boot_partition <disk-dev> <root-partnum>
#
# Auto-detect a separate boot partition on a disk, if one exists.
# Returns the partition number if a dedicated boot partition is found,
# or returns 1 if boot is embedded in the root partition (single-partition layout).
#
# Strategy:
#   1. Look for a vfat partition (common boot filesystem).
#   2. Look for a partition labelled "boot" or "BOOT".
#   3. If a partition exists before root that is not the root, treat it as boot.
#   4. If none found, boot is embedded in rootfs.
detect_boot_partition() {
  local disk="$1"
  local root_partnum="$2"
  local partnum=""
  local partdev=""
  local fstype=""
  local label=""

  while IFS= read -r line; do
    [[ "${line}" =~ ^[0-9]+: ]] || continue
    partnum=$(echo "${line}" | cut -d: -f1)
    [[ "${partnum}" -ne "${root_partnum}" ]] || continue
    partdev=$(partition_path "${disk}" "${partnum}")
    [[ -b "${partdev}" ]] || continue

    fstype=$(blkid -o value -s TYPE "${partdev}" 2>/dev/null || true)
    label=$(blkid -o value -s LABEL "${partdev}" 2>/dev/null || true)
    label_lower=$(echo "${label}" | tr '[:upper:]' '[:lower:]')

    if [[ "${fstype}" == "vfat" || "${label_lower}" == "boot" ]]; then
      echo "${partnum}"
      return 0
    fi
  done < <(parted -ms "${disk}" unit s print 2>/dev/null || true)

  return 1
}

# detect_disk_layout <disk-dev>
#
# Unified disk layout detection for factory scripts.
# Sets the following global variables:
#   _DL_LABEL         - "gpt" or "dos"
#   _DL_ROOT_PARTNUM  - root partition number
#   _DL_ROOT_PART     - root partition device path
#   _DL_ROOT_FS       - root filesystem type
#   _DL_BOOT_SEPARATE - "true" if a dedicated boot partition exists
#   _DL_BOOT_PARTNUM  - boot partition number (empty if embedded)
#   _DL_BOOT_PART     - boot partition device path (empty if embedded)
#   _DL_BOOT_FS       - boot filesystem type (empty if embedded)
#
# Returns 1 if no root partition can be detected.
detect_disk_layout() {
  local disk="$1"

  _DL_LABEL=$(disk_label_type "${disk}")
  _DL_ROOT_PARTNUM=""
  _DL_ROOT_PART=""
  _DL_ROOT_FS=""
  _DL_BOOT_SEPARATE="false"
  _DL_BOOT_PARTNUM=""
  _DL_BOOT_PART=""
  _DL_BOOT_FS=""

  _DL_ROOT_PARTNUM=$(detect_root_partition "${disk}") || {
    warn "No Linux root partition found on ${disk}"
    return 1
  }

  _DL_ROOT_PART=$(partition_path "${disk}" "${_DL_ROOT_PARTNUM}")
  _DL_ROOT_FS=$(blkid -o value -s TYPE "${_DL_ROOT_PART}" 2>/dev/null || echo "ext4")

  local boot_pn=""
  boot_pn=$(detect_boot_partition "${disk}" "${_DL_ROOT_PARTNUM}" 2>/dev/null) || true
  if [[ -n "${boot_pn}" ]]; then
    _DL_BOOT_SEPARATE="true"
    _DL_BOOT_PARTNUM="${boot_pn}"
    _DL_BOOT_PART=$(partition_path "${disk}" "${_DL_BOOT_PARTNUM}")
    _DL_BOOT_FS=$(blkid -o value -s TYPE "${_DL_BOOT_PART}" 2>/dev/null || echo "vfat")
  fi
}

# print_disk_layout
#
# Print a summary of the detected disk layout for human review.
# Must be called after detect_disk_layout.
print_disk_layout() {
  log "Detected disk layout:"
  log "  Partition table:  ${_DL_LABEL}"
  log "  Root partition:   ${_DL_ROOT_PART} (partnum=${_DL_ROOT_PARTNUM}, fs=${_DL_ROOT_FS})"
  if [[ "${_DL_BOOT_SEPARATE}" == "true" ]]; then
    log "  Boot partition:   ${_DL_BOOT_PART} (partnum=${_DL_BOOT_PARTNUM}, fs=${_DL_BOOT_FS})"
  else
    log "  Boot partition:   embedded in rootfs (single-partition layout)"
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
