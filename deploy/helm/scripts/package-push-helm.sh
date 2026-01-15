#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
CHART_DIR="$REPO_ROOT/deploy/helm/ng-gateway"
DIST_DIR="$REPO_ROOT/target/helm-dist"

# Configuration
REGISTRY="${REGISTRY:-}" # e.g. oci://registry-1.docker.io/myuser
VERSION="${VERSION:-}"
PUSH="${PUSH:-true}"

# Proxy handling for Helm
# Helm respects HTTP_PROXY, HTTPS_PROXY, and NO_PROXY env vars automatically.
# We ensure they are exported just in case they were passed as inline vars.
export HTTP_PROXY="${HTTP_PROXY:-}"
export HTTPS_PROXY="${HTTPS_PROXY:-}"
export NO_PROXY="${NO_PROXY:-}"
export http_proxy="${HTTP_PROXY:-}"
export https_proxy="${HTTPS_PROXY:-}"
export no_proxy="${NO_PROXY:-}"

if [[ -z "$REGISTRY" ]] && [[ "$PUSH" == "true" ]]; then
  echo "Error: REGISTRY environment variable is required for pushing (e.g. 'oci://registry-1.docker.io/myuser')."
  echo "Usage: REGISTRY=oci://registry-1.docker.io/username VERSION=0.1.0 ./package-push-helm.sh"
  exit 1
fi

if [[ -z "$VERSION" ]]; then
    # Extract version from Chart.yaml if not provided
    VERSION=$(grep '^version:' "$CHART_DIR/Chart.yaml" | awk '{print $2}')
    echo "Version not provided, using version from Chart.yaml: $VERSION"
fi

echo "[init] Cleaning dist directory: $DIST_DIR"
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

echo "[lint] Linting chart..."
helm lint "$CHART_DIR"

echo "[package] Packaging chart version $VERSION..."
helm package "$CHART_DIR" --version "$VERSION" --app-version "$VERSION" --destination "$DIST_DIR"

PACKAGE_FILE=$(find "$DIST_DIR" -name "*.tgz" -type f | head -n 1)

if [[ -z "$PACKAGE_FILE" ]]; then
    echo "Error: Package file not found in $DIST_DIR"
    exit 1
fi

echo "Found package: $PACKAGE_FILE"

if [[ "$PUSH" == "true" ]]; then
    echo "[push] Pushing to $REGISTRY..."
    
    # Check if registry is OCI
    if [[ "$REGISTRY" == oci://* ]]; then
        helm push "$PACKAGE_FILE" "$REGISTRY"
    else
        echo "Error: Only OCI registries (oci://...) are supported by this script for helm push."
        exit 1
    fi
    
    echo "[success] Chart pushed successfully."
else
    echo "[skip] Push skipped. Chart available at: $PACKAGE_FILE"
fi

