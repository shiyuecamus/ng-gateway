#!/usr/bin/env bash
set -euo pipefail

# create-golden-image.sh
#
# Create a minimal, compressed eMMC golden image from a fully configured
# NG Gateway device. The resulting image is safe to flash onto any eMMC
# capacity (32GB / 64GB / 256GB) because the rootfs partition is shrunk
# to its minimum size. The first-boot service will expand it at runtime.
#
# Supports both single-partition (e.g. Orange Pi 4 Pro, MBR) and
# dual-partition (e.g. Orange Pi 5 Plus, GPT boot+rootfs) layouts.
# When partition numbers are not explicitly provided, the script
# auto-detects the disk layout.
#
# Usage:
#   sudo bash create-golden-image.sh --device /dev/mmcblk0 --output /mnt/usb/ng-gateway-v1.0.0 --version v1.0.0
#   sudo bash create-golden-image.sh --device /dev/mmcblk1 --output - --version v1.0.0 | ssh server "cat > image.img.zst"
#
# Prerequisites:
#   - Must run from an SD-card-booted system (eMMC must be fully unmounted)
#   - Required tools: parted, partprobe, resize2fs, e2fsck, dumpe2fs,
#                     sfdisk, dd, jq, blkid, findmnt
#   - GPT disks additionally require: sgdisk

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[create-image]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../shared/_common.sh"

# ─── Argument Parsing ───

DEVICE=""
OUTPUT=""
VERSION="unknown"
COMPRESSION="zstd"
USER_ROOT_PARTNUM=""
USER_BOOT_PARTNUM=""
BUFFER_MB=64

usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Options:
  --device DEVICE       Source eMMC block device (e.g. /dev/mmcblk0)
  --output PATH         Output image path (without .zst extension) or '-' for stdout
  --version VERSION     Image version string (e.g. v1.0.0)
  --root-partnum N      Root partition number (auto-detected if omitted)
  --boot-partnum N      Boot partition number (auto-detected if omitted; may not exist)
  --buffer-mb N         Extra buffer in MB after shrunk rootfs (default: 64)
  --compression ALGO    Compression algorithm: zstd, gzip, none (default: zstd)
  -h, --help            Show this help

Examples:
  # Auto-detect layout (recommended):
  sudo ${SCRIPT_NAME} --device /dev/mmcblk0 --output /mnt/usb/ng-gateway --version v1.0.0

  # Explicit partition numbers (override auto-detection):
  sudo ${SCRIPT_NAME} --device /dev/mmcblk1 --root-partnum 2 --boot-partnum 1 --output /mnt/usb/ng-gateway --version v1.0.0
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --device)       DEVICE="$2";            shift 2 ;;
    --output)       OUTPUT="$2";            shift 2 ;;
    --version)      VERSION="$2";           shift 2 ;;
    --root-partnum) USER_ROOT_PARTNUM="$2"; shift 2 ;;
    --boot-partnum) USER_BOOT_PARTNUM="$2"; shift 2 ;;
    --buffer-mb)    BUFFER_MB="$2";         shift 2 ;;
    --compression)  COMPRESSION="$2";       shift 2 ;;
    -h|--help)      usage ;;
    *)              die "Unknown option: $1" ;;
  esac
done

[[ -z "${DEVICE}" ]] && die "Missing --device"
[[ -z "${OUTPUT}" ]] && die "Missing --output"

# ─── Validate Environment ───

require_root
require_commands parted partprobe resize2fs e2fsck dumpe2fs sfdisk dd jq blkid findmnt sha256sum

case "${COMPRESSION}" in
  zstd) require_commands zstd; COMP_EXT=".zst" ;;
  gzip) require_commands gzip; COMP_EXT=".gz" ;;
  none) COMP_EXT="" ;;
  *)    die "Unsupported compression: ${COMPRESSION}" ;;
esac

# ─── Detect / Validate Source Device Layout ───

[[ -b "${DEVICE}" ]] || die "Not a block device: ${DEVICE}"

detect_disk_layout "${DEVICE}" || die "Cannot detect disk layout on ${DEVICE}"

DISK_LABEL="${_DL_LABEL}"

if [[ -n "${USER_ROOT_PARTNUM}" ]]; then
  ROOT_PARTNUM="${USER_ROOT_PARTNUM}"
  ROOT_PART=$(partition_path "${DEVICE}" "${ROOT_PARTNUM}")
  ROOT_FS=$(blkid -o value -s TYPE "${ROOT_PART}" 2>/dev/null || echo "ext4")
  log "Using user-specified root partition: ${ROOT_PART}"
