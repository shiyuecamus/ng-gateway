#!/usr/bin/env bash
set -euo pipefail

# flash-image.sh
#
# Flash a compressed NG Gateway golden image to a target eMMC device.
#
# Usage:
#   sudo bash flash-image.sh --image /mnt/usb/ng-gateway-v1.0.0.img.zst --device /dev/mmcblk1
#
# Safety:
#   - Refuses to write to the boot device (prevents bricking the running system)
#   - Requires explicit confirmation (unless --yes is passed)
#   - Verifies checksum before writing (if available)

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[flash]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../shared/_common.sh"

# ─── Argument Parsing ───

IMAGE=""
DEVICE=""
BLOCK_SIZE="4M"
SKIP_VERIFY=0
AUTO_YES=0

usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Options:
  --image PATH        Path to the compressed image file (.img.zst, .img.gz, or .img)
  --device DEVICE     Target eMMC block device (e.g. /dev/mmcblk1)
  --bs BLOCK_SIZE     dd block size (default: 4M)
  --skip-verify       Skip SHA256 verification
  --yes               Skip confirmation prompt
  -h, --help          Show this help

Example:
  sudo ${SCRIPT_NAME} --image /mnt/usb/ng-gateway-v1.0.0.img.zst --device /dev/mmcblk1
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --image)       IMAGE="$2";      shift 2 ;;
    --device)      DEVICE="$2";     shift 2 ;;
    --bs)          BLOCK_SIZE="$2"; shift 2 ;;
    --skip-verify) SKIP_VERIFY=1;   shift ;;
    --yes)         AUTO_YES=1;      shift ;;
    -h|--help)     usage ;;
    *)             die "Unknown option: $1" ;;
  esac
done

[[ -z "${IMAGE}" ]]  && die "Missing --image"
[[ -z "${DEVICE}" ]] && die "Missing --device"
[[ -f "${IMAGE}" ]]  || die "Image file not found: ${IMAGE}"
[[ -b "${DEVICE}" ]] || die "Not a block device: ${DEVICE}"

# ─── Safety Checks ───

require_root

# Prevent writing to the boot device.
BOOT_DEVICE=$(findmnt -n -o SOURCE / 2>/dev/null | head -1 || true)
if [[ -n "${BOOT_DEVICE}" ]] && parse_block_device "${BOOT_DEVICE}"; then
  if [[ "${DEVICE}" == "${_PBD_DISK}" ]]; then
    die "REFUSING to write to the boot device (${_PBD_DISK}). You would brick the running system!"
  fi
fi

if mount | grep -q "${DEVICE}"; then
  die "Target device ${DEVICE} has mounted partitions. Unmount first: sudo umount ${DEVICE}*"
fi

# Detect compression from file extension.
COMP_TOOL=""
case "${IMAGE}" in
  *.zst) require_commands zstd;  COMP_TOOL="zstd" ;;
  *.gz)  require_commands gzip;  COMP_TOOL="gzip" ;;
  *.img) COMP_TOOL="none" ;;
  *)     warn "Unknown extension — treating as raw image"; COMP_TOOL="none" ;;
esac

DEVICE_SIZE=$(blockdev --getsize64 "${DEVICE}" 2>/dev/null || echo "unknown")
DEVICE_SIZE_GB="unknown"
[[ "${DEVICE_SIZE}" != "unknown" ]] && DEVICE_SIZE_GB=$((DEVICE_SIZE / 1073741824))
IMAGE_SIZE=$(stat -c%s "${IMAGE}" 2>/dev/null || stat -f%z "${IMAGE}" 2>/dev/null || echo "unknown")
IMAGE_SIZE_MB="unknown"
[[ "${IMAGE_SIZE}" != "unknown" ]] && IMAGE_SIZE_MB=$((IMAGE_SIZE / 1048576))

log "=========================================="
log "NG Gateway Image Flasher"
log "=========================================="
log "Image:            ${IMAGE} (${IMAGE_SIZE_MB} MB compressed)"
log "Target:           ${DEVICE} (${DEVICE_SIZE_GB} GB)"
log "Compression:      ${COMP_TOOL}"
log "Block size:       ${BLOCK_SIZE}"
log "=========================================="
log ""

# ─── Confirmation ───

if [[ ${AUTO_YES} -eq 0 ]]; then
  log "WARNING: This will ERASE ALL DATA on ${DEVICE}!"
  read -rp "${LOG_TAG} Type 'yes' to continue: " confirm
  [[ "${confirm}" == "yes" ]] || die "Aborted by user"
fi

# ─── Step 1: Verify checksum ───

if [[ ${SKIP_VERIFY} -eq 0 ]]; then
  SHA256_FILE="${IMAGE}.sha256"
  if [[ -f "${SHA256_FILE}" ]]; then
    log "Step 1/3: Verifying SHA256 checksum..."
    EXPECTED=$(awk '{print $1}' "${SHA256_FILE}")
    ACTUAL=$(sha256sum "${IMAGE}" | awk '{print $1}')
    if [[ "${EXPECTED}" == "${ACTUAL}" ]]; then
      log "  Checksum OK: ${ACTUAL}"
    else
      die "Checksum MISMATCH! Expected: ${EXPECTED}  Actual: ${ACTUAL}"
    fi
  else
    log "Step 1/3: No .sha256 file found — skipping verification"
    warn "Consider generating a checksum for production use"
  fi
else
  log "Step 1/3: Checksum verification skipped (--skip-verify)"
fi

# ─── Step 2: Flash image ───

log "Step 2/3: Flashing image to ${DEVICE}..."

