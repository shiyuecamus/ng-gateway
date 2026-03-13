#!/usr/bin/env bash
set -euo pipefail

# auto-flash-emmc.sh
#
# Headless, unattended eMMC flashing script for production TF maintenance cards.
#
# When a TF maintenance card boots, this script:
#   1. Discovers the target eMMC device (not the boot TF card).
#   2. Locates the golden image on the TF card (/opt/ng-images/).
#   3. Writes the image to eMMC via dd (with decompression if needed).
#   4. Reinforces the Allwinner bootloader (for sunxi platforms).
#   5. Signals success/failure via GPIO LED or kernel console.
#
# The script is idempotent: it checks for a "flash-done" marker on eMMC
# after flashing to avoid re-flashing on accidental reboots.
#
# Designed to be launched by ng-gateway-auto-flash.service at boot.
#
# Usage (manual):
#   sudo bash auto-flash-emmc.sh [--force]
#
# Image search path: /opt/ng-images/*.img.zst  (or .img.gz, .img)
# The newest image (by filename sort) is selected automatically.

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[auto-flash]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../shared/_common.sh"

IMAGE_DIR="/opt/ng-images"
FLASH_MARKER_LABEL="auto-flash-done"
FORCE_FLASH=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --force) FORCE_FLASH=1; shift ;;
    -h|--help)
      echo "Usage: ${SCRIPT_NAME} [--force]"
      echo "  --force   Flash even if eMMC already has a valid image"
      exit 0
      ;;
    *) die "Unknown option: $1" ;;
  esac
done

require_root

# ─────────────────────────────────────────────
# Visual / audible feedback helpers
# ─────────────────────────────────────────────

# Generic LED control. Orange Pi boards typically expose a green and red
# LED via /sys/class/leds/. Names vary by board; we try common patterns.
_led_path() {
  local color="$1"
  for p in \
    "/sys/class/leds/${color}:status" \
    "/sys/class/leds/${color}_led" \
    "/sys/class/leds/${color}:indicator" \
    "/sys/class/leds/${color}:disk" \
    "/sys/class/leds/orangepi:${color}:status"; do
    [[ -d "$p" ]] && { echo "$p"; return 0; }
  done
  return 1
}

led_on() {
  local p; p=$(_led_path "$1" 2>/dev/null) || return 0
  echo none   > "${p}/trigger" 2>/dev/null || true
  echo 255    > "${p}/brightness" 2>/dev/null || true
}

led_off() {
  local p; p=$(_led_path "$1" 2>/dev/null) || return 0
  echo 0 > "${p}/brightness" 2>/dev/null || true
}

led_blink() {
  local p; p=$(_led_path "$1" 2>/dev/null) || return 0
  echo timer > "${p}/trigger" 2>/dev/null || true
  echo "${2:-500}" > "${p}/delay_on" 2>/dev/null || true
  echo "${3:-500}" > "${p}/delay_off" 2>/dev/null || true
}

signal_flashing() {
  led_blink "green" 200 200
  led_off "red"
}

signal_success() {
  led_on "green"
  led_off "red"
}

signal_failure() {
  led_off "green"
  led_blink "red" 100 100
}

# ─────────────────────────────────────────────
# Step 1: Identify boot device and eMMC target
# ─────────────────────────────────────────────

log "=========================================="
log "NG Gateway Auto-Flash (TF → eMMC)"
log "=========================================="
log ""

BOOT_SOURCE=$(findmnt -n -o SOURCE / 2>/dev/null | head -1) || true
[[ -z "${BOOT_SOURCE}" ]] && die "Cannot identify boot device"

parse_block_device "${BOOT_SOURCE}" || die "Cannot parse boot device: ${BOOT_SOURCE}"
BOOT_DISK="${_PBD_DISK}"
log "Boot device (TF card): ${BOOT_DISK}"

# Find the eMMC: scan mmcblk devices, exclude the boot disk.
EMMC_DEVICE=""
for dev in /dev/mmcblk[0-9]; do
  [[ -b "$dev" ]] || continue
  [[ "$dev" == "${BOOT_DISK}" ]] && continue
  # Prefer the device that is NOT removable (eMMC vs SD).
  local_removable=$(cat "/sys/block/$(basename "$dev")/removable" 2>/dev/null || echo "1")
  if [[ "${local_removable}" == "0" ]]; then
    EMMC_DEVICE="$dev"
    break
  fi
  # Fallback: use the first non-boot mmcblk.
  [[ -z "${EMMC_DEVICE}" ]] && EMMC_DEVICE="$dev"
done

[[ -z "${EMMC_DEVICE}" ]] && die "No eMMC device found (only ${BOOT_DISK} detected)"
log "Target eMMC:           ${EMMC_DEVICE}"

EMMC_SIZE=$(blockdev --getsize64 "${EMMC_DEVICE}" 2>/dev/null || echo 0)
EMMC_SIZE_GB=$(( EMMC_SIZE / 1073741824 ))
log "eMMC capacity:         ${EMMC_SIZE_GB} GB"

