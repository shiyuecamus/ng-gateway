# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-02-16

Initial public release of NG Gateway — a high-performance industrial IoT edge gateway built with Rust.

### Added

#### Core Architecture

- Runtime hot-pluggable extension system — southbound drivers and northbound plugins loaded as `cdylib` shared libraries via `libloading`, with metadata probing (version, platform, checksum, SDK version)
- Backpressure-first data pipeline — bounded queues across the entire data path with explicit failure semantics (timeout / retry / backoff / drop / block)
- Automatic backpressure propagation from northbound to southbound
- Fault-domain isolation at device / channel / plugin granularity
- Protocol-aware batch planner for Modbus (register/coil gap merge), S7, and Mitsubishi MC
- Zero-copy protocol parsing with `bytes` crate, avoiding unnecessary memory copies in hot paths
- Structured concurrency powered by Tokio async runtime
- jemalloc allocator on Linux for reduced memory fragmentation

#### Southbound Drivers (9 built-in)

- **Modbus** — RTU / TCP, batch read/write planner, TCP connection pool
- **Siemens S7** — batch planner, multi-model support
- **IEC 60870-5-104** — power system telecontrol protocol
- **OPC UA Client** — subscription & polling, security policies (Sign / SignAndEncrypt), multi-auth (Anonymous / UserPassword / Certificate)
- **EtherNet/IP** — industrial automation (CIP)
- **DNP3** — power system protocol, CROB support
- **DL/T 645** — smart meter protocol (1997 / 2007), DI-schema driven parsing
- **CJ/T 188** — water / heat / gas meter protocol, read-only mode
- **Mitsubishi MC** — batch planner

#### Northbound Plugins (4 built-in)

- **ThingsBoard** — uplink / downlink, RPC & attribute sync, large payload chunking
- **Kafka** — partition strategy, TLS / SASL, batching & compression
- **Pulsar** — auth, uplink / downlink, batching
- **OPC UA Server** — data model mapping, write-back link, security policies

#### Web UI

- Modern dashboard built with Vue 3 + TypeScript
- Gateway overview: resource monitoring, connection rate, average collection latency
- Real-time channel observability via WebSocket
- Device / point / action configuration management
- Bulk import via Excel templates (with metadata support)
- Runtime log level adjustment and log management
- Multi-theme support (Ant Design / Naive UI / Element Plus / TDesign)

#### Observability

- Prometheus metrics with low-cardinality label design
  - System resources: CPU / memory / disk / network
  - Southbound: TX / RX, latency, success rate, collection timeout, reconnect count
  - Northbound: send / drops / errors / retries / average latency
  - Queue depth monitoring at each pipeline stage
- WebSocket real-time streams
  - `/api/ws/metrics` — aggregated metrics for dashboard (low cardinality)
  - `/api/ws/monitor` — per-device data snapshot for troubleshooting (high cardinality)
- Unified logging system with per-channel / per-app TTL overrides, text / json format, time / size rotation, log download & cleanup

#### Security

- JWT authentication with Bearer Token
- RBAC via Casbin
- TLS / HTTPS support
- CA certificate management
- Driver / plugin supply-chain governance (version, platform, checksum verification)

#### Configuration

- Multi-source configuration: TOML files + environment variable overrides (`NG__GENERAL__COLLECTOR__*`)
- Runtime hot-reload without restart
- Configuration persistence: runtime changes written back to config files

#### Deployment

- Docker: all-in-one image (gateway + UI), multi-arch (amd64 / arm64), offline package support
- Kubernetes: Helm Chart with liveness / readiness probes
- Linux: systemd service
- macOS: Homebrew support
- Source build: `cargo xtask` build pipeline

#### Developer Experience

- `ng-gateway-sdk` crate for driver / plugin development
- Proc macros: `ng_driver_factory!` / `ng_plugin_factory!`
- `ng-gateway-bench` benchmark tool — 8 scenarios covering collection throughput, driver downlink, and API write-point latency (Modbus & OPC UA)
- Prometheus + Grafana observability stack with docker-compose one-click deployment
- Comprehensive documentation site with quick-start guides

[0.1.0]: https://github.com/shiyuecamus/ng-gateway/releases/tag/v0.1.0
