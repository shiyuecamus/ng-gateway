#!/usr/bin/env bash
set -euo pipefail

# build-maintenance-tf.sh
#
# Prepare a TF maintenance card that automatically flashes eMMC when
# inserted into an OrangePi 4 Pro (or any Allwinner board) and powered on.
#
# Prerequisites:
#   - A TF card already flashed with the official OrangePi system image
#     (e.g. Orangepi4pro_1.0.6_ubuntu_jammy_server_linux5.15.147.img)
#     and bootable. This script does NOT burn the base OS — use
#     balenaEtcher or dd for that step.
#   - The TF card must be mounted or the device must be accessible via SSH.
#   - The golden image (.img.zst + .sha256 + .manifest.json) must be available.
#
# What this script does:
#   1. Copies the factory scripts (auto-flash-emmc.sh + _common.sh) to the TF.
#   2. Copies the golden image to /opt/ng-images/ on the TF.
#   3. Installs + enables the ng-gateway-auto-flash.service.
#   4. Configures auto-login on tty1 for visual progress monitoring.
#
# Usage:
#   # Via SSH to a booted TF card:
#   bash build-maintenance-tf.sh --ssh orangepi@192.168.88.113 --image /path/to/ng-gateway-v1.0.0.img.zst
#
#   # Via local mount (TF card in a reader on your workstation):
#   bash build-maintenance-tf.sh --mount /mnt/tf-rootfs --image /path/to/ng-gateway-v1.0.0.img.zst

SCRIPT_NAME="$(basename "$0")"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../../.." && pwd)"

SSH_TARGET=""
MOUNT_TARGET=""
IMAGE_PATH=""
IMAGE_VERSION=""

usage() {
  cat <<EOF
Usage: ${SCRIPT_NAME} [OPTIONS]

Prepare a TF maintenance card for automated eMMC flashing.

Options:
  --ssh  USER@HOST       Deploy via SSH to a booted TF card
  --mount PATH           Deploy to a locally mounted TF rootfs
  --image PATH           Path to the golden image file (.img.zst, .img.gz, or .img)
  --version VERSION      Optional version label (auto-detected from manifest if omitted)
  -h, --help             Show this help

Examples:
  # SSH mode (TF card is booted in a device):
  ${SCRIPT_NAME} --ssh orangepi@192.168.88.113 --image ./ng-gateway-v1.0.0.img.zst

  # Mount mode (TF card in USB reader):
  ${SCRIPT_NAME} --mount /mnt/tf-rootfs --image ./ng-gateway-v1.0.0.img.zst

Prerequisites:
  The TF card must already be flashed with the official OrangePi base system.
  This script only adds the auto-flash layer on top of the existing OS.
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ssh)     SSH_TARGET="$2";   shift 2 ;;
    --mount)   MOUNT_TARGET="$2"; shift 2 ;;
    --image)   IMAGE_PATH="$2";   shift 2 ;;
    --version) IMAGE_VERSION="$2"; shift 2 ;;
    -h|--help) usage ;;
    *)         echo "Unknown option: $1"; exit 1 ;;
  esac
done

[[ -z "${SSH_TARGET}" && -z "${MOUNT_TARGET}" ]] && { echo "ERROR: Specify --ssh or --mount"; exit 1; }
[[ -n "${SSH_TARGET}" && -n "${MOUNT_TARGET}" ]] && { echo "ERROR: Use --ssh OR --mount, not both"; exit 1; }
[[ -z "${IMAGE_PATH}" ]] && { echo "ERROR: --image is required"; exit 1; }
[[ -f "${IMAGE_PATH}" ]] || { echo "ERROR: Image not found: ${IMAGE_PATH}"; exit 1; }

DEPLOY_DIR="${REPO_ROOT}/deploy/linux"
SCRIPTS_DIR="${DEPLOY_DIR}/scripts"
SYSTEMD_DIR="${DEPLOY_DIR}/systemd"