else
  ROOT_PARTNUM="${_DL_ROOT_PARTNUM}"
  ROOT_PART="${_DL_ROOT_PART}"
  ROOT_FS="${_DL_ROOT_FS}"
fi

BOOT_SEPARATE="${_DL_BOOT_SEPARATE}"
BOOT_PARTNUM="${_DL_BOOT_PARTNUM}"
BOOT_PART="${_DL_BOOT_PART:-}"
BOOT_FS="${_DL_BOOT_FS:-}"

if [[ -n "${USER_BOOT_PARTNUM}" ]]; then
  BOOT_PARTNUM="${USER_BOOT_PARTNUM}"
  BOOT_PART=$(partition_path "${DEVICE}" "${BOOT_PARTNUM}")
  BOOT_FS=$(blkid -o value -s TYPE "${BOOT_PART}" 2>/dev/null || echo "vfat")
  BOOT_SEPARATE="true"
  log "Using user-specified boot partition: ${BOOT_PART}"
fi

[[ -b "${ROOT_PART}" ]] || die "Root partition not found: ${ROOT_PART}"
if [[ "${BOOT_SEPARATE}" == "true" ]] && [[ -n "${BOOT_PART}" ]]; then
  [[ -b "${BOOT_PART}" ]] || die "Boot partition not found: ${BOOT_PART}"
fi

# Ensure source partitions are not mounted.
if findmnt "${ROOT_PART}" >/dev/null 2>&1; then
  die "Root partition ${ROOT_PART} is mounted. Boot from SD card and ensure eMMC is fully unmounted."
fi
if [[ "${BOOT_SEPARATE}" == "true" ]] && [[ -n "${BOOT_PART}" ]]; then
  if findmnt "${BOOT_PART}" >/dev/null 2>&1; then
    die "Boot partition ${BOOT_PART} is mounted. Ensure eMMC is fully unmounted."
  fi
fi

# ─── Detect Allwinner (sunxi) bootloader overlap ───
#
# On Allwinner SoCs, the boot_package (U-Boot) is written at a fixed raw
# offset (sector 32800 ≈ 16MB) that physically overlaps with the rootfs
# partition when the partition starts at sector 8192 (4MB). Shrinking the
# ext4 filesystem (resize2fs -M) may relocate data blocks into that region
# and destroy the bootloader.
#
# Strategy: before any filesystem modifications, back up the raw bootloader
# sectors. After the dd export, stamp the bootloader back onto the .img file.

SUNXI_BOOTLOADER="false"
SUNXI_BL_BACKUP=""
SUNXI_UBOOT_DIR=""

if is_sunxi_platform || has_sunxi_bootloader "${DEVICE}"; then
  log "Allwinner (sunxi) platform detected — bootloader overlap protection enabled"

  SUNXI_BOOTLOADER="true"
  SUNXI_BL_BACKUP=$(mktemp -d)

  # Back up the bootloader raw sectors BEFORE any filesystem operations.
  backup_sunxi_bootloader "${DEVICE}" "${SUNXI_BL_BACKUP}"

  # Also try to locate firmware files for flash-image.sh use.
  # On a live system these are in /usr/lib/linux-u-boot-*; when running from
  # an SD maintenance system we may need to mount the eMMC rootfs briefly.
  SUNXI_UBOOT_DIR=$(find_sunxi_uboot_dir 2>/dev/null || true)
  if [[ -n "${SUNXI_UBOOT_DIR}" ]]; then
    log "  Firmware files found: ${SUNXI_UBOOT_DIR}"
  else
    log "  No firmware files on this system — will use raw backup instead"
  fi
fi

log "=========================================="
log "NG Gateway Golden Image Creator"
log "=========================================="
log "Source device:  ${DEVICE}"
print_disk_layout
if [[ "${SUNXI_BOOTLOADER}" == "true" ]]; then
  log "Platform:       Allwinner (sunxi) — bootloader overlap protection active"
fi
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

if [[ "${DISK_LABEL}" == "gpt" ]]; then
  GPT_BACKUP_SECTORS=34
else
  GPT_BACKUP_SECTORS=0
fi

TOTAL_SECTORS=$((LAST_SECTOR + GPT_BACKUP_SECTORS))
TOTAL_BYTES=$((TOTAL_SECTORS * SECTOR_SIZE))
TOTAL_MB=$((TOTAL_BYTES / 1048576))

