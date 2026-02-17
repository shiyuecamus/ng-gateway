<div align="center">
  <a href="https://github.com/shiyuecamus/ng-gateway">
    <img alt="NG Gateway Logo" width="215" src="https://i.postimg.cc/MTkKmT2b/image.png">
  </a>
  <br /><br />
</div>

<p align="center">
  <b>NG Gateway · A high-performance industrial IoT edge gateway in Rust</b>
</p>

<p align="center">
  High concurrency · High throughput · Low latency · Reliability · Observability · Extensible
</p>

<p align="center">
  <a href="https://www.rust-lang.org">Rust 1.70+</a> · Tokio runtime · Runtime-pluggable Southward drivers & Northward plugins
</p>

<p align="center">
  <b>English</b> · <a href="./README.zh-CN.md">简体中文</a>
</p>

---

<p align="center">
  <img alt="NG Gateway Web UI - Dashboard" width="900" src="https://i.postimg.cc/QMSmdkgX/ng-home.png" />
  <br />
  <br />
  <img alt="NG Gateway Web UI - Channel Observability" width="900" src="https://i.postimg.cc/2yMJFFbn/ng_channel.png" />
</p>

---

## 🌐 Documentation

- **Docs**: [ng-gateway.com](https://ng-gateway.com)
- **Quick Start (Docker)**: [Install / Quick Start](https://ng-gateway.com/install/)

## ✨ Features

- **Runtime-pluggable Southward drivers (cdylib)**: install / probe / enable on demand
- **Runtime-pluggable Northward plugins (cdylib)**: install / probe / enable on demand, app-isolated runtime
- **Backpressure-first pipeline**: bounded queues end-to-end with explicit failure semantics
- **Protocol-aware batching (Planner)**: reduce RTT and device pressure via batched reads/writes (e.g. Modbus / S7 / MC)
- **Observability for operations**: Prometheus metrics (`/metrics`) + Web UI + live snapshots (Monitor)
- **Runtime tuning & log governance**: runtime settings + per-channel/app log level override with TTL

## 🎯 What it is for

NG Gateway targets industrial edge deployments where you need **high-throughput acquisition**, **reliable uplink delivery**, and **operational-grade observability** across diverse protocols and downstream platforms.

Typical scenarios:

- Multi-protocol device acquisition (PLC / meters / OPC UA servers) → normalize → deliver to platforms or message systems
- Weak networks and downstream jitter → controlled retry/backoff + backpressure boundaries + clear failure semantics
- Extensibility without forking the core → add new protocols/platforms via drivers/plugins

---

## 🚀 Quick Start

> Full guide: [Install / Quick Start](https://ng-gateway.com/install/)

```bash
docker run -d --name ng-gateway \
  --privileged=true \
  --restart unless-stopped \
  -p 8978:5678 \
  -p 8979:5679 \
  -v gateway-data:/app/data \
  -v gateway-drivers:/app/drivers/custom \
  -v gateway-plugins:/app/plugins/custom \
  shiyuecamus/ng-gateway:latest
```

- **Web UI**: `http://localhost:8978/`
- **API**: `http://localhost:8978/api`
- **Default credentials**: `system_admin` / `system_admin` (change it after first login)

## 🛠️ Run from source

> See [Local Development](https://ng-gateway.com/dev/local-dev) for `cargo xtask`, UI dev server, and proxy setup.

From repo root:

```bash
# Build backend + drivers/plugins (skip UI build for faster iteration)
cargo xtask build --profile debug --without-ui

# Run (default config file: gateway.toml)
./target/debug/ng-gateway-bin --config ./gateway.toml
```

Start Web UI (recommended: dev server with HMR):

```bash
cd ng-gateway-ui
pnpm install
pnpm dev:antd
```

## 🧩 Driver / Plugin Customization

- **Southward driver development**: [Southward Driver Dev](https://ng-gateway.com/dev/driver-dev)
- **Northward plugin development**: [Northward Plugin Dev](https://ng-gateway.com/dev/plugin-dev)

## 🧪 Benchmarks

This repo provides `ng-gateway-bench` for benchmarking and regression checks (compare under fixed hardware/config).

## 🗺️ Roadmap & Contributing

- Roadmap: [ng-gateway.com Roadmap](https://ng-gateway.com/guide/introduction/roadmap)
- Issues / discussions: please include logs, metrics, and repro steps

## 🤝 Contributing

- **Bug reports**: include (1) version (2) minimal repro (3) logs/metrics (4) expected vs actual
- **New protocol / platform support**: prefer adding a driver/plugin to keep the core stable
