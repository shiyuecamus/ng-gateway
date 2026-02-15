# ng-gateway-bench

`ng-gateway-bench` 是一个 **基于场景的性能基准测试工具**，用于压测 ng-gateway 的南向驱动（Modbus / OPC UA）以及网关 HTTP API 写点链路，并输出 Markdown 表格结果。

## 测量内容

- **数据采集（Collection）** — 场景 1–7
  - 周期调度下的端到端 `Driver::collect_data()` 执行情况
- **驱动层数据下发（Driver Downlink）** — 场景 7
  - 在采集同时进行 `Driver::write_point()` 的响应时间
- **API 数据下发（API Downlink）** — 场景 8
  - 通过 HTTP `POST /api/point/write` 测试完整链路延迟：HTTP → 认证 → 网关验证 → 值转换 → 通道串行化 → 驱动写入 → HTTP 响应
- **资源摘要（best-effort）** — 场景 1–7
  - **CPU 使用(avg)**：当前进程的平均 CPU 使用率（采样平均）
  - **内存使用(peak RSS)**：当前进程在测试窗口内的 RSS 峰值
  - **网络带宽消耗**：整机网卡累计字节数差分/时间（`receive: ... transmit: ...`，注意是**系统级**，非进程级）

## 场景说明

场景定义在 `ng-gateway-bench/src/scenarios.rs`：

| 场景 | 类型 | 说明 |
|---:|---|---|
| 1 | 采集 | 1 ch · 10 dev · 1k pts · 1000 ms |
| 2 | 采集 | 5 ch · 10 dev · 1k pts · 1000 ms |
| 3 | 采集 | 10 ch · 10 dev · 1k pts · 1000 ms |
| 4 | 采集 | 1 ch · 1 dev · 1k pts · 100 ms |
| 5 | 采集 | 5 ch · 1 dev · 1k pts · 100 ms |
| 6 | 采集 | 10 ch · 1 dev · 1k pts · 100 ms |
| 7 | 采集 + 驱动下发 | 10 ch · 10 dev · 1k pts · 1000 ms + driver `write_point` |
| 8 | API 写点 | 纯 HTTP API `write_point` 延迟测试（无需本地驱动） |

> **场景 8** 不建立本地通道/驱动，bench 仅作为 HTTP 客户端向运行中的网关发送写点请求。需要通过 `--api-base-url` 指定网关地址。

## 快速开始

建议优先用 release 构建（吞吐更稳定）。

由于 Rust 链接限制（多个驱动包含相同的 C-ABI 符号），测试不同协议时需要使用不同的 `--features` 参数。

### Modbus — 采集测试（场景 1–6）

```bash
# 运行单个场景
cargo run --release -p ng-gateway-bench -- --protocol modbus --scenario 3

# 运行所有场景（1–7，场景 8 需要 --api-base-url 才会包含）
cargo run --release -p ng-gateway-bench -- --protocol modbus --all-scenarios
```

### Modbus — 混合负载测试（场景 7：采集 + 驱动下发）

```bash
cargo run --release -p ng-gateway-bench -- \
  --protocol modbus \
  --scenario 7 \
  --downlink-points 100 \
  --downlink-iterations 50 \
  --downlink-timeout-ms 3000
```

### Modbus — API 写点延迟测试（场景 8）

```bash
cargo run --release -p ng-gateway-bench -- \
  --protocol modbus \
  --scenario 8 \
  --api-base-url http://192.168.1.11:8978 \
  --api-username system_admin \
  --api-password system_admin \
  --api-device-id-start 1 \
  --api-device-id-end 100 \
  --api-point-key-prefix p \
  --api-points-per-device 1000 \
  --api-downlink-iterations 100 \
  --api-downlink-timeout-ms 3000
```

### OPC UA — 采集测试

必须显式**禁用默认 feature** 并 **开启 opcua feature**。

```bash
# 运行单个场景
cargo run --release -p ng-gateway-bench --no-default-features --features opcua -- \
  --protocol opcua --scenario 3

# 运行所有场景
cargo run --release -p ng-gateway-bench --no-default-features --features opcua -- \
  --protocol opcua --all-scenarios
```

### OPC UA — API 写点延迟测试（场景 8）

```bash
cargo run --release -p ng-gateway-bench --no-default-features --features opcua -- \
  --protocol opcua \
  --scenario 8 \
  --api-base-url http://192.168.1.11:8978 \
  --api-username system_admin \
  --api-password system_admin \
  --api-device-id-start 1 \
  --api-device-id-end 100 \
  --api-point-key-prefix p \
  --api-points-per-device 1000 \
  --api-downlink-iterations 100 \
  --api-downlink-timeout-ms 3000
```

### 全场景一次跑完（1–8）

```bash
cargo run --release -p ng-gateway-bench -- \
  --protocol modbus \
  --all-scenarios \
  --api-base-url http://192.168.1.11:8978 \
  --api-username system_admin \
  --api-password system_admin \
  --api-device-id-start 1 \
  --api-device-id-end 100 \
  --api-downlink-iterations 100
```

> `--all-scenarios` 会自动运行 1–8。若未提供 `--api-base-url`，场景 8 将被静默跳过。

### 调整预热/测量时长

```bash
cargo run --release -p ng-gateway-bench -- \
  --protocol modbus --scenario 3 --warmup-secs 10 --duration-secs 60
```