# Refuse to write to mounted devices.
if mount | grep -q "${EMMC_DEVICE}"; then
  log "eMMC has mounted partitions — unmounting..."
  umount "${EMMC_DEVICE}"* 2>/dev/null || true
  sleep 1
  if mount | grep -q "${EMMC_DEVICE}"; then
    die "Cannot unmount all partitions on ${EMMC_DEVICE}"
  fi
fi

# ─────────────────────────────────────────────
# Step 2: Check if eMMC already flashed
# ─────────────────────────────────────────────

if [[ ${FORCE_FLASH} -eq 0 ]]; then
  # Check if the eMMC rootfs has the auto-flash marker from a previous run.
  # The rootfs may be partition 1 (single-partition MBR, e.g. OPi 4 Pro) or
  # partition 2 (dual-partition GPT, e.g. OPi 5 Plus). Try auto-detection
  # first, then fall back to scanning partitions 1 and 2.
  _check_partnums=""
  if detect_disk_layout "${EMMC_DEVICE}" 2>/dev/null; then
    _check_partnums="${_DL_ROOT_PARTNUM}"
  else
    _check_partnums="1 2"
  fi

  for _pn in ${_check_partnums}; do
    _check_part=$(partition_path "${EMMC_DEVICE}" "${_pn}")
    [[ -b "${_check_part}" ]] || continue
    TMP_MNT=$(mktemp -d)
    if mount -o ro "${_check_part}" "${TMP_MNT}" 2>/dev/null; then
      if [[ -f "${TMP_MNT}/var/lib/ng-gateway/.${FLASH_MARKER_LABEL}" ]]; then
        umount "${TMP_MNT}" 2>/dev/null || true
        rmdir "${TMP_MNT}" 2>/dev/null || true
        log ""
        log "eMMC already flashed (marker found on partition ${_pn}). Use --force to re-flash."
        signal_success
        exit 0
      fi
      umount "${TMP_MNT}" 2>/dev/null || true
    fi
    rmdir "${TMP_MNT}" 2>/dev/null || true
  done
fi

# ─────────────────────────────────────────────
# Step 3: Locate the golden image on TF card
# ─────────────────────────────────────────────

log ""
log "Searching for golden image in ${IMAGE_DIR}/..."

IMAGE_FILE=""
MANIFEST_FILE=""

# Priority: .img.zst > .img.gz > .img — pick the newest by name sort.
for ext in img.zst img.gz img; do
  for f in $(ls -1 "${IMAGE_DIR}/"*.${ext} 2>/dev/null | sort -V -r); do
    IMAGE_FILE="$f"
    break
  done
  [[ -n "${IMAGE_FILE}" ]] && break
done

[[ -z "${IMAGE_FILE}" ]] && die "No golden image found in ${IMAGE_DIR}/  (expected *.img.zst, *.img.gz, or *.img)"

log "Selected image:        ${IMAGE_FILE}"
IMAGE_SIZE=$(stat -c%s "${IMAGE_FILE}" 2>/dev/null || stat -f%z "${IMAGE_FILE}" 2>/dev/null || echo 0)
log "Image size:            $(( IMAGE_SIZE / 1048576 )) MB"

# Locate manifest and sha256 alongside the image.
IMAGE_BASE="${IMAGE_FILE}"
IMAGE_BASE="${IMAGE_BASE%.zst}"
IMAGE_BASE="${IMAGE_BASE%.gz}"
IMAGE_BASE="${IMAGE_BASE%.img}"
[[ -f "${IMAGE_BASE}.manifest.json" ]] && MANIFEST_FILE="${IMAGE_BASE}.manifest.json"

# ─────────────────────────────────────────────
# Step 4: Verify checksum
# ─────────────────────────────────────────────

SHA256_FILE="${IMAGE_FILE}.sha256"
if [[ -f "${SHA256_FILE}" ]]; then
  log ""
  log "Verifying SHA256 checksum..."
  EXPECTED=$(awk '{print $1}' "${SHA256_FILE}")
  ACTUAL=$(sha256sum "${IMAGE_FILE}" | awk '{print $1}')
  if [[ "${EXPECTED}" == "${ACTUAL}" ]]; then
    log "  Checksum OK: ${ACTUAL}"
  else
    signal_failure
    die "Checksum MISMATCH! Expected: ${EXPECTED}  Actual: ${ACTUAL}"
  fi
else
  warn "No .sha256 file found — skipping checksum verification"
fi

# ─────────────────────────────────────────────
# Step 5: Flash image to eMMC
# ─────────────────────────────────────────────

log ""
log "Flashing image to ${EMMC_DEVICE}..."
signal_flashing

COMP_TOOL="none"
case "${IMAGE_FILE}" in
  *.zst) COMP_TOOL="zstd" ;;
  *.gz)  COMP_TOOL="gzip" ;;
esac

FLASH_START=$(date +%s)

