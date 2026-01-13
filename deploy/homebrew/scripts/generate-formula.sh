#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HB_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION="${VERSION:-}"
URL_ARM64="${URL_ARM64:-${URL:-}}"
SHA256_ARM64="${SHA256_ARM64:-${SHA256:-}}"
URL_AMD64="${URL_AMD64:-${URL:-}}"
SHA256_AMD64="${SHA256_AMD64:-${SHA256:-}}"

if [[ -z "$VERSION" ]]; then
  echo "错误: 缺少 VERSION，例如 VERSION=v1.2.3"
  exit 1
fi
if [[ -z "$URL_ARM64" || -z "$URL_AMD64" ]]; then
  echo "错误: 缺少 URL（tarball 的可下载地址）。可用以下任一方式："
  echo "  - 同时提供 URL_ARM64 / URL_AMD64"
  echo "  - 或提供 URL（将同时用于 arm64/amd64，不推荐）"
  exit 1
fi
if [[ -z "$SHA256_ARM64" || -z "$SHA256_AMD64" ]]; then
  echo "错误: 缺少 SHA256（tarball 的 sha256）。可用以下任一方式："
  echo "  - 同时提供 SHA256_ARM64 / SHA256_AMD64"
  echo "  - 或提供 SHA256（将同时用于 arm64/amd64，不推荐）"
  exit 1
fi

tmpl="${HB_DIR}/Formula/ng-gateway.rb.tmpl"
out="${HB_DIR}/Formula/ng-gateway.rb"

if [[ ! -f "$tmpl" ]]; then
  echo "错误: 未找到模板: ${tmpl}"
  exit 1
fi

tmp_out="${out}.tmp"
sed \
  -e "s|{{VERSION}}|${VERSION}|g" \
  -e "s|{{URL_ARM64}}|${URL_ARM64}|g" \
  -e "s|{{SHA256_ARM64}}|${SHA256_ARM64}|g" \
  -e "s|{{URL_AMD64}}|${URL_AMD64}|g" \
  -e "s|{{SHA256_AMD64}}|${SHA256_AMD64}|g" \
  "$tmpl" > "$tmp_out"

mv "$tmp_out" "$out"
echo "[ok] generated: ${out}"