### 调整资源采样间隔

```bash
cargo run --release -p ng-gateway-bench -- \
  --protocol modbus --scenario 3 --sample-interval-ms 200
```

## 参数说明

### 运行控制

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--protocol <modbus\|opcua>` | 选择协议 | `modbus` |
| `--scenario <1..=8>` | 选择单个场景 | — |
| `--all-scenarios` | 顺序执行所有场景 | `false` |
| `--warmup-secs <s>` | 预热时长（不记录统计） | `5` |
| `--duration-secs <s>` | 正式测量时长 | `20` |
| `--sample-interval-ms <ms>` | CPU/RSS/网络采样间隔 | `500` |

### Modbus 参数

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--modbus-host` | Modbus TCP server 地址 | `8.155.153.52` |
| `--modbus-port` | 端口 | `502` |
| `--modbus-slave-id` | UnitId/SlaveId 起始值 | `1` |
| `--modbus-slave-id-max` | UnitId/SlaveId 最大值（轮询分配） | `10` |
| `--modbus-address-base` | 寄存器起始地址 | `0` |
| `--modbus-address-step` | 点位地址步进（寄存器单位） | `2` |
| `--modbus-tcp-pool-size` | 每个 channel 的 TCP 连接池大小 | `10` |

### OPC UA 参数

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--opcua-endpoint` | OPC UA endpoint URL | `opc.tcp://192.168.66.8:53530/...` |
| `--opcua-application-name` | 客户端 application name | `SimulationServer@shiyuecamus-MacBook-Pro` |
| `--opcua-application-uri` | 客户端 application URI | `urn:shiyuecamus-MacBook-Pro.local:...` |
| `--opcua-node-id-start` | 点位 NodeId 起始 (`ns=3;i=<start>`) | `1002` |

### 驱动下发参数（场景 7）

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--downlink-points` | 下发点位数量 | `100` |
| `--downlink-iterations` | 下发测试次数 | `50` |
| `--downlink-timeout-ms` | 单次 `write_point` 超时 | `3000` |

### API 下发参数（场景 8）

| 参数 | 说明 | 默认值 |
|---|---|---|
| `--api-base-url` | 网关 API 地址（如 `http://192.168.1.11:8978`） | — (必填) |
| `--api-username` | 登录用户名 | `system_admin` |
| `--api-password` | 登录密码 | `system_admin` |
| `--api-version` | API 版本头 | `v1` |
| `--api-device-id-start` | 设备 ID 起始（含） | `1` |
| `--api-device-id-end` | 设备 ID 结束（含） | `100` |
| `--api-point-key-prefix` | 点位 key 前缀（如 `p` → `p1`…`p1000`） | `p` |
| `--api-points-per-device` | 每设备点位数 | `1000` |
| `--api-downlink-iterations` | 写点测试次数 | `100` |
| `--api-downlink-timeout-ms` | 单次写超时（毫秒） | `3000` |

## 输出说明

### 数据采集性能表（场景 1–7）

```
| 场景 | 协议 | Channel数量 | 每个Channel设备数 | 每个设备点位数 | 采集频率 | 总计点位 | 点位类型 | 内存使用(peak RSS) | CPU 使用(avg) | 网络带宽消耗 |
```

> 场景 8 不出现在此表中（bench 进程不做采集）。

### 数据下发延迟表（场景 7 & 8）

```
| 场景 | 协议 | 下发方式 | 设备范围 | 点位数/设备 | 测试次数 | 成功 | 失败 | 最小响应时间 | 最大响应时间 | 平均响应时间 |
```

- **下发方式**：`driver write_point`（场景 7）或 `API write_point`（场景 8）
- **设备范围**：场景 8 显示 `1..=100` 形式；场景 7 显示 `-`

## 重要说明

### Modbus Float32 映射

本 benchmark 将每个 Float32 点位建模为 **2 个 Modbus Holding Registers**（`quantity = 2`），默认地址布局为：

- point 0 → address 0（寄存器 0..1）
- point 1 → address 2（寄存器 2..3）
- ...

如果你的 simulator 地址布局不同，请调整 `--modbus-address-base` 和 `--modbus-address-step`。

### 场景 8 前置条件

- 网关必须已启动并可达
- 网关中已创建好目标设备和点位（设备 ID 和点位 key 需与 CLI 参数匹配）
- bench 会先调用 `/api/auth/login` 获取 Bearer token，然后复用该 token 发送所有写点请求

## 生成压测用 DevicePoints 点位表（Excel）

有些压测/性能回归场景会需要一批符合网关导入格式的 **DevicePoints 模板文件**（含隐藏 `__meta__`），命名形如：

- `{driver}-scenario{S}-channel{N}-device-points.xlsx`

```bash
# 生成 Modbus 的 scenario 1..7
cargo run --release -p ng-gateway-bench --bin gen_point_tables -- \
  --all --out-dir generated --locale zh-CN

# 生成 OPC UA 的 scenario 1..7
cargo run --release -p ng-gateway-bench --bin gen_point_tables \
  --no-default-features --features opcua -- \
  --all --out-dir generated --locale zh-CN

# 只生成某几个场景
cargo run --release -p ng-gateway-bench --bin gen_point_tables -- \
  --scenarios 1,3,7 --out-dir generated --locale zh-CN
```