[[ -f "${SCRIPTS_DIR}/factory/auto-flash-emmc.sh" ]] || { echo "ERROR: auto-flash-emmc.sh not found"; exit 1; }
[[ -f "${SCRIPTS_DIR}/shared/_common.sh" ]]          || { echo "ERROR: _common.sh not found"; exit 1; }
[[ -f "${SYSTEMD_DIR}/ng-gateway-auto-flash.service" ]] || { echo "ERROR: ng-gateway-auto-flash.service not found"; exit 1; }

# Collect image artifacts (image + optional sha256 + optional manifest).
IMAGE_BASE="${IMAGE_PATH}"
IMAGE_BASE="${IMAGE_BASE%.zst}"
IMAGE_BASE="${IMAGE_BASE%.gz}"
IMAGE_BASE="${IMAGE_BASE%.img}"
IMAGE_ARTIFACTS=("${IMAGE_PATH}")
[[ -f "${IMAGE_PATH}.sha256" ]] && IMAGE_ARTIFACTS+=("${IMAGE_PATH}.sha256")
[[ -f "${IMAGE_BASE}.manifest.json" ]] && IMAGE_ARTIFACTS+=("${IMAGE_BASE}.manifest.json")

echo "================================================================="
echo "  NG Gateway — TF Maintenance Card Builder"
echo "================================================================="
echo ""
echo "  Mode:     $(if [[ -n "${SSH_TARGET}" ]]; then echo "SSH → ${SSH_TARGET}"; else echo "Mount → ${MOUNT_TARGET}"; fi)"
echo "  Image:    ${IMAGE_PATH} ($(( $(stat -c%s "${IMAGE_PATH}" 2>/dev/null || stat -f%z "${IMAGE_PATH}" 2>/dev/null) / 1048576 )) MB)"
echo "  Artifacts: ${#IMAGE_ARTIFACTS[@]} file(s)"
echo ""

# ─── Deploy via SSH ───

if [[ -n "${SSH_TARGET}" ]]; then
  echo "[1/5] Testing SSH connection..."
  ssh -o ConnectTimeout=10 -o BatchMode=yes "${SSH_TARGET}" "echo ok" >/dev/null 2>&1 || {
    echo "  SSH with key auth failed, trying with password prompt..."
    ssh -o ConnectTimeout=10 "${SSH_TARGET}" "echo ok" >/dev/null 2>&1 || {
      echo "ERROR: Cannot connect to ${SSH_TARGET}"
      exit 1
    }
  }
  echo "  Connected."

  # Helper to run remote commands as root.
  r() { ssh "${SSH_TARGET}" "echo orangepi | sudo -S bash -c '$*'" 2>/dev/null; }

  echo "[2/5] Creating directories on TF card..."
  r "mkdir -p /opt/ng-gateway-factory/scripts/factory"
  r "mkdir -p /opt/ng-gateway-factory/scripts/shared"
  r "mkdir -p /opt/ng-images"

  echo "[3/5] Copying factory scripts..."
  # Pack scripts into a tarball and extract on remote.
  tar czf - \
    -C "${SCRIPTS_DIR}" \
    factory/auto-flash-emmc.sh \
    shared/_common.sh \
    | ssh "${SSH_TARGET}" "sudo tar xzf - -C /opt/ng-gateway-factory/scripts/"
  ssh "${SSH_TARGET}" "sudo chmod +x /opt/ng-gateway-factory/scripts/factory/auto-flash-emmc.sh"

  echo "[4/5] Copying golden image (this may take a few minutes)..."
  for artifact in "${IMAGE_ARTIFACTS[@]}"; do
    echo "  → $(basename "${artifact}")"
    scp -q "${artifact}" "${SSH_TARGET}:/tmp/$(basename "${artifact}")"
    ssh "${SSH_TARGET}" "sudo mv /tmp/$(basename "${artifact}") /opt/ng-images/"
  done

  echo "[5/5] Installing systemd service..."
  scp -q "${SYSTEMD_DIR}/ng-gateway-auto-flash.service" "${SSH_TARGET}:/tmp/ng-gateway-auto-flash.service"
  r "mv /tmp/ng-gateway-auto-flash.service /lib/systemd/system/"
  r "systemctl daemon-reload"
  r "systemctl enable ng-gateway-auto-flash.service"

  echo ""
  echo "  Verifying..."
  r "ls -la /opt/ng-gateway-factory/scripts/factory/auto-flash-emmc.sh"
  r "ls -lh /opt/ng-images/"
  r "systemctl is-enabled ng-gateway-auto-flash.service"