log "  Last data sector:     ${LAST_SECTOR}"
if [[ ${GPT_BACKUP_SECTORS} -gt 0 ]]; then
  log "  GPT backup:           +${GPT_BACKUP_SECTORS} sectors"
else
  log "  MBR layout:           no GPT backup sectors needed"
fi
log "  Total sectors to copy: ${TOTAL_SECTORS}"
log "  Total image size:     ${TOTAL_MB} MB (before compression)"

# ─── Step 5: Fix GPT backup before export (GPT only) ───

if [[ "${DISK_LABEL}" == "gpt" ]]; then
  log "Step 5/6: Repairing GPT backup header before export..."
  require_commands sgdisk
  sgdisk -e "${DEVICE}" >/dev/null 2>&1 || die "Failed to relocate GPT backup header with sgdisk -e"
  partprobe "${DEVICE}" 2>/dev/null || true
  sleep 1
  log "  GPT backup header relocated to current disk end"
else
  log "Step 5/6: Skipping GPT repair (disk uses ${DISK_LABEL} partition table)"
fi

# ─── Step 6: Export image ───

log "Step 6/6: Exporting image..."

RAW_SHA256_FILE=$(mktemp)

if [[ "${OUTPUT}" == "-" ]]; then
  dd if="${DEVICE}" bs=1M count="${TOTAL_BYTES}" iflag=count_bytes status=progress 2>/dev/null \
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

  log "  6a) Writing raw image: ${IMG_PATH}"
  dd if="${DEVICE}" bs=1M count="${TOTAL_BYTES}" iflag=count_bytes status=progress \
    of="${IMG_PATH}" 2>/dev/null

  # 6a-sunxi) Stamp Allwinner bootloader back into the raw image.
  # The resize2fs -M in Step 2 may have relocated ext4 data blocks into
  # the raw sectors where boot0 and boot_package reside. We restore the
  # bootloader from the pre-shrink backup to guarantee a bootable image.
  if [[ "${SUNXI_BOOTLOADER}" == "true" ]] && [[ -n "${SUNXI_BL_BACKUP}" ]]; then
    log "  6a-sunxi) Restoring Allwinner bootloader into raw image..."
    restore_sunxi_bootloader "${IMG_PATH}" "${SUNXI_BL_BACKUP}"
  fi

  RAW_SHA256=$(sha256sum "${IMG_PATH}" | awk '{print $1}')
  echo "${RAW_SHA256}  $(basename "${IMG_PATH}")" > "${IMG_PATH}.sha256"
  RAW_BYTES=$(stat -c%s "${IMG_PATH}" 2>/dev/null || stat -f%z "${IMG_PATH}" 2>/dev/null)
  log "  Raw image: ${IMG_PATH} (${TOTAL_MB} MB)"
  log "  SHA256:    ${IMG_PATH}.sha256"

  COMP_SHA256=""
  COMP_BYTES=""
  if [[ "${COMPRESSION}" != "none" ]]; then
    log "  6b) Compressing: ${COMP_PATH}"
    case "${COMPRESSION}" in
      zstd) zstd -T0 -3 "${IMG_PATH}" -o "${COMP_PATH}" 2>/dev/null ;;
      gzip) gzip -c "${IMG_PATH}" > "${COMP_PATH}" ;;
    esac
    COMP_SHA256=$(sha256sum "${COMP_PATH}" | awk '{print $1}')
    COMP_BYTES=$(stat -c%s "${COMP_PATH}" 2>/dev/null || stat -f%z "${COMP_PATH}" 2>/dev/null)
    echo "${COMP_SHA256}  $(basename "${COMP_PATH}")" > "${COMP_PATH}.sha256"
    log "  Compressed: ${COMP_PATH} ($(( ${COMP_BYTES} / 1048576 )) MB)"
    log "  SHA256:     ${COMP_PATH}.sha256"
  fi

  # Step 6c: Generate layout-aware manifest.
  log "  6c) Generating manifest..."

  KERNEL_VER=$(uname -r 2>/dev/null || echo "unknown")
  OS_INFO=$(lsb_release -ds 2>/dev/null || head -1 /etc/os-release 2>/dev/null || echo "unknown")
  ROOT_SIZE_MB="${TARGET_PART_MB}"

  # Build partition list dynamically based on detected layout.
  PARTITIONS_JSON="["
  if [[ "${BOOT_SEPARATE}" == "true" ]] && [[ -n "${BOOT_PARTNUM}" ]]; then
    BOOT_SIZE_MB=$(parted -ms "${DEVICE}" unit MB print 2>/dev/null \
      | awk -F: "/^${BOOT_PARTNUM}:/{gsub(/MB/,\"\",\$4); print \$4}" || echo "256")
    PARTITIONS_JSON+="{\"number\": ${BOOT_PARTNUM}, \"label\": \"boot\", \"fs\": \"${BOOT_FS}\", \"size_mb\": ${BOOT_SIZE_MB}}, "
  fi
  PARTITIONS_JSON+="{\"number\": ${ROOT_PARTNUM}, \"label\": \"rootfs\", \"fs\": \"${ROOT_FS}\", \"size_mb\": ${ROOT_SIZE_MB}}"
  PARTITIONS_JSON+="]"

  # Detect board model from device-tree if available, fall back to hostname.
  BOARD_MODEL=""
  if [[ -f /proc/device-tree/model ]]; then
    BOARD_MODEL=$(tr -d '\0' < /proc/device-tree/model 2>/dev/null || true)
  fi
  if [[ -f /sys/firmware/devicetree/base/model ]]; then
    BOARD_MODEL=$(tr -d '\0' < /sys/firmware/devicetree/base/model 2>/dev/null || true)
  fi
  [[ -z "${BOARD_MODEL}" ]] && BOARD_MODEL=$(hostname 2>/dev/null || echo "unknown")

  jq -n \
    --arg version "${VERSION}" \
    --arg created_at "$(date -Iseconds)" \
    --arg source_device "${DEVICE}" \
    --argjson source_sectors "${TOTAL_SECTORS}" \
    --argjson source_bytes "${TOTAL_BYTES}" \
    --arg sha256_raw "${RAW_SHA256}" \
    --arg compression "${COMPRESSION}" \
    --arg sha256_compressed "${COMP_SHA256:-}" \
    --argjson compressed_bytes "${COMP_BYTES:-0}" \
    --arg partition_table "${DISK_LABEL}" \
    --argjson root_partnum "${ROOT_PARTNUM}" \
    --argjson boot_separate "$(if [[ "${BOOT_SEPARATE}" == "true" ]]; then echo true; else echo false; fi)" \
    --argjson partitions "${PARTITIONS_JSON}" \
    --argjson sunxi_bootloader "$(if [[ "${SUNXI_BOOTLOADER}" == "true" ]]; then echo true; else echo false; fi)" \
    --arg ng_gateway_version "${VERSION}" \
    --arg os "${OS_INFO}" \
    --arg kernel "${KERNEL_VER}" \
    --arg board "${BOARD_MODEL}" \
    '{
      version: $version,
      created_at: $created_at,
      source_device: $source_device,
      source_sectors: $source_sectors,
      source_bytes: $source_bytes,
      sha256_raw: $sha256_raw,
      compression: $compression,
      sha256_compressed: $sha256_compressed,
      compressed_bytes: $compressed_bytes,
      partition_table: $partition_table,
      root_partnum: $root_partnum,
      boot_separate: $boot_separate,
      partitions: $partitions,
      sunxi_bootloader: $sunxi_bootloader,
      ng_gateway_version: $ng_gateway_version,
      os: $os,
      kernel: $kernel,
      board: $board
    }' > "${MANIFEST_PATH}"

  log "  Manifest: ${MANIFEST_PATH}"
