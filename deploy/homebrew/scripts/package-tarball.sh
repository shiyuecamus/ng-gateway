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

arch="${ARCH:-}"
if [[ -z "${arch}" ]]; then
  case "$uname_m" in
    arm64) arch="arm64" ;;
    x86_64) arch="amd64" ;;
    *)
      echo "错误: 不支持的架构: ${uname_m}"
      exit 1
      ;;
  esac
fi

target_triple="${TARGET_TRIPLE:-}"
if [[ -z "${target_triple}" ]]; then
  case "${arch}" in
    arm64) target_triple="aarch64-apple-darwin" ;;
    amd64) target_triple="x86_64-apple-darwin" ;;
    *)
      echo "错误: 不支持的 ARCH: ${arch}（期望 arm64/amd64）"
      exit 1
      ;;
  esac
fi

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
echo "TARGET:  ${target_triple}"
echo "OUTPUT:  ${tarball}"
echo "=========================================="
echo ""

without_ui="${WITHOUT_UI:-}"
xtask_without_ui_arg=""
if [[ "$without_ui" == "1" || "$without_ui" == "true" || "$without_ui" == "yes" ]]; then
  if [[ ! -f "${REPO_ROOT}/ng-gateway-web/ui-dist.zip" ]]; then
    echo "错误: 指定 WITHOUT_UI=${WITHOUT_UI} 但未找到预置 UI zip: ${REPO_ROOT}/ng-gateway-web/ui-dist.zip"
    echo "提示: 请先在构建前写入 ui-dist.zip（例如 CI 先下载 artifact 到该路径）"
    exit 1
  fi
  xtask_without_ui_arg="--without-ui"
  echo "[build] cargo xtask build (release, ui-embedded, --without-ui)"
else
  echo "[build] cargo xtask build (release, ui-embedded)"
fi
cd "$REPO_ROOT"
cargo xtask build \
  --profile release \
  ${xtask_without_ui_arg} \
  -- \
  --target "${target_triple}" \
  --features ng-gateway-bin/ui-embedded

target_dir="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"
bin_path="${target_dir}/${target_triple}/release/ng-gateway-bin"
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

# Builtin drivers/plugins for current platform (dylib)
cp -f "${REPO_ROOT}/drivers/builtin/"*.dylib "${root}/drivers/builtin/" 2>/dev/null || true
cp -f "${REPO_ROOT}/plugins/builtin/"*.dylib "${root}/plugins/builtin/" 2>/dev/null || true

echo "[pack] tar.gz"
cd "$workdir"
tar -czf "$tarball" "$pkg_name"

echo "[verify] tarball layout (single top-level directory)"
top_levels="$(tar -tzf "$tarball" | cut -d/ -f1 | sort -u)"
top_level_count="$(printf "%s\n" "$top_levels" | sed '/^$/d' | wc -l | tr -d ' ')"
if [[ "$top_level_count" != "1" ]]; then
  echo "错误: tarball 顶层目录数量不为 1（方案A要求只有一个顶层目录）"
  echo "top levels:"
  printf "%s\n" "$top_levels"
  exit 1
fi
if [[ "$top_levels" != "$pkg_name" ]]; then
  echo "错误: tarball 顶层目录名不匹配（期望: ${pkg_name}，实际: ${top_levels}）"
  exit 1
fi

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


