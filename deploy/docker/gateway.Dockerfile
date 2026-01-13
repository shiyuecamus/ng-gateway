# Dockerfile for NG Gateway using XTask pattern
# This Dockerfile leverages the xtask automation tool for building and deploying drivers

ARG RUST_VERSION=1.91-bookworm
ARG BASE_IMAGE=debian:bookworm-slim
ARG PNPM_VERSION=9.15.4

# Optional proxy settings (forwarded from deploy scripts via `--build-arg`).
# We map them to ENV in stages that perform network access so tools like
# pnpm/apt/cargo can pick them up automatically.
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY

# ==============================================================================
# STAGE - UI Builder (web-antd)
# Build frontend dist once on the BUILDPLATFORM (host arch), then copy into target image.
# This avoids running Node.js via QEMU emulation on ARM, which is extremely slow.
# ==============================================================================
FROM --platform=$BUILDPLATFORM node:22-alpine AS ui-builder

ARG PNPM_VERSION
ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ENV HTTP_PROXY="${HTTP_PROXY}" \
    HTTPS_PROXY="${HTTPS_PROXY}" \
    NO_PROXY="${NO_PROXY}" \
    http_proxy="${HTTP_PROXY}" \
    https_proxy="${HTTPS_PROXY}" \
    no_proxy="${NO_PROXY}"

ENV PNPM_HOME=/pnpm
ENV PATH="${PNPM_HOME}:${PATH}"

# Ensure pnpm is available inside the container even if the host has no pnpm.
RUN corepack enable && corepack prepare "pnpm@${PNPM_VERSION}" --activate

# Use a stable, cache-mounted pnpm store path for faster incremental builds.
RUN pnpm config set store-dir /pnpm/store

WORKDIR /app/ng-gateway-ui

# Copy frontend workspace
COPY ng-gateway-ui/ ./

# Install deps + build (cache pnpm store + node_modules for BuildKit incremental builds)
RUN --mount=type=cache,target=/pnpm/store \
    --mount=type=cache,target=/app/ng-gateway-ui/node_modules \
    pnpm install --frozen-lockfile --prefer-offline

ENV NODE_OPTIONS=--max-old-space-size=8192
RUN --mount=type=cache,target=/pnpm/store \
    --mount=type=cache,target=/app/ng-gateway-ui/node_modules \
    pnpm run build --filter=@vben/web-antd

RUN ls -lh /app/ng-gateway-ui/apps/web-antd/dist/ || (echo "Build failed: dist directory not found" && exit 1)

