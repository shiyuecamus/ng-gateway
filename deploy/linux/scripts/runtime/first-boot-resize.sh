#!/usr/bin/env bash
set -euo pipefail

# first-boot-resize.sh
#
# One-time first-boot initialization for factory-flashed NG Gateway devices.
#
# This script runs once on the very first boot after a golden image has been
# flashed to eMMC. It performs:
#   1. Root partition expansion  (growpart + resize2fs)
#   2. GPT backup header repair  (sgdisk -e)
#   3. Machine identity regeneration  (machine-id, SSH host keys)
#   4. AP hotspot re-initialization  (MAC-specific SSID)
#   5. Marker file creation to prevent re-execution
#
# Called by ng-gateway-first-boot.service after local filesystems are fully
# mounted/remounted and before SSH / NG Gateway services start. Idempotent —
# safe to re-run, but will skip if marker file exists.
#
# Required tools: growpart (cloud-guest-utils), resize2fs, sgdisk, ssh-keygen

MARKER_FILE="/var/lib/ng-gateway/.first-boot-done"
OPT_DIR="/opt/ng-gateway"
LOG_TAG="[first-boot]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "${SCRIPT_DIR}/_common.sh" ]]; then
  source "${SCRIPT_DIR}/_common.sh"
else
  source "${SCRIPT_DIR}/../shared/_common.sh"
fi

# ─── Guard: skip if already completed ───

if [[ -f "${MARKER_FILE}" ]]; then
  log "First-boot already completed (marker exists: ${MARKER_FILE}). Skipping."
  exit 0
fi

log "=========================================="
log "NG Gateway First-Boot Initialization"
log "=========================================="

require_root

# Ensure the root filesystem is writable before mutating identity files.
ensure_root_writable() {
  local probe_file="/etc/.ng-gateway-rw-test.$$"
  local attempt=0

  for attempt in $(seq 1 10); do
    if touch "${probe_file}" 2>/dev/null; then
      rm -f "${probe_file}" 2>/dev/null || true
      if [[ ${attempt} -gt 1 ]]; then
        log "  Root filesystem is writable after ${attempt} attempt(s)"
      fi
      return 0
    fi

    if [[ ${attempt} -eq 1 ]]; then
      warn "Root filesystem appears read-only; retrying remount as read-write..."
    else
      warn "Root filesystem still not writable (attempt ${attempt}/10); retrying..."
    fi

    mount -o remount,rw / 2>/dev/null || true
    sync
    sleep 1
  done

  local mount_info="unknown"
  if command -v findmnt >/dev/null 2>&1; then
    mount_info=$(findmnt -n -o SOURCE,TARGET,FSTYPE,OPTIONS / 2>/dev/null || echo "unknown")
  fi

  die "Root filesystem is still not writable after retries (${mount_info})"
}

# Ensure a specific file path can be opened for write after online resize.
ensure_file_writable() {
  local file_path="$1"
  local file_label="${2:-$1}"
  local attempt=0

  for attempt in $(seq 1 10); do
    ensure_root_writable

    if : >> "${file_path}" 2>/dev/null; then
      if [[ ${attempt} -gt 1 ]]; then
        log "  ${file_label} is writable after ${attempt} attempt(s)"
      fi
      return 0
    fi

    warn "${file_label} is not writable yet (attempt ${attempt}/10); waiting for filesystem to settle..."
    mount -o remount,rw / 2>/dev/null || true
    sync
    sleep 1
  done

  local mount_info="unknown"
  if command -v findmnt >/dev/null 2>&1; then
    mount_info=$(findmnt -n -o SOURCE,TARGET,FSTYPE,OPTIONS / 2>/dev/null || echo "unknown")
  fi

  die "${file_label} is still not writable after retries (${mount_info})"
}

# ─── Step 1: Identify root device and partition ───

log "Step 1/7: Identifying root filesystem device..."

ROOT_DEV=""

if command -v findmnt >/dev/null 2>&1; then
  ROOT_DEV=$(findmnt -n -o SOURCE / 2>/dev/null | head -1) || true
fi

if [[ -z "${ROOT_DEV}" ]]; then
  ROOT_DEV=$(grep -oP 'root=\K\S+' /proc/cmdline 2>/dev/null || true)
  if [[ "${ROOT_DEV}" == UUID=* ]]; then
    uuid="${ROOT_DEV#UUID=}"
    ROOT_DEV=$(blkid -U "$uuid" 2>/dev/null || true)
  fi
fi

[[ -z "${ROOT_DEV}" ]] && die "Cannot identify root device"

parse_block_device "${ROOT_DEV}" || die "Cannot parse root device: ${ROOT_DEV}"
ROOT_DISK="${_PBD_DISK}"
ROOT_PARTNUM="${_PBD_PARTNUM}"

log "  Root device:    ${ROOT_DEV}"
log "  Disk device:    ${ROOT_DISK}"
log "  Partition:      ${ROOT_PARTNUM}"

# ─── Step 2: Repair GPT backup header ───

log "Step 2/7: Repairing GPT backup header..."

if command -v sgdisk >/dev/null 2>&1; then
  sgdisk -e "${ROOT_DISK}" 2>/dev/null || {
    warn "sgdisk -e failed (non-fatal, may already be correct)"
  }
  log "  GPT backup header repaired"
