#!/usr/bin/env bash
set -euo pipefail

# create-golden-image.sh
#
# Create a minimal, compressed eMMC golden image from a fully configured
# NG Gateway device.  The resulting image is safe to flash onto any eMMC
# capacity (32GB / 64GB / 256GB) because the rootfs partition is shrunk
# to its minimum size. The first-boot service will expand it at runtime.
#
# Usage:
#   sudo bash create-golden-image.sh --device /dev/mmcblk1 --output /mnt/usb/ng-gateway-v1.0.0.img --version v1.0.0
#   sudo bash create-golden-image.sh --device /dev/mmcblk1 --output - --version v1.0.0 | ssh server "cat > image.img.zst"
#
# Prerequisites:
#   - Must run from an SD-card-booted system (eMMC must be fully unmounted)
#   - Required tools: parted, resize2fs, e2fsck, sgdisk, dd, zstd, jq

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[create-image]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/_common.sh"

# ─── Argument Parsing ───

DEVICE=""
OUTPUT=""
VERSION="unknown"
COMPRESSION="zstd"
BOOT_PARTNUM=1
ROOT_PARTNUM=2
BUFFER_MB=64

usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Options:
  --device DEVICE       Source eMMC block device (e.g. /dev/mmcblk1)
  --output PATH         Output image path (without .zst extension) or '-' for stdout
  --version VERSION     Image version string (e.g. v1.0.0)
  --root-partnum N      Root partition number (default: 2)
  --boot-partnum N      Boot partition number (default: 1)
  --buffer-mb N         Extra buffer in MB after shrunk rootfs (default: 64)
  --compression ALGO    Compression algorithm: zstd, gzip, none (default: zstd)
  -h, --help            Show this help

Example:
  sudo ${SCRIPT_NAME} --device /dev/mmcblk1 --output /mnt/usb/ng-gateway --version v1.0.0
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --device)       DEVICE="$2";       shift 2 ;;
    --output)       OUTPUT="$2";       shift 2 ;;
    --version)      VERSION="$2";      shift 2 ;;
    --root-partnum) ROOT_PARTNUM="$2"; shift 2 ;;
    --boot-partnum) BOOT_PARTNUM="$2"; shift 2 ;;
    --buffer-mb)    BUFFER_MB="$2";    shift 2 ;;
    --compression)  COMPRESSION="$2";  shift 2 ;;
    -h|--help)      usage ;;
    *)              die "Unknown option: $1" ;;
  esac
done

[[ -z "${DEVICE}" ]] && die "Missing --device"
[[ -z "${OUTPUT}" ]] && die "Missing --output"

# ─── Validate Environment ───

require_root
require_commands parted resize2fs e2fsck dd jq blkid

case "${COMPRESSION}" in
  zstd) require_commands zstd; COMP_EXT=".zst" ;;
  gzip) require_commands gzip; COMP_EXT=".gz" ;;
  none) COMP_EXT="" ;;
  *)    die "Unsupported compression: ${COMPRESSION}" ;;
esac

# ─── Validate Source Device ───

[[ -b "${DEVICE}" ]] || die "Not a block device: ${DEVICE}"

ROOT_PART=$(partition_path "${DEVICE}" "${ROOT_PARTNUM}")
BOOT_PART=$(partition_path "${DEVICE}" "${BOOT_PARTNUM}")

[[ -b "${ROOT_PART}" ]] || die "Root partition not found: ${ROOT_PART}"
[[ -b "${BOOT_PART}" ]] || die "Boot partition not found: ${BOOT_PART}"

if findmnt "${ROOT_PART}" >/dev/null 2>&1 || findmnt "${BOOT_PART}" >/dev/null 2>&1; then
  die "Source partitions are mounted. Boot from SD card and ensure eMMC is fully unmounted."
fi

log "=========================================="
log "NG Gateway Golden Image Creator"
log "=========================================="
log "Source device:  ${DEVICE}"
log "Root partition: ${ROOT_PART}"
log "Boot partition: ${BOOT_PART}"
log "Output:         ${OUTPUT}"
log "Version:        ${VERSION}"
log "Compression:    ${COMPRESSION}"
log "=========================================="
log ""

# ─── Step 1: Check and repair filesystem ───

