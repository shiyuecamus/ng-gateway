#!/usr/bin/env bash
set -euo pipefail

# package.sh
#
# Build and package NG Gateway into a `.deb` or `.rpm` using nfpm.
# Unified script replacing the former package-deb.sh and package-rpm.sh.
#
# Usage:
#   PKG_VERSION=1.2.3 TARGET_TRIPLE=aarch64-unknown-linux-gnu PKG_ARCH=arm64 \
#     bash package.sh --format deb
#
# Inputs (env):
#   RELEASE_TAG    - e.g. v1.2.3 (optional; used for logging only)
#   PKG_VERSION    - e.g. 1.2.3 (required)
#   TARGET_TRIPLE  - x86_64-unknown-linux-gnu / aarch64-unknown-linux-gnu (required)
#   PKG_ARCH       - deb: amd64/arm64  |  rpm: x86_64/aarch64 (required)
#   OUT_DIR        - output directory (default: deploy/linux/dist)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# ─── Argument Parsing ───

FORMAT=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --format|-f) FORMAT="$2"; shift 2 ;;
    *)           echo "error: unknown option: $1"; exit 1 ;;
  esac
done

PKG_VERSION="${PKG_VERSION:-}"
TARGET_TRIPLE="${TARGET_TRIPLE:-}"
PKG_ARCH="${PKG_ARCH:-}"
OUT_DIR="${OUT_DIR:-${REPO_ROOT}/deploy/linux/dist}"

if [[ -z "${FORMAT}" ]]; then
  echo "error: missing --format (deb or rpm)"
  exit 1
fi
if [[ -z "${PKG_VERSION}" || -z "${TARGET_TRIPLE}" || -z "${PKG_ARCH}" ]]; then
  echo "error: missing PKG_VERSION/TARGET_TRIPLE/PKG_ARCH"
  exit 1
fi

# ─── Format-specific configuration ───

case "${FORMAT}" in
  deb)
    nfpm_tmpl="${REPO_ROOT}/deploy/linux/nfpm/nfpm.deb.yaml.tmpl"
    nfpm_cfg_name="nfpm.deb.yaml"
    pkg_name="ng-gateway_${PKG_VERSION}_${PKG_ARCH}.deb"
    ARCH_VAR="DEB_ARCH"
    ;;
  rpm)
    nfpm_tmpl="${REPO_ROOT}/deploy/linux/nfpm/nfpm.rpm.yaml.tmpl"
    nfpm_cfg_name="nfpm.rpm.yaml"
    pkg_name="ng-gateway-${PKG_VERSION}-1.${PKG_ARCH}.rpm"
    ARCH_VAR="RPM_ARCH"
    ;;
  *)
    echo "error: unsupported format '${FORMAT}' (must be deb or rpm)"
    exit 1
    ;;
esac

# ─── Build ───

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
POSTREMOVE="${REPO_ROOT}/deploy/linux/scripts/postremove.sh"

chmod +x "${POSTINSTALL}" "${PREREMOVE}" "${POSTREMOVE}" || true

nfpm_cfg="${workdir}/${nfpm_cfg_name}"

# Export the correct arch variable name for render-nfpm-config.sh.
export "${ARCH_VAR}=${PKG_ARCH}"

TMPL="${nfpm_tmpl}" OUT="${nfpm_cfg}" \
PKG_VERSION="${PKG_VERSION}" ROOTFS_DIR="${rootfs}" \
SYSTEMD_UNIT="${SYSTEMD_UNIT}" POSTINSTALL="${POSTINSTALL}" PREREMOVE="${PREREMOVE}" \
POSTREMOVE="${POSTREMOVE}" \
  bash "${REPO_ROOT}/deploy/linux/scripts/render-nfpm-config.sh"

out_path="${OUT_DIR}/${pkg_name}"

echo "[nfpm] building ${FORMAT} -> ${out_path}"
nfpm package -f "${nfpm_cfg}" -p "${FORMAT}" -t "${out_path}"

echo "[ok] built: ${out_path}"