fi

rm -f "${RAW_SHA256_FILE}"
if [[ -n "${SUNXI_BL_BACKUP:-}" ]]; then
  rm -rf "${SUNXI_BL_BACKUP}"
fi

log ""
log "=========================================="
log "Golden Image Creation Complete"
log "=========================================="
log ""
if [[ "${OUTPUT}" != "-" ]]; then
  log "Artifacts:"
  log "  Raw image:  ${IMG_PATH} (${TOTAL_MB} MB)"
  [[ -n "${COMP_BYTES:-}" ]] && \
  log "  Compressed: ${COMP_PATH} ($(( ${COMP_BYTES} / 1048576 )) MB)"
  log "  Manifest:   ${MANIFEST_PATH}"
  if [[ "${SUNXI_BOOTLOADER}" == "true" ]]; then
    log ""
    log "  Platform:   Allwinner (sunxi) — bootloader embedded in image"
  fi
  log ""
  log "For RKDevTool / Windows mass-production flashing, use the raw .img file."
  log "For archival or network transfer, use the compressed .img${COMP_EXT} file."
fi
log ""
log "IMPORTANT: The rootfs partition in this image has been shrunk."
log "Do NOT resize it back on the golden sample. If you need to boot"
log "the golden sample again, run:"
log "  sudo resize2fs ${ROOT_PART}"
log ""