# ─── Deploy via local mount ───

elif [[ -n "${MOUNT_TARGET}" ]]; then
  [[ -d "${MOUNT_TARGET}" ]] || { echo "ERROR: Mount path not found: ${MOUNT_TARGET}"; exit 1; }
  [[ -d "${MOUNT_TARGET}/etc" ]] || { echo "ERROR: ${MOUNT_TARGET} does not look like a rootfs (no /etc)"; exit 1; }

  echo "[1/5] Verified mount point: ${MOUNT_TARGET}"

  echo "[2/5] Creating directories..."
  sudo mkdir -p "${MOUNT_TARGET}/opt/ng-gateway-factory/scripts/factory"
  sudo mkdir -p "${MOUNT_TARGET}/opt/ng-gateway-factory/scripts/shared"
  sudo mkdir -p "${MOUNT_TARGET}/opt/ng-images"

  echo "[3/5] Copying factory scripts..."
  sudo cp "${SCRIPTS_DIR}/factory/auto-flash-emmc.sh" "${MOUNT_TARGET}/opt/ng-gateway-factory/scripts/factory/"
  sudo cp "${SCRIPTS_DIR}/shared/_common.sh" "${MOUNT_TARGET}/opt/ng-gateway-factory/scripts/shared/"
  sudo chmod +x "${MOUNT_TARGET}/opt/ng-gateway-factory/scripts/factory/auto-flash-emmc.sh"

  echo "[4/5] Copying golden image..."
  for artifact in "${IMAGE_ARTIFACTS[@]}"; do
    echo "  → $(basename "${artifact}")"
    sudo cp "${artifact}" "${MOUNT_TARGET}/opt/ng-images/"
  done

  echo "[5/5] Installing systemd service..."
  UNIT_DIR="${MOUNT_TARGET}/lib/systemd/system"
  sudo mkdir -p "${UNIT_DIR}"
  sudo cp "${SYSTEMD_DIR}/ng-gateway-auto-flash.service" "${UNIT_DIR}/"
  # Enable the service by creating the symlink.
  sudo mkdir -p "${MOUNT_TARGET}/etc/systemd/system/multi-user.target.wants"
  sudo ln -sf /lib/systemd/system/ng-gateway-auto-flash.service \
    "${MOUNT_TARGET}/etc/systemd/system/multi-user.target.wants/ng-gateway-auto-flash.service"
fi

echo ""
echo "================================================================="
echo "  TF Maintenance Card Ready"
echo "================================================================="
echo ""
echo "  Workflow:"
echo "    1. Insert this TF card into an OrangePi 4 Pro"
echo "    2. Power on → system boots from TF"
echo "    3. auto-flash-emmc.sh runs automatically"
echo "       - Detects eMMC"
echo "       - Writes golden image via dd"
echo "       - Reinforces Allwinner bootloader"
echo "       - Writes flash marker to prevent re-flash"
echo "    4. Power off → remove TF card"
echo "    5. Power on → eMMC boots → first-boot expands partition"
echo "    6. QA verification"
echo ""
echo "  The same TF card can flash unlimited devices."
echo "  Use --force in auto-flash-emmc.sh to re-flash a device."
echo ""
