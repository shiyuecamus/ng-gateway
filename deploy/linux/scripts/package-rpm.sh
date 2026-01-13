#!/usr/bin/env bash
set -euo pipefail

# package-rpm.sh
#
# Build and package NG Gateway into a `.rpm` using nfpm.
#
# Inputs (env):
# - RELEASE_TAG: e.g. v1.2.3 (optional; used for logging only)
# - PKG_VERSION: e.g. 1.2.3 (required)
# - TARGET_TRIPLE: x86_64-unknown-linux-gnu / aarch64-unknown-linux-gnu (required)
# - RPM_ARCH: x86_64 / aarch64 (required)
# - OUT_DIR: output directory (default: deploy/linux/dist)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

PKG_VERSION="${PKG_VERSION:-}"
TARGET_TRIPLE="${TARGET_TRIPLE:-}"
RPM_ARCH="${RPM_ARCH:-}"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/deploy/linux/dist}"

if [[ -z "${PKG_VERSION}" || -z "${TARGET_TRIPLE}" || -z "${RPM_ARCH}" ]]; then
  echo "error: missing PKG_VERSION/TARGET_TRIPLE/RPM_ARCH"
  exit 1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

rootfs="${workdir}/rootfs"
mkdir -p "${rootfs}"

ROOTFS_DIR="${rootfs}" TARGET_TRIPLE="${TARGET_TRIPLE}" PROFILE="release" \
  bash "${REPO_ROOT}/deploy/linux/scripts/stage-rootfs.sh"

mkdir -p "${OUT_DIR}"

SYSTEMD_UNIT="${REPO_ROOT}/deploy/linux/systemd/ng-gateway.service"
POSTINSTALL="${REPO_ROOT}/deploy/linux/scripts/postinstall.sh"
PREREMOVE="${REPO_ROOT}/deploy/linux/scripts/preremove.sh"

chmod +x "${POSTINSTALL}" "${PREREMOVE}" || true

nfpm_tmpl="${REPO_ROOT}/deploy/linux/nfpm/nfpm.rpm.yaml.tmpl"
nfpm_cfg="${workdir}/nfpm.rpm.yaml"

TMPL="${nfpm_tmpl}" OUT="${nfpm_cfg}" \
PKG_VERSION="${PKG_VERSION}" ROOTFS_DIR="${rootfs}" \
SYSTEMD_UNIT="${SYSTEMD_UNIT}" POSTINSTALL="${POSTINSTALL}" PREREMOVE="${PREREMOVE}" \
RPM_ARCH="${RPM_ARCH}" \
  bash "${REPO_ROOT}/deploy/linux/scripts/render-nfpm-config.sh"

pkg_name="ng-gateway-${PKG_VERSION}-1.${RPM_ARCH}.rpm"
out_path="${OUT_DIR}/${pkg_name}"

echo "[nfpm] building rpm -> ${out_path}"
nfpm package -f "${nfpm_cfg}" -p rpm -t "${out_path}"

echo "[ok] built: ${out_path}"

