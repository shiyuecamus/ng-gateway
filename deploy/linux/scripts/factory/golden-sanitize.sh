#!/usr/bin/env bash
set -euo pipefail

# golden-sanitize.sh
#
# Prepare a validated NG Gateway device for golden-image capture.
# The script removes machine-specific identity and transient factory data so the
# exported image can be safely replicated across production devices.
#
# Default behavior targets an "empty factory template" image:
#   - remove machine-id and SSH host keys
#   - remove gateway runtime database / logs / certificates
#   - remove NetworkManager saved Wi-Fi profiles
#   - clear first-boot marker so cloned devices re-run initialization
#
# Optional flags:
#   --keep-db    Preserve /var/lib/ng-gateway/ng-gateway.db* for project-template images
#   --poweroff   Power off the device after cleanup completes

SCRIPT_NAME="$(basename "$0")"
LOG_TAG="[golden-sanitize]"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "${SCRIPT_DIR}/../shared/_common.sh"

KEEP_DB=0
POWER_OFF=0

usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Options:
  --keep-db     Preserve ng-gateway database files for project-template images
  --poweroff    Power off the device after cleanup completes
  -h, --help    Show this help

Examples:
  sudo ${SCRIPT_NAME}
  sudo ${SCRIPT_NAME} --keep-db
  sudo ${SCRIPT_NAME} --poweroff
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --keep-db)  KEEP_DB=1; shift ;;
    --poweroff) POWER_OFF=1; shift ;;
    -h|--help)  usage ;;
    *)          die "Unknown option: $1" ;;
  esac
done

require_root

log "=========================================="
log "NG Gateway Golden Sample Sanitizer"
log "=========================================="
if [[ ${KEEP_DB} -eq 1 ]]; then
  log "Profile: project-template (database preserved)"
else
  log "Profile: empty-template (database removed)"
fi
log "=========================================="

# Stop gateway-managed services so files are quiescent before cleanup.
for unit in \
  ng-gateway.service \
  ng-gateway-hostapd.service \
  ng-gateway-dnsmasq.service \
  ng-gateway-ap-setup.service \
  ng-gateway-ap-auto.service; do
  if command -v systemctl >/dev/null 2>&1; then
    systemctl stop "${unit}" 2>/dev/null || true
  fi
done

log "Removing machine identity..."
truncate -s 0 /etc/machine-id
rm -f /var/lib/dbus/machine-id

log "Removing SSH host keys..."
rm -f /etc/ssh/ssh_host_*

log "Cleaning NG Gateway runtime state..."
if [[ ${KEEP_DB} -eq 0 ]]; then
  rm -f /var/lib/ng-gateway/ng-gateway.db
  rm -f /var/lib/ng-gateway/ng-gateway.db-wal
  rm -f /var/lib/ng-gateway/ng-gateway.db-shm
else
  log "  Keeping database files as requested"
fi
rm -rf /var/lib/ng-gateway/logs/*
rm -rf /var/lib/ng-gateway/certs/*

log "Removing saved NetworkManager Wi-Fi profiles..."
for profile in /etc/NetworkManager/system-connections/*.nmconnection; do
  [[ -e "${profile}" ]] || continue
  if grep -Eq '(^type=wifi$|^\[wifi\]$)' "${profile}" 2>/dev/null; then
    rm -f "${profile}"
  fi
done

log "Clearing first-boot completion marker..."
rm -f /var/lib/ng-gateway/.first-boot-done

if command -v apt-get >/dev/null 2>&1; then
  log "Cleaning APT caches..."
  apt-get clean
  rm -rf /var/cache/apt/archives/*.deb
fi

if command -v journalctl >/dev/null 2>&1; then
  log "Vacuuming system journal..."
  journalctl --rotate || true
  journalctl --vacuum-time=1s || true
fi

log "Cleaning plain-text log files..."
rm -rf /var/log/*.gz /var/log/*.old /var/log/*.1
truncate -s 0 /var/log/syslog 2>/dev/null || true
truncate -s 0 /var/log/kern.log 2>/dev/null || true

log "Clearing shell history..."
truncate -s 0 /root/.bash_history 2>/dev/null || true

if [[ -n "${SUDO_USER:-}" ]] && [[ "${SUDO_USER}" != "root" ]]; then
  user_home="$(getent passwd "${SUDO_USER}" | cut -d: -f6 || true)"
  if [[ -n "${user_home}" ]]; then
    truncate -s 0 "${user_home}/.bash_history" 2>/dev/null || true
  fi
fi

log "Syncing filesystem buffers..."
sync
sync
sync

log ""
log "Golden sample cleanup complete."
log "Do not boot from the eMMC again before creating the image."
log "Boot from SD card and run create-golden-image.sh next."

if [[ ${POWER_OFF} -eq 1 ]]; then
  log "Powering off..."
  poweroff
fi
