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

ROOT_PART=$(partition_path "${DEVICE}" 2)
if command -v e2fsck >/dev/null 2>&1 && [[ -b "${ROOT_PART}" ]]; then
  log "  Running quick fsck on rootfs..."
  e2fsck -n "${ROOT_PART}" 2>&1 | tail -3 || true
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