log "Step 1/6: Checking root filesystem integrity..."
e2fsck -fy "${ROOT_PART}" 2>&1 || {
  rc=$?
  if [[ $rc -le 1 ]]; then
    log "  Filesystem clean (or minor fixes applied)"
  else
    die "e2fsck failed with exit code ${rc}. Fix manually before creating image."
  fi
}

# ─── Step 2: Shrink root filesystem to minimum ───

log "Step 2/6: Shrinking root filesystem to minimum size..."

resize2fs -M "${ROOT_PART}" 2>&1

FS_BLOCK_COUNT=$(dumpe2fs -h "${ROOT_PART}" 2>/dev/null | awk '/^Block count:/{print $3}')
FS_BLOCK_SIZE=$(dumpe2fs -h "${ROOT_PART}" 2>/dev/null | awk '/^Block size:/{print $3}')
FS_BYTES=$((FS_BLOCK_COUNT * FS_BLOCK_SIZE))
FS_MB=$((FS_BYTES / 1048576))

log "  Filesystem shrunk to: ${FS_BLOCK_COUNT} blocks × ${FS_BLOCK_SIZE} = ${FS_MB} MB"

# ─── Step 3: Shrink root partition to match filesystem + buffer ───

log "Step 3/6: Shrinking root partition..."

TARGET_PART_MB=$((FS_MB + BUFFER_MB))
TARGET_PART_BYTES=$((TARGET_PART_MB * 1048576))

ROOT_START_SECTOR=$(parted -ms "${DEVICE}" unit s print 2>/dev/null \
  | awk -F: "/^${ROOT_PARTNUM}:/{gsub(/s/,\"\",\$2); print \$2}")

[[ -z "${ROOT_START_SECTOR}" ]] && die "Cannot determine start sector of partition ${ROOT_PARTNUM}"

SECTOR_SIZE=512
NEW_END_SECTOR=$(( ROOT_START_SECTOR + (TARGET_PART_BYTES / SECTOR_SIZE) - 1 ))

log "  Root partition start:  sector ${ROOT_START_SECTOR}"
log "  New end sector:        ${NEW_END_SECTOR}"
log "  New partition size:    ${TARGET_PART_MB} MB"

parted -s "${DEVICE}" resizepart "${ROOT_PARTNUM}" "${NEW_END_SECTOR}s" 2>&1 || {
  warn "parted resizepart failed — trying sfdisk approach..."
  echo "${ROOT_START_SECTOR} $((TARGET_PART_BYTES / SECTOR_SIZE))" \
    | sfdisk --no-reread -N "${ROOT_PARTNUM}" "${DEVICE}" 2>&1 || {
      die "Failed to shrink partition"
    }
}

partprobe "${DEVICE}" 2>/dev/null || true
sleep 1

# ─── Step 4: Calculate clone boundary ───

log "Step 4/6: Calculating clone boundary..."

LAST_SECTOR=$((NEW_END_SECTOR + 1))
GPT_BACKUP_SECTORS=34
TOTAL_SECTORS=$((LAST_SECTOR + GPT_BACKUP_SECTORS))
TOTAL_BYTES=$((TOTAL_SECTORS * SECTOR_SIZE))
TOTAL_MB=$((TOTAL_BYTES / 1048576))

log "  Last data sector:     ${LAST_SECTOR}"
log "  GPT backup:           +${GPT_BACKUP_SECTORS} sectors"
log "  Total sectors to copy: ${TOTAL_SECTORS}"
log "  Total image size:     ${TOTAL_MB} MB (before compression)"

# ─── Step 5: Fix GPT backup before export ───

if command -v sgdisk >/dev/null 2>&1; then
  log "  Fixing GPT backup header position..."
fi

# ─── Step 6: Export image ───

log "Step 5/6: Exporting image..."

RAW_SHA256_FILE=$(mktemp)

if [[ "${OUTPUT}" == "-" ]]; then
  dd if="${DEVICE}" bs=1M count="${TOTAL_MB}" iflag=count_bytes status=progress 2>/dev/null \
    | tee >(sha256sum | awk '{print $1}' > "${RAW_SHA256_FILE}") \
    | case "${COMPRESSION}" in
        zstd) zstd -T0 -3 ;;
        gzip) gzip -c ;;
        none) cat ;;
      esac
