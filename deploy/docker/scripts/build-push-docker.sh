#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# Ensure BuildKit is enabled
export DOCKER_BUILDKIT="${DOCKER_BUILDKIT:-1}"

# Configuration
REGISTRY="${REGISTRY:-}"
NAMESPACE="${NAMESPACE:-}" # e.g. your dockerhub username
IMAGE_PREFIX="${IMAGE_PREFIX:-ng}"
VERSION="${VERSION:-local}"
PLATFORM="${PLATFORM:-linux/amd64,linux/arm64}"
TAG_LATEST="${TAG_LATEST:-false}"
PUSH="${PUSH:-true}"
BUILDER_NAME="${BUILDER_NAME:-ng-builder}"

# Construct Image Name
# Priority: REGISTRY > NAMESPACE > (empty)
if [[ -n "$REGISTRY" ]]; then
  # Remove trailing slash if present to avoid double slashes
  REGISTRY="${REGISTRY%/}"
  IMAGE_BASE="${REGISTRY}/${IMAGE_PREFIX}-gateway"
elif [[ -n "$NAMESPACE" ]]; then
  IMAGE_BASE="${NAMESPACE}/${IMAGE_PREFIX}-gateway"
else
  # If pushing to docker hub root (official) or local only
  if [[ "$PUSH" == "true" ]]; then
     echo "Error: PUSH is enabled but neither REGISTRY nor NAMESPACE is set."
     echo "For Docker Hub, set NAMESPACE=your_username."
     echo "For Private Registry, set REGISTRY=registry.example.com/repo"
     exit 1
  fi
  IMAGE_BASE="${IMAGE_PREFIX}-gateway"
fi

gateway_image="${IMAGE_BASE}:${VERSION}"

# ... (build_opts logic remains same)
build_opts=()
if [[ -n "$PLATFORM" ]]; then
  build_opts+=(--platform "$PLATFORM")
fi

# Proxy args
proxy_args=()
if [[ -n "${HTTP_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "HTTP_PROXY=${HTTP_PROXY}")
  proxy_args+=(--build-arg "http_proxy=${HTTP_PROXY}")
fi
if [[ -n "${HTTPS_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "HTTPS_PROXY=${HTTPS_PROXY}")
  proxy_args+=(--build-arg "https_proxy=${HTTPS_PROXY}")
fi
if [[ -n "${NO_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "NO_PROXY=${NO_PROXY}")
  proxy_args+=(--build-arg "no_proxy=${NO_PROXY}")
fi

echo "[setup] Setting up Docker Buildx..."
# Ensure we have a builder that supports multi-arch
if ! docker buildx inspect "$BUILDER_NAME" >/dev/null 2>&1; then
  echo "  Creating new builder: $BUILDER_NAME"
  # Pass proxy config to buildx driver if needed
  driver_opt=()
  if [[ -n "${HTTP_PROXY:-}" ]]; then
    driver_opt+=(--driver-opt "env.http_proxy=${HTTP_PROXY}")
    driver_opt+=(--driver-opt "env.HTTP_PROXY=${HTTP_PROXY}")
  fi
  if [[ -n "${HTTPS_PROXY:-}" ]]; then
    driver_opt+=(--driver-opt "env.https_proxy=${HTTPS_PROXY}")
    driver_opt+=(--driver-opt "env.HTTPS_PROXY=${HTTPS_PROXY}")
  fi
  if [[ -n "${NO_PROXY:-}" ]]; then
    driver_opt+=(--driver-opt "env.no_proxy=${NO_PROXY}")
    driver_opt+=(--driver-opt "env.NO_PROXY=${NO_PROXY}")
  fi

  docker buildx create --use --name "$BUILDER_NAME" --driver docker-container --bootstrap "${driver_opt[@]}"
else
  echo "  Using existing builder: $BUILDER_NAME"
  docker buildx use "$BUILDER_NAME"
fi

echo "[build] Building and Pushing gateway image..."
echo "  Target: $gateway_image"
echo "  Platform: $PLATFORM"

# Prepare tags
tags_args=(-t "$gateway_image")
if [[ "$TAG_LATEST" == "true" ]]; then
  latest_image="${IMAGE_BASE}:latest"
  tags_args+=(-t "$latest_image")
  echo "  Tagging additional: $latest_image"
fi

action="--load"
if [[ "$PUSH" == "true" ]]; then
  action="--push"
  echo "  Action: Push to registry"
else
  echo "  Action: Load to local docker daemon (only works for single platform usually)"
  # If multi-platform, --load often fails. Warn user.
  if [[ "$PLATFORM" == *","* ]]; then
     echo "WARNING: Multi-platform build with --load might fail. Use PUSH=true or single platform."
  fi
fi

# Build command
docker buildx build \
  "$action" \
  ${build_opts[@]+"${build_opts[@]}"} \
  ${proxy_args[@]+"${proxy_args[@]}"} \
  "${tags_args[@]}" \
  -f "$REPO_ROOT/deploy/docker/gateway.Dockerfile" \
  "$REPO_ROOT"

echo "[done] Build and push completed."