else
  warn "sgdisk not found — skipping GPT repair (install gdisk package)"
fi

# ─── Step 3: Expand root partition ───

log "Step 3/7: Expanding root partition to fill disk..."

if command -v growpart >/dev/null 2>&1; then
  growpart "${ROOT_DISK}" "${ROOT_PARTNUM}" 2>&1 || {
    rc=$?
    if [[ $rc -eq 1 ]]; then
      log "  Partition already at maximum size (NOCHANGE)"
    else
      die "growpart failed with exit code ${rc}"
    fi
  }
  log "  Partition expanded"
else
  die "growpart not found — install cloud-guest-utils"
fi

partprobe "${ROOT_DISK}" 2>/dev/null || true
sleep 1

# ─── Step 4: Expand root filesystem ───

log "Step 4/7: Expanding ext4 filesystem..."

if [[ $(blkid -o value -s TYPE "${ROOT_DEV}" 2>/dev/null) == "ext4" ]]; then
  resize2fs "${ROOT_DEV}" 2>&1 || {
    die "resize2fs failed — filesystem may need manual repair"
  }
  log "  Filesystem expanded"
  log "  Waiting for filesystem state to settle after online resize..."
  sync
  if command -v udevadm >/dev/null 2>&1; then
    udevadm settle 2>/dev/null || true
  fi
  sleep 1
  NEW_SIZE=$(df -h "${ROOT_DEV}" 2>/dev/null | awk 'NR==2{print $2}') || true
  log "  New rootfs size: ${NEW_SIZE}"
else
  die "Root filesystem is not ext4 — unsupported first-boot resize target"
fi

# ─── Step 5: Regenerate machine identity ───

log "Step 5/7: Regenerating machine identity..."

ensure_root_writable
ensure_file_writable /etc/machine-id "/etc/machine-id"

if [[ -f /etc/machine-id ]]; then
  truncate -s 0 /etc/machine-id
  if command -v systemd-machine-id-setup >/dev/null 2>&1; then
    systemd-machine-id-setup 2>/dev/null || die "systemd-machine-id-setup failed"
  fi
  [[ -s /etc/machine-id ]] || die "machine-id regeneration failed"
  log "  machine-id: $(cat /etc/machine-id 2>/dev/null || echo '(unavailable)')"
fi

rm -f /var/lib/dbus/machine-id 2>/dev/null || true
if [[ -f /etc/machine-id ]] && [[ -s /etc/machine-id ]]; then
  ln -sf /etc/machine-id /var/lib/dbus/machine-id 2>/dev/null || true
fi

# ─── Step 6: Regenerate SSH host keys ───

log "Step 6/7: Regenerating SSH host keys..."

rm -f /etc/ssh/ssh_host_* 2>/dev/null || true

if command -v ssh-keygen >/dev/null 2>&1; then
  ssh-keygen -A 2>/dev/null || die "ssh-keygen -A failed"
  generated_ssh_key_count=$(ls /etc/ssh/ssh_host_*_key 2>/dev/null | wc -l)
  [[ ${generated_ssh_key_count} -ge 2 ]] || die "SSH host key regeneration failed"
  log "  SSH host keys regenerated (${generated_ssh_key_count} key pair(s))"
else
  die "ssh-keygen not found — cannot regenerate SSH host keys"
fi

# ─── Step 7: Re-initialize AP hotspot (MAC-specific SSID) ───

log "Step 7/7: Re-initializing AP hotspot configuration..."

init_script="${OPT_DIR}/scripts/init-network.sh"
if [[ -f "${init_script}" ]]; then
  FORCE_REGENERATE=1 DEFER_SERVICE_ACTIVATION=1 bash "${init_script}" || {
    warn "AP re-initialization had issues (non-fatal)"
  }
else
  warn "init-network.sh not found — AP config may use golden sample's SSID"
fi

# ─── Finalize ───

MACHINE_ID=$(cat /etc/machine-id 2>/dev/null || echo "unknown")
SSH_KEY_COUNT=$(ls /etc/ssh/ssh_host_*_key 2>/dev/null | wc -l)
ROOT_SIZE=$(df -h "${ROOT_DEV}" 2>/dev/null | awk 'NR==2{print $2}' || echo "unknown")

mkdir -p "$(dirname "${MARKER_FILE}")"
cat > "${MARKER_FILE}" <<MARKER_EOF
completed_at=$(date -Iseconds)
root_disk=${ROOT_DISK}
root_dev=${ROOT_DEV}
root_size=${ROOT_SIZE}
machine_id=${MACHINE_ID}
ssh_key_count=${SSH_KEY_COUNT}
MARKER_EOF
log "  Marker written: ${MARKER_FILE}"

log ""
log "=========================================="
log "First-Boot Initialization Complete"
log "=========================================="
log ""
log "Summary:"
log "  Root partition: expanded to ${ROOT_SIZE} on ${ROOT_DISK}"
log "  Machine ID:     ${MACHINE_ID}"
log "  SSH keys:       ${SSH_KEY_COUNT} key pair(s) generated"
log "  AP hotspot:     re-initialized with device-specific SSID"
log ""

exit 0