case "${COMP_TOOL}" in
  zstd) zstd -d -c "${IMAGE_FILE}" | dd of="${EMMC_DEVICE}" bs=4M conv=fsync status=progress 2>&1 ;;
  gzip) gzip -d -c "${IMAGE_FILE}" | dd of="${EMMC_DEVICE}" bs=4M conv=fsync status=progress 2>&1 ;;
  none) dd if="${IMAGE_FILE}" of="${EMMC_DEVICE}" bs=4M conv=fsync status=progress 2>&1 ;;
esac

sync
blockdev --flushbufs "${EMMC_DEVICE}" 2>/dev/null || true

FLASH_END=$(date +%s)
FLASH_DURATION=$(( FLASH_END - FLASH_START ))
log "  Flash completed in ${FLASH_DURATION}s"

# ─────────────────────────────────────────────
# Step 6: Reinforce Allwinner bootloader
# ─────────────────────────────────────────────

SUNXI_NEEDED="false"

if [[ -n "${MANIFEST_FILE}" ]] && command -v jq >/dev/null 2>&1; then
  SUNXI_NEEDED=$(jq -r '.sunxi_bootloader // false' "${MANIFEST_FILE}" 2>/dev/null || echo "false")
fi

if [[ "${SUNXI_NEEDED}" != "true" ]]; then
  # Fall back to platform / device detection.
  if is_sunxi_platform 2>/dev/null || has_sunxi_bootloader "${EMMC_DEVICE}" 2>/dev/null; then
    SUNXI_NEEDED="true"
  fi
fi

if [[ "${SUNXI_NEEDED}" == "true" ]]; then
  log ""
  log "Allwinner platform — reinforcing bootloader..."

  local_uboot_dir=$(find_sunxi_uboot_dir 2>/dev/null || true)
  if [[ -n "${local_uboot_dir}" ]]; then
    write_sunxi_bootloader "${EMMC_DEVICE}" "${local_uboot_dir}"
    sync
  else
    if has_sunxi_bootloader "${EMMC_DEVICE}" 2>/dev/null; then
      log "  Bootloader already present in image (eGON.BT0 at sector 16)"
    else
      signal_failure
      die "No Allwinner firmware files found and no bootloader in image!"
    fi
  fi
fi

# ─────────────────────────────────────────────
# Step 7: Post-flash verification
# ─────────────────────────────────────────────

log ""
log "Post-flash verification..."

# Re-read partition table.
partprobe "${EMMC_DEVICE}" 2>/dev/null || true
sleep 1

# Quick fsck on rootfs.
ROOT_PARTNUM=""
if [[ -n "${MANIFEST_FILE}" ]] && command -v jq >/dev/null 2>&1; then
  ROOT_PARTNUM=$(jq -r '.root_partnum // empty' "${MANIFEST_FILE}" 2>/dev/null || true)
fi
if [[ -z "${ROOT_PARTNUM}" ]] && detect_disk_layout "${EMMC_DEVICE}" 2>/dev/null; then
  ROOT_PARTNUM="${_DL_ROOT_PARTNUM}"
fi

if [[ -n "${ROOT_PARTNUM}" ]]; then
  ROOT_PART=$(partition_path "${EMMC_DEVICE}" "${ROOT_PARTNUM}")
  if [[ -b "${ROOT_PART}" ]]; then
    log "  Checking rootfs (${ROOT_PART})..."
    e2fsck -n "${ROOT_PART}" 2>&1 | tail -3 || true
  fi
fi

# Write auto-flash marker into eMMC rootfs (not /boot).
# ROOT_PARTNUM was resolved above via manifest or auto-detection.
if [[ -n "${ROOT_PARTNUM}" ]]; then
  ROOT_PART=$(partition_path "${EMMC_DEVICE}" "${ROOT_PARTNUM}")
  TMP_MNT=$(mktemp -d)
  if mount "${ROOT_PART}" "${TMP_MNT}" 2>/dev/null; then
    mkdir -p "${TMP_MNT}/var/lib/ng-gateway"
    cat > "${TMP_MNT}/var/lib/ng-gateway/.${FLASH_MARKER_LABEL}" <<EOF
flashed_at=$(date -Iseconds)
image=$(basename "${IMAGE_FILE}")
emmc_device=${EMMC_DEVICE}
flash_duration_sec=${FLASH_DURATION}
tf_boot_disk=${BOOT_DISK}
EOF
    sync
    umount "${TMP_MNT}" 2>/dev/null || true
    log "  Auto-flash marker written"
  fi
  rmdir "${TMP_MNT}" 2>/dev/null || true
fi

# ─────────────────────────────────────────────
# Done
# ─────────────────────────────────────────────

signal_success

log ""
log "=========================================="
log "Auto-Flash Complete"
log "=========================================="
log ""
log "  Device:     ${EMMC_DEVICE} (${EMMC_SIZE_GB} GB)"
log "  Image:      $(basename "${IMAGE_FILE}")"
log "  Duration:   ${FLASH_DURATION}s"
log ""
log "Next:"
log "  1. Power off this device"
log "  2. Remove the TF maintenance card"
log "  3. Power on — eMMC will boot and run first-boot initialization"
log ""
