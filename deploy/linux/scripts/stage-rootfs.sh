#!/usr/bin/env bash
set -euo pipefail

# stage-rootfs.sh
#
# Build NG Gateway for a given target triple and stage a Linux rootfs directory
# that matches the desired install layout:
# - /opt/ng-gateway (read-only install area)
# - /etc/ng-gateway (config)
# - /var/lib/ng-gateway (runtime working directory)
#
# This script is intended to be called from CI packaging jobs (deb/rpm).

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

TARGET_TRIPLE="${TARGET_TRIPLE:-}"
PROFILE="${PROFILE:-release}"
ROOTFS_DIR="${ROOTFS_DIR:-}"

if [[ -z "${TARGET_TRIPLE}" ]]; then
  echo "error: missing TARGET_TRIPLE (e.g. x86_64-unknown-linux-gnu)"
  exit 1
fi
if [[ -z "${ROOTFS_DIR}" ]]; then
  echo "error: missing ROOTFS_DIR (staging directory path)"
  exit 1
fi

# UI embedding contract:
# When we build with `ui-embedded`, the binary expects `ng-gateway-web/ui-dist.zip`
# to exist at compile-time. In CI we build it once and download it before packaging.
if [[ ! -f "${REPO_ROOT}/ng-gateway-web/ui-dist.zip" ]]; then
  echo "error: missing prebuilt UI zip: ${REPO_ROOT}/ng-gateway-web/ui-dist.zip"
  echo "hint: download the workflow artifact to this path before building"
  exit 1
fi

echo "=========================================="
echo "Linux staging rootfs"
echo "=========================================="
echo "TARGET_TRIPLE: ${TARGET_TRIPLE}"
echo "PROFILE:       ${PROFILE}"
echo "ROOTFS_DIR:    ${ROOTFS_DIR}"
echo "=========================================="
echo ""

cd "${REPO_ROOT}"

# Build (drivers/plugins + deploy) for the given target.
# - We skip UI build because ui-dist.zip is already present.
# - We enable `ui-embedded` so Linux packages contain embedded UI by default.
cargo xtask build \
  --profile "${PROFILE}" \
  --without-ui \
  -- \
  --target "${TARGET_TRIPLE}" \
  --features ng-gateway-bin/ui-embedded

target_dir="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
bin_path="${target_dir}/${TARGET_TRIPLE}/${PROFILE}/ng-gateway-bin"
if [[ ! -f "${bin_path}" ]]; then
  echo "error: binary not found: ${bin_path}"
  exit 1
fi

opt_dir="${ROOTFS_DIR}/opt/ng-gateway"
mkdir -p "${opt_dir}/bin" "${opt_dir}/data" "${opt_dir}/drivers/builtin" "${opt_dir}/plugins/builtin"

# Stage empty config/runtime directories as part of the package payload.
# The actual files will be created/copied in postinstall on first install.
etc_dir="${ROOTFS_DIR}/etc/ng-gateway"
var_dir="${ROOTFS_DIR}/var/lib/ng-gateway"
mkdir -p "${etc_dir}"
mkdir -p "${var_dir}/data" "${var_dir}/certs" "${var_dir}/pki/own" "${var_dir}/pki/private"
mkdir -p "${var_dir}/drivers/builtin" "${var_dir}/drivers/custom"
mkdir -p "${var_dir}/plugins/builtin" "${var_dir}/plugins/custom"

echo "[stage] binary"
cp -f "${bin_path}" "${opt_dir}/bin/ng-gateway-bin"
chmod +x "${opt_dir}/bin/ng-gateway-bin"

echo "[stage] default config"
if [[ -f "${REPO_ROOT}/gateway.toml" ]]; then
  cp -f "${REPO_ROOT}/gateway.toml" "${opt_dir}/gateway.toml"
else
  echo "error: default config not found: ${REPO_ROOT}/gateway.toml"
  exit 1
fi

echo "[stage] initial database"
if [[ -f "${REPO_ROOT}/data/ng-gateway.db" ]]; then
  cp -f "${REPO_ROOT}/data/ng-gateway.db" "${opt_dir}/data/ng-gateway.db"
else
  echo "error: initial db not found: ${REPO_ROOT}/data/ng-gateway.db"
  exit 1
fi

# Builtin drivers/plugins (shared objects) were deployed by xtask into
# `drivers/builtin` and `plugins/builtin`.
echo "[stage] builtin drivers/plugins"
cp -f "${REPO_ROOT}/drivers/builtin/"*.so "${opt_dir}/drivers/builtin/" 2>/dev/null || true
cp -f "${REPO_ROOT}/plugins/builtin/"*.so "${opt_dir}/plugins/builtin/" 2>/dev/null || true

echo "[ok] staged rootfs at: ${ROOTFS_DIR}"