flash_cmd() {
  case "${COMP_TOOL}" in
    zstd) zstd -d -c "${IMAGE}" | dd of="${DEVICE}" bs="${BLOCK_SIZE}" conv=fsync status=progress ;;
    gzip) gzip -d -c "${IMAGE}" | dd of="${DEVICE}" bs="${BLOCK_SIZE}" conv=fsync status=progress ;;
    none) dd if="${IMAGE}" of="${DEVICE}" bs="${BLOCK_SIZE}" conv=fsync status=progress ;;
  esac
}

if command -v pv >/dev/null 2>&1 && [[ "${COMP_TOOL}" != "none" ]]; then
  case "${COMP_TOOL}" in
    zstd) pv "${IMAGE}" | zstd -d -c | dd of="${DEVICE}" bs="${BLOCK_SIZE}" conv=fsync 2>/dev/null ;;
    gzip) pv "${IMAGE}" | gzip -d -c | dd of="${DEVICE}" bs="${BLOCK_SIZE}" conv=fsync 2>/dev/null ;;
  esac
else
  flash_cmd
fi

# ─── Step 3: Sync and verify ───

log "Step 3/3: Syncing and verifying..."

sync
blockdev --flushbufs "${DEVICE}" 2>/dev/null || true

if command -v fdisk >/dev/null 2>&1; then
  log "  Partition table:"
  fdisk -l "${DEVICE}" 2>/dev/null | grep "^${DEVICE}" || true
fi

# Identify rootfs partition for quick fsck.
# Priority: manifest root_partnum > auto-detect > skip.
ROOT_PARTNUM=""
MANIFEST_PATH=""

# Try to locate the manifest alongside the image file.
IMAGE_BASE="${IMAGE}"
IMAGE_BASE="${IMAGE_BASE%.zst}"
IMAGE_BASE="${IMAGE_BASE%.gz}"
IMAGE_BASE="${IMAGE_BASE%.img}"
if [[ -f "${IMAGE_BASE}.manifest.json" ]]; then
  MANIFEST_PATH="${IMAGE_BASE}.manifest.json"
elif [[ -f "${IMAGE%.zst}.manifest.json" ]]; then
  MANIFEST_PATH="${IMAGE%.zst}.manifest.json"
fi

if [[ -n "${MANIFEST_PATH}" ]] && command -v jq >/dev/null 2>&1; then
  ROOT_PARTNUM=$(jq -r '.root_partnum // empty' "${MANIFEST_PATH}" 2>/dev/null || true)
  if [[ -n "${ROOT_PARTNUM}" ]]; then
    log "  Root partition from manifest: partition ${ROOT_PARTNUM}"
  fi
fi

if [[ -z "${ROOT_PARTNUM}" ]]; then
  if detect_disk_layout "${DEVICE}" 2>/dev/null; then
    ROOT_PARTNUM="${_DL_ROOT_PARTNUM}"
    log "  Root partition auto-detected: partition ${ROOT_PARTNUM}"
  fi
fi

if [[ -n "${ROOT_PARTNUM}" ]]; then
  ROOT_PART=$(partition_path "${DEVICE}" "${ROOT_PARTNUM}")
  if command -v e2fsck >/dev/null 2>&1 && [[ -b "${ROOT_PART}" ]]; then
    log "  Running quick fsck on rootfs (${ROOT_PART})..."
    e2fsck -n "${ROOT_PART}" 2>&1 | tail -3 || true
  fi
else
  log "  Skipping rootfs fsck (root partition not identified)"
fi

# ─── Step 3b: Allwinner (sunxi) bootloader reinforcement ───
#
# The create-golden-image.sh script already stamps the bootloader into the
# raw .img file, so in most cases this step is a no-op. However, as a
# safety net, we re-write the bootloader from firmware files if available.
# This handles the case where the image was transferred or re-compressed by
# a third-party tool that might have altered raw sectors.

SUNXI_FROM_MANIFEST="false"
if [[ -n "${MANIFEST_PATH}" ]] && command -v jq >/dev/null 2>&1; then
  SUNXI_FROM_MANIFEST=$(jq -r '.sunxi_bootloader // false' "${MANIFEST_PATH}" 2>/dev/null || echo "false")
fi

if [[ "${SUNXI_FROM_MANIFEST}" == "true" ]] || is_sunxi_platform 2>/dev/null || has_sunxi_bootloader "${DEVICE}" 2>/dev/null; then
  log ""
  log "  Allwinner (sunxi) platform detected — reinforcing bootloader on ${DEVICE}..."

  local_uboot_dir=$(find_sunxi_uboot_dir 2>/dev/null || true)
  if [[ -n "${local_uboot_dir}" ]]; then
    write_sunxi_bootloader "${DEVICE}" "${local_uboot_dir}"
    sync
  else
    # The image already has the bootloader stamped by create-golden-image.sh.
    # Verify by checking for the eGON magic at sector 16.
    egon_magic=$(dd if="${DEVICE}" bs=512 skip=16 count=1 status=none 2>/dev/null | head -c 12 | strings 2>/dev/null || true)
    if echo "${egon_magic}" | grep -q "eGON.BT0"; then
      log "  Bootloader already present in image (eGON.BT0 verified at sector 16)"
    else
      warn "No Allwinner firmware files found and no bootloader detected in image!"
      warn "The device may not boot. Ensure boot0_sdcard.fex and boot_package.fex are available."
    fi
  fi
fi

log ""
log "=========================================="
log "Flash Complete"
log "=========================================="
log ""
log "Next steps:"
log "  1. Remove SD card (if booting from SD)"
log "  2. Power on the device"
log "  3. Wait ~60s for first-boot initialization"
log "  4. Connect to AP hotspot (NG-Gateway-XXXX)"
log "  5. Run verify-image.sh for QA validation"
log ""
