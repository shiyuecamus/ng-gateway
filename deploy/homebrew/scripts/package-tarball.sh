#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
HB_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION="${VERSION:-$(date +%Y%m%d-%H%M%S)}"

uname_s="$(uname -s | tr '[:upper:]' '[:lower:]')"
uname_m="$(uname -m)"
if [[ "$uname_s" != "darwin" ]]; then
  echo "错误: 该脚本目前仅用于 macOS (darwin)，当前: ${uname_s}"
  exit 1
fi

arch="unknown"
case "$uname_m" in
  arm64) arch="arm64" ;;
  x86_64) arch="amd64" ;;
  *)
    echo "错误: 不支持的架构: ${uname_m}"
    exit 1
    ;;
esac

dist_dir="${HB_DIR}/dist"
mkdir -p "$dist_dir"

pkg_name="ng-gateway-${VERSION}-darwin-${arch}"
tarball="${dist_dir}/${pkg_name}.tar.gz"
sha_file="${tarball}.sha256"

echo "=========================================="
echo "Homebrew tarball packaging (macOS)"
echo "=========================================="
echo "VERSION: ${VERSION}"
echo "ARCH:    ${arch}"
echo "OUTPUT:  ${tarball}"
echo "=========================================="
echo ""

echo "[build] cargo xtask build (release, ui-embedded)"
cd "$REPO_ROOT"
cargo xtask build \
  --profile release \
  -- \
  --features ng-gateway-bin/ui-embedded

bin_path="${REPO_ROOT}/target/release/ng-gateway-bin"
if [[ ! -f "$bin_path" ]]; then
  echo "错误: 未找到二进制: ${bin_path}"
  exit 1
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

root="${workdir}/${pkg_name}"
mkdir -p "${root}/bin"
mkdir -p "${root}/data"
mkdir -p "${root}/drivers/builtin"
mkdir -p "${root}/plugins/builtin"
mkdir -p "${root}/certs"
mkdir -p "${root}/pki/own" "${root}/pki/private"

echo "[stage] binary + resources"
cp -f "$bin_path" "${root}/bin/ng-gateway-bin"
chmod +x "${root}/bin/ng-gateway-bin"

# Default config for Homebrew (embedded UI mode)
cp -f "${HB_DIR}/resources/gateway.toml" "${root}/gateway.toml"

# Initial database (contains relative paths to drivers/plugins)
cp -f "${REPO_ROOT}/data/ng-gateway.db" "${root}/data/ng-gateway.db"

# Builtin drivers/plugins for current platform (dylib)
cp -f "${REPO_ROOT}/drivers/builtin/"*.dylib "${root}/drivers/builtin/" 2>/dev/null || true
cp -f "${REPO_ROOT}/plugins/builtin/"*.dylib "${root}/plugins/builtin/" 2>/dev/null || true

echo "[pack] tar.gz"
cd "$workdir"
tar -czf "$tarball" "$pkg_name"

echo "[sha256] ${sha_file}"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$tarball" | awk '{print $1}' > "$sha_file"
else
  shasum -a 256 "$tarball" | awk '{print $1}' > "$sha_file"
fi

echo ""
echo "=========================================="
echo "done"
echo "=========================================="
echo "tarball: ${tarball}"
echo "sha256:  $(cat "$sha_file")"
echo ""