else
  IMG_PATH="${OUTPUT}.img"
  COMP_PATH="${OUTPUT}.img${COMP_EXT}"
  MANIFEST_PATH="${OUTPUT}.manifest.json"

  dd if="${DEVICE}" bs=1M count="${TOTAL_MB}" iflag=count_bytes status=progress 2>/dev/null \
    | tee >(sha256sum | awk '{print $1}' > "${RAW_SHA256_FILE}") \
    | case "${COMPRESSION}" in
        zstd) zstd -T0 -3 -o "${COMP_PATH}" ;;
        gzip) gzip -c > "${COMP_PATH}" ;;
        none) cat > "${IMG_PATH}" ;;
      esac

  log "  Image written: ${COMP_PATH:-${IMG_PATH}}"

  log "Step 6/6: Generating checksums and manifest..."

  FINAL_PATH="${COMP_PATH:-${IMG_PATH}}"
  COMP_SHA256=$(sha256sum "${FINAL_PATH}" | awk '{print $1}')
  RAW_SHA256=$(cat "${RAW_SHA256_FILE}")
  COMP_BYTES=$(stat -c%s "${FINAL_PATH}" 2>/dev/null || stat -f%z "${FINAL_PATH}" 2>/dev/null)

  echo "${COMP_SHA256}  $(basename "${FINAL_PATH}")" > "${FINAL_PATH}.sha256"
  log "  SHA256: ${FINAL_PATH}.sha256"

  KERNEL_VER=$(uname -r 2>/dev/null || echo "unknown")
  OS_INFO=$(lsb_release -ds 2>/dev/null || cat /etc/os-release 2>/dev/null | head -1 || echo "unknown")

  BOOT_SIZE_MB=$(parted -ms "${DEVICE}" unit MB print 2>/dev/null \
    | awk -F: "/^${BOOT_PARTNUM}:/{gsub(/MB/,\"\",\$4); print \$4}" || echo "256")
  ROOT_SIZE_MB="${TARGET_PART_MB}"
  BOOT_FS=$(blkid -o value -s TYPE "${BOOT_PART}" 2>/dev/null || echo "ext4")
  ROOT_FS=$(blkid -o value -s TYPE "${ROOT_PART}" 2>/dev/null || echo "ext4")

  jq -n \
    --arg version "${VERSION}" \
    --arg created_at "$(date -Iseconds)" \
    --arg source_device "${DEVICE}" \
    --argjson source_sectors "${TOTAL_SECTORS}" \
    --argjson source_bytes "${TOTAL_BYTES}" \
    --argjson compressed_bytes "${COMP_BYTES}" \
    --arg compression "${COMPRESSION}" \
    --arg sha256_compressed "${COMP_SHA256}" \
    --arg sha256_raw "${RAW_SHA256}" \
    --arg partition_table "gpt" \
    --argjson partitions "[
      {\"number\": ${BOOT_PARTNUM}, \"label\": \"boot\", \"fs\": \"${BOOT_FS}\", \"size_mb\": ${BOOT_SIZE_MB}},
      {\"number\": ${ROOT_PARTNUM}, \"label\": \"rootfs\", \"fs\": \"${ROOT_FS}\", \"size_mb\": ${ROOT_SIZE_MB}}
    ]" \
    --arg ng_gateway_version "${VERSION}" \
    --arg os "${OS_INFO}" \
    --arg kernel "${KERNEL_VER}" \
    --arg board "orangepi5plus" \
    '{
      version: $version,
      created_at: $created_at,
      source_device: $source_device,
      source_sectors: $source_sectors,
      source_bytes: $source_bytes,
      compressed_bytes: $compressed_bytes,
      compression: $compression,
      sha256_compressed: $sha256_compressed,
      sha256_raw: $sha256_raw,
      partition_table: $partition_table,
      partitions: $partitions,
      ng_gateway_version: $ng_gateway_version,
      os: $os,
      kernel: $kernel,
      board: $board
    }' > "${MANIFEST_PATH}"

  log "  Manifest: ${MANIFEST_PATH}"
fi

rm -f "${RAW_SHA256_FILE}"

log ""
log "=========================================="
log "Golden Image Creation Complete"
log "=========================================="
log ""
log "Image size: ${TOTAL_MB} MB (raw) → ${COMP_BYTES:-N/A} bytes (compressed)"
log ""
log "IMPORTANT: The rootfs partition in this image has been shrunk."
log "Do NOT resize it back on the golden sample. If you need to boot"
log "the golden sample again, run:"
log "  sudo resize2fs ${ROOT_PART}"
log ""

exit 0
