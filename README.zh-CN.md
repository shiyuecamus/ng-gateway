<div align="center">
  <a href="https://github.com/shiyuecamus/ng-gateway">
    <img alt="NG Gateway Logo" width="215" src="https://i.postimg.cc/MTkKmT2b/image.png">
  </a>
  <br /><br />
</div>

<p align="center">
  <b>NG Gateway · 基于 Rust 的工业物联网边缘网关</b>
</p>

<p align="center">
  高并发 · 高吞吐 · 低时延 · 强可靠 · 可观测 · 可扩展
</p>

<p align="center">
  <a href="https://www.rust-lang.org">Rust 1.70+</a> · Tokio 异步运行时 · 运行时可插拔南向驱动 & 北向插件
</p>

<p align="center">
  <a href="./README.md">English</a> · <b>简体中文</b>
</p>

---

## 🌐 文档

- **官网文档**：[ng-gateway.com](https://ng-gateway.com)
- **快速开始（Docker）**：[安装 / 快速开始](https://ng-gateway.com/install/)
- **路线图**：[Roadmap](https://ng-gateway.com/guide/introduction/roadmap)

## ✨ Features

- **运行时可插拔的南向驱动（cdylib）**：支持安装 / Probe 探测 / 按需启用
- **运行时可插拔的北向插件（cdylib）**：支持安装 / Probe 探测 / 按需启用，并按 App 隔离 runtime
- **Backpressure-first 数据管线**：全链路有界队列 + 明确失败语义，避免无界堆积导致 OOM 与雪崩
- **协议内生批量规划（Planner）**：通过批处理读写减少 RTT 与设备压力（如 Modbus / S7 / MC）
- **可观测性**：Prometheus 指标（`/metrics`）+ Web UI + 实时快照（Monitor）
- **运行时调参与日志治理**：运行时调参 + per-channel/app 日志级别 TTL 覆盖

## 🎯 项目定位（适合什么场景）

NG Gateway 面向工业现场/边缘侧部署：在多协议设备与云/私有平台之间，提供高吞吐采集、可靠交付与可观测运维的网关底座。

典型场景：

- 多协议设备采集（PLC/电表/OPC UA 服务器等）→ 汇聚 → 上行到平台/消息系统
- 弱网/抖动下持续运行：可控重试/退避、背压隔离、明确失败语义
- 需要二开扩展协议/平台：通过驱动/插件扩展，尽量不改核心仓库

## 🚀 30 秒起跑（Docker）

> 完整图文版请看文档：[快速开始](https://ng-gateway.com/install/)

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

- **Web UI**：`http://localhost:8978/`
- **API**：`http://localhost:8978/api`
- **默认账号**：`system_admin` / `system_admin`（建议首次登录后修改）

## 🛠️ 从源码运行（开发者）

> 完整的开发工作流（`cargo xtask` / UI dev server / 代理联调）见文档：[本地开发](https://ng-gateway.com/dev/local-dev)

在仓库根目录：

```bash
# 构建后端 + drivers/plugins（开发期建议跳过 UI build）
cargo xtask build --profile debug --without-ui

# 运行（默认配置文件为 gateway.toml）
./target/debug/ng-gateway-bin --config ./gateway.toml
```

启动 Web UI（开发期推荐 dev server + HMR）：

```bash
cd ng-gateway-ui
pnpm install
pnpm dev:antd
```

## 📦 驱动 / 插件交付目录（cdylib）

- **builtin**：内置驱动/插件通常部署在 `drivers/builtin`、`plugins/builtin`
- **custom**：自定义扩展建议放在 `drivers/custom`、`plugins/custom`（Docker 示例已挂载 volume）
- **开发期提示**：本仓库推荐用 `cargo xtask deploy` + 重启进程验证动态库替换

## 🧩 扩展开发（驱动 / 插件二开）

- **南向驱动开发**：[南向驱动开发](https://ng-gateway.com/dev/driver-dev)
- **北向插件开发**：[北向插件开发](https://ng-gateway.com/dev/plugin-dev)

## 🧪 性能与基准（Bench）

仓库提供 `ng-gateway-bench` 用于做基准测试与回归对比（建议在相同硬件与固定配置下对比）。

## 🗺️ Roadmap & 参与贡献

- Roadmap：[ng-gateway.com 路线图](https://ng-gateway.com/guide/introduction/roadmap)
- Issue / 讨论：建议在 GitHub 发起，并附上日志/指标/复现步骤