# ==============================================================================
# STAGE 0: Build Base - Common build dependencies (libclang, etc.)
# This base image is reused by planner/cacher/builder to avoid repeating apt installs
# ==============================================================================
FROM rust:${RUST_VERSION} AS build-base

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ENV HTTP_PROXY="${HTTP_PROXY}" \
    HTTPS_PROXY="${HTTPS_PROXY}" \
    NO_PROXY="${NO_PROXY}" \
    http_proxy="${HTTP_PROXY}" \
    https_proxy="${HTTPS_PROXY}" \
    no_proxy="${NO_PROXY}"

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      pkg-config \
      protobuf-compiler \
      clang \
      libclang-dev \
      # OpenSSL/SASL are built vendored via rdkafka/openssl-sys/sasl2-sys, avoid system -dev deps.
      # Keep perl available (commonly required by OpenSSL source build toolchain).
      perl \
      cmake && \
    rm -rf /var/lib/apt/lists/*

# Install cargo-chef once in the shared build base to avoid repeating it per stage.
# Cache cargo registry/git downloads for faster rebuilds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-chef --locked

WORKDIR /app

# ==============================================================================
# STAGE 1: Planner - Determine dependency fingerprint
# This stage calculates a "recipe" of your dependencies using cargo-chef
# ==============================================================================
FROM build-base AS planner

# Copy entire workspace (stable + simple).
# Note: we intentionally do NOT attempt to exclude UI here to keep `cargo metadata`
# and `cargo-chef` working reliably for the full workspace graph.
COPY . .

# Prepare build recipe (including xtask)
RUN cargo chef prepare --recipe-path recipe.json

# ==============================================================================
# STAGE 2: Cacher - Build and cache dependencies
# This stage builds only the dependencies based on the recipe from the planner
# ==============================================================================
FROM build-base AS cacher
WORKDIR /app

# Copy recipe from planner
COPY --from=planner /app/recipe.json recipe.json

# Build dependencies (includes xtask dependencies)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo chef cook --release --recipe-path recipe.json

# ==============================================================================
# STAGE 3: Builder - Build application and deploy drivers using XTask
# This stage builds the application and uses xtask to automatically deploy drivers
# ==============================================================================
FROM build-base AS builder
WORKDIR /app

# Copy entire workspace (stable + simple).
COPY . .

# Copy cached dependencies from cacher stage
COPY --from=cacher /app/target target
COPY --from=cacher /usr/local/cargo /usr/local/cargo

# Set build flags for smaller binary
ENV RUSTFLAGS="-C strip=symbols"

# Build application and deploy drivers/plugins using xtask.
# Notes:
# - This Docker image serves UI from filesystem mode (`/app/ui`), so we do NOT pack `ui-dist.zip` here.
# - Embedded UI (`--features ui-embedded`) is intended for single-binary distributions (e.g. Homebrew),
#   and is handled by packaging scripts (see `deploy/homebrew/package-binary.sh`).
RUN cargo xtask build \
    --profile release \
    --without-ui

# Verify drivers were deployed
RUN echo "=== Deployed Drivers ===" && \
    ls -lh /app/drivers/builtin/ && \
    echo "======================="

# Verify plugins were deployed
RUN echo "=== Deployed Plugins ===" && \
    ls -lh /app/plugins/builtin/ && \
    echo "======================="

# Prepare output directory
RUN mkdir -p /out && \
    cp target/release/ng-gateway-bin /out/ && \
    cp -r drivers /out/ && \
    cp -r plugins /out/ && \
    find /out/drivers/builtin -type f ! -name '*.so' -delete && \
    find /out/plugins/builtin -type f ! -name '*.so' -delete

# ==============================================================================
# STAGE 4: Runtime - Create the final minimal image
# This stage copies the compiled binary and drivers into a minimal base image
# ==============================================================================
FROM ${BASE_IMAGE} AS runtime

ARG HTTP_PROXY
ARG HTTPS_PROXY
ARG NO_PROXY
ENV HTTP_PROXY="${HTTP_PROXY}" \
    HTTPS_PROXY="${HTTPS_PROXY}" \
    NO_PROXY="${NO_PROXY}" \
    http_proxy="${HTTP_PROXY}" \
    https_proxy="${HTTPS_PROXY}" \
    no_proxy="${NO_PROXY}"

# Install runtime dependencies
# - ca-certificates, libsqlite3-0: core runtime deps
# - openssl: optional (debug / tooling). Prefer removing if not needed by runtime workflows.
# - curl, telnet: convenient utilities for in-container debugging
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      libsqlite3-0 \
      openssl \
      curl \
      telnet \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Ensure PKI directory structure exists and is writable for OPC UA client
RUN mkdir -p /app/pki/own /app/pki/private

# Copy binary from builder
COPY --from=builder --chmod=755 /out/ng-gateway-bin ./ng-gateway-bin

# Copy all deployed drivers
COPY --from=builder /out/drivers/builtin/ /app/drivers/builtin/

# Copy all deployed plugins
COPY --from=builder /out/plugins/builtin/ /app/plugins/builtin/

# Copy UI static assets into runtime image (filesystem serving mode)
COPY --from=ui-builder /app/ng-gateway-ui/apps/web-antd/dist/ /app/ui/

# Set environment variables
ENV RUST_LOG=info

# Enable UI static serving by default in the all-in-one gateway image.
# - API remains under /api
# - UI is served from /app/ui (built in ui-builder stage)
ENV NG__WEB__UI__ENABLED=true
ENV NG__WEB__UI__MODE=filesystem
ENV NG__WEB__UI__FILESYSTEM_ROOT=/app/ui

# Runtime root directory for relative paths (./data, ./drivers, ./plugins, ./certs, ...).
# Keep it aligned with WORKDIR so the runtime layout is stable.
ENV NG__GENERAL__RUNTIME_DIR=/app

# Expose default ports
# Keep ports aligned with the default `gateway.toml` and offline compose packaging.
EXPOSE 5678
EXPOSE 5679

# Health check (optional - adjust endpoint as needed)
# HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
#   CMD curl -f http://localhost:8878/health || exit 1

# Set the entrypoint for the container
CMD ["./ng-gateway-bin"]
