#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# This project Dockerfiles rely on BuildKit features (e.g. `RUN --mount=type=cache`).
# Ensure BuildKit is enabled so `docker build` works consistently across environments.
export DOCKER_BUILDKIT="${DOCKER_BUILDKIT:-1}"

# Config via env with sane defaults
REGISTRY="${REGISTRY:-}"
IMAGE_PREFIX="${IMAGE_PREFIX:-ng}"
VERSION="${VERSION:-local}"
PLATFORM="${PLATFORM:-}"
TAG_LATEST="${TAG_LATEST:-false}"

gateway_image="${REGISTRY:+$REGISTRY/}${IMAGE_PREFIX}-gateway:${VERSION}"

build_opts=()
if [[ -n "$PLATFORM" ]]; then
  build_opts+=(--platform "$PLATFORM")
fi

# If proxy envs are set, forward them to Docker build as build args.
# This keeps behavior consistent with `package-helm-offline.sh`.
proxy_args=()
if [[ -n "${HTTP_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "HTTP_PROXY=${HTTP_PROXY}")
fi
if [[ -n "${HTTPS_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "HTTPS_PROXY=${HTTPS_PROXY}")
fi
if [[ -n "${NO_PROXY:-}" ]]; then
  proxy_args+=(--build-arg "NO_PROXY=${NO_PROXY}")
fi

echo "[build] gateway (all-in-one, includes UI) -> $gateway_image"
# Bash 3.2 compatibility: in `set -u`, expanding an empty array like "${arr[@]}"
# triggers "unbound variable". Use the `${arr[@]+"${arr[@]}"}`
# idiom to expand only when the array has elements.
docker build \
  ${build_opts[@]+"${build_opts[@]}"} \
  ${proxy_args[@]+"${proxy_args[@]}"} \
  -t "$gateway_image" \
  -f "$REPO_ROOT/deploy/docker/gateway.Dockerfile" \
  "$REPO_ROOT"

if [[ "$TAG_LATEST" == "true" ]]; then
  echo "[tag] latest"
  docker tag "$gateway_image" "${REGISTRY:+$REGISTRY/}${IMAGE_PREFIX}-gateway:latest"
fi

echo "[done] Image built:"
echo "  $gateway_image"


