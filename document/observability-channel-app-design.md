## NG Gateway：Channel（含 per-device）与 App 可观测性（Observability）设计与实施计划

> 目标：在 **Web UI 层**提供 Channel 与 Northward App 的“产品级可观测性监控”体验；同时在 **南向驱动层**补齐通道级/设备级 `bytes_in/out` 的完整语义闭环，并以 `NGMetricsHub` 为唯一权威数据源（single source of truth）。
>
> 范围：`ng-gateway-common/src/metrics`、`ng-gateway-sdk/src/southward/*`、`ng-gateway-core/*`、`ng-gateway-web/*`、`ng-gateway-ui/apps/web-antd/*`、`ng-gateway-models/src/idens/menu.rs`。

---

## 1. 现状审计（基于当前仓库）

### 1.1 NGMetricsHub 已经具备的能力（可复用）

- **统一指标中心**：`ng-gateway-common/src/metrics/mod.rs` 内已有 `NGMetricsHub`（显式对象、启动时初始化、全局唯一）。
- **已落地的聚合 snapshot**（对 UI 友好）：
  - Gateway 全局：`GatewayStatusSnapshot`（WS：`GET /api/ws/metrics`，scope=global）
  - Channel：`ChannelStatsSnapshot`（WS：`GET /api/ws/metrics`，scope=channel）
  - App：`NorthwardAppStatsSnapshot`（WS：`GET /api/ws/metrics`，scope=app）
- **已有成熟 UI 模式**：
  - Dashboard：使用 `/api/ws/metrics` 生成 KPI + 趋势（`ng-gateway-ui/apps/web-antd/src/views/dashboard/analytics`）
  - 设备实时监控：WS + 批量 coalesce + 分页/过滤（`/api/ws/monitor` 与 `maintenance/monitor`）

### 1.2 当前缺口（本设计要补齐）

- **Channel metrics snapshot 里的 bytes 仍为 0**：`ng-gateway-common/src/metrics/southward.rs` 的 `snapshot_metrics()` 目前硬编码：
  - `bytes_sent: 0`
  - `bytes_received: 0`
- **缺少 per-device observability**：
  - Prometheus 指标设计强调低基数（不放 `device_id` label），因此 per-device 需要走 **Hub 内存态 + REST 分页查询**（或独立存储）而不是直接 Prom labels。
- **bytes_in/out 语义未定义且实现不统一**：
  - 自研驱动有 `Framed/Codec` 或明确的 `frame`/`pdu`，计量容易落地。
  - 使用第三方库的驱动（Modbus/OPC UA/DNP3/EtherNet-IP）常把 socket 隐藏在库内部，必须做“统一计量抽象 + 针对库的注入点策略”。

---

## 2. 信息架构与导航（IA）

### 2.1 菜单与路由：独立隐藏菜单项（hide_in_menu=true）

要求：Channel/App 的 observability 是独立菜单项，但 **隐藏**（不在侧边栏展示），仅从列表/详情 CTA 进入。

当前 repo 中 `ng-gateway-models/src/idens/menu.rs` 已加入（示例）：

- **Channel Observability**（隐藏）
  - `path`: `/southward/channel/:id/observability`
  - `component`: `/southward/channel/observability/index`
  - `hide_in_menu: true`
- **App Observability**（隐藏）
  - `path`: `/northward/app/:id/observability`
  - `component`: `/northward/app/observability/index`
  - `hide_in_menu: true`

> 说明：必须挂在 `BasicLayout` 的子树下（parent_id 为 Southward/Northward 的目录节点），保证路由渲染有 `router-view` 父级。

### 2.2 入口（Entry Points）

- **Channel 列表页**：在 `CellOperation` 增加“监控/Observability”按钮（如图表 icon），跳转到 `/southward/channel/{id}/observability`。
- **App 列表页**：增加“监控/Observability”按钮，跳转到 `/northward/app/{id}/observability`。
- **Dashboard drilldown**：可选增强（非必须），从全局概览快速进入“某个异常 Channel/App”的监控页。

---

## 3. 指标语义与闭环（核心：bytes_in/out）

### 3.1 统一口径：bytes_in/out = **Transport Bytes**（全局唯一语义）

你要求“全局统一、不要两端式计量”。因此本设计将 `bytes_in/out` 定义为 **Transport Bytes**，并保证所有驱动都用同一口径采集：

- **bytes_out**：驱动把字节写入其底层 I/O（TCP stream / Serial stream / UDP socket）的有效 payload 字节数总和  
- **bytes_in**：驱动从其底层 I/O 读取到的有效 payload 字节数总和

边界与包含项（语义必须明确，避免误解）：

- **包含**：协议报文头/尾、CRC、preamble、keepalive、握手/会话维护、重传（只要它们经由该连接真实写入/读出）  
- **包含**：TLS/加密后的密文字节（如果第三方库在 TCP 之上做 TLS，本计量看到的是 **加密后的** transport 字节）  
- **不包含**：IP/TCP/UDP 包头等内核网络层开销（我们只统计用户态 read/write 的 buffer 大小）  
- **不尝试**：推导“协议层纯净 payload”——这类口径在不同协议/第三方库下难以做到一致且可验证，你已明确不需要

> 这套语义的价值：**跨驱动一致、实现可落地、可验证**。它代表“实际链路占用”的字节统计，是容量规划/带宽诊断/计费的稳健口径。

### 3.2 方向（Direction）统一定义

- **southward.bytes_out**：Gateway → 现场设备/现场总线（写/读请求、控制命令等）
- **southward.bytes_in**：现场设备/现场总线 → Gateway（读响应、上报、确认等）

### 3.3 计量粒度：Channel 级 + Device 级（per-device）

- **Channel 级**：同一 channel 内所有设备流量聚合。
- **Device 级**：按 device_id 聚合（用于 per-device 表格与 TopN 排序）。

### 3.4 闭环：定义 → 打点 → 聚合 → API → UI → CTA

一个“语义完整”的闭环必须满足：

- **定义**：每个指标的口径、单位、边界清楚（本节已给出 bytes 的口径）。
- **打点**：每条 southward I/O 都能同时命中：
  - `operation`（collect / write_point / execute_action / internal）
  - `device_id`（能映射则必须映射）
  - `channel_id`、`driver_type`
  - `direction`（in/out）
  - `transport`（tcp/serial/udp，可选，仅用于诊断与归因）
- **聚合**：`NGMetricsHub` 作为权威聚合器，产出：
  - Channel snapshot：可给 `/api/ws/metrics scope=channel` 使用
  - Device snapshot list：可分页查询
- **UI**：默认展示最“可行动”的指标（黄金四信号：Traffic/Errors/Latency/Saturation），并提供 CTA。
- **CTA**：按钮必须对应可执行的闭环动作：reconnect/disable/healthcheck/export diagnostics/open realtime monitor。

---

## 4. 南向 bytes 计量：统一方案（Transport wrap + NGMetricsHub 权威）

### 4.1 总体架构

核心思想：

- **Transport wrap（在 SDK 封装）**：所有驱动（含自研与第三方库）都必须通过 SDK 创建/持有底层 I/O（TCP/Serial/UDP），由 SDK 包装为 `InstrumentedTransport` 自动计量 `bytes_in/out`。驱动不需要理解“该算多少字节”，也不需要在业务逻辑里手工打点。
- **NGMetricsHub 权威**：SDK 包装层把计量事件汇聚到由 host 注入的 hub（single source of truth），最终对外 snapshot/API 都从 hub 读，避免“各驱动各自计量漂移”。

建议新增注入能力（示意）：

```text
NGMetricsHub
  ├─ SouthwardMetricsHub（现有：channel low-card metrics）
  ├─ SouthwardDeviceObservabilityHub（新增：device-level聚合 + 分页查询）
  └─ SouthwardTransportBytesHub（新增：统一 bytes_in/out 计量 + 窗口 bps）

SouthwardInitContext（ng-gateway-sdk）
  ├─ devices / points_by_device / runtime_channel / publisher（现有）
  └─ observability: SouthwardObservabilityContext（新增）
        ├─ channel_id / driver_type
        ├─ meter: Arc<dyn SouthwardTransportMeter>
        └─ transport: Arc<dyn InstrumentedTransportFactory>
```

### 4.2 SDK 侧：统一计量封装（MeteredStream / InstrumentedTransport）

在 `ng-gateway-sdk` 增加一层 **可复用 transport instrumentation**，让所有驱动在“连接创建”这一处完成 bytes 计量闭环：

- **`MeteredStream<T>`**（TCP/Serial）：实现 `tokio::io::AsyncRead + AsyncWrite`，在 `poll_read/poll_write` 内对 `bytes_in/out` 做累加
- **`MeteredUdpSocket`**（UDP）：包装 `tokio::net::UdpSocket`，在 `send_to/recv_from`（或等价）处累加 `bytes_in/out`
- **`SouthwardTransportMeter`**（host 注入的计量汇聚器）：SDK wrapper 把“每次读/写的字节数”上报到 hub（同时包含 channel_id / driver / device_id）

关键点（这是“统一语义”的根基）：

- **统一语义，拒绝估算**：
  - `bytes_in/out` **只允许**来自 transport wrapper 的真实读写计量（Measured transport bytes）
  - 不做任何“按协议/按调用参数”的估算口径，避免语义漂移与误导
- **非阻塞与低开销**：wrapper 只做原子累加或写入 lock-free 环形缓冲（由 hub 实现），绝不在热路径做分配/格式化。
- **device_id 归属策略**：
  - 单设备连接：transport 绑定 `device_id`（最优）
  - 多设备复用连接：transport 绑定 `channel_id`，device 侧 bytes 通过“请求上下文绑定/切换”（见 4.4.2 Modbus）
  - 无法精确归属：允许 `device_id=None`，但必须保证 channel 聚合正确，并在 UI 标注“未分摊”

建议的 SDK 抽象（示意，强调“封装在 SDK”而不是散落在驱动里）：

```rust
/// Host 注入：把 bytes 汇聚到 NGMetricsHub（权威来源）
pub trait SouthwardTransportMeter: Send + Sync {
    fn add_bytes_in(&self, channel_id: i32, driver: &str, device_id: Option<i32>, bytes: u64);
    fn add_bytes_out(&self, channel_id: i32, driver: &str, device_id: Option<i32>, bytes: u64);
}

/// SDK 封装：所有 TCP/Serial 都用这个包装类型（统一口径）
pub struct MeteredStream<T> {
    inner: T,
    meter: std::sync::Arc<dyn SouthwardTransportMeter>,
    channel_id: i32,
    driver: std::sync::Arc<str>,
    device_id: std::sync::atomic::AtomicI32, // -1 表示 None；用于“请求上下文绑定”
}

/// SDK 封装：连接创建统一入口（便于全局替换/patch）
pub trait InstrumentedTransportFactory: Send + Sync {
    async fn connect_tcp(
        &self,
        channel_id: i32,
        driver: &str,
        device_id: Option<i32>,
        addr: std::net::SocketAddr,
    ) -> anyhow::Result<MeteredStream<tokio::net::TcpStream>>;

    async fn open_serial(
        &self,
        channel_id: i32,
        driver: &str,
        device_id: Option<i32>,
        cfg: SerialOpenConfig,
    ) -> anyhow::Result<MeteredStream<tokio_serial::SerialStream>>;

    async fn bind_udp(
        &self,
        channel_id: i32,
        driver: &str,
        device_id: Option<i32>,
        bind: std::net::SocketAddr,
    ) -> anyhow::Result<MeteredUdpSocket>;
}
```

### 4.3 自研协议栈驱动：同样必须走 Transport wrap（保持全局一致）

对于自研驱动（IEC104 / S7 / MC / DLT645 / CJT188），虽然我们“可以在 Framed/Codec 层按 frame 长度计量”，但为了满足“全局统一语义”，本设计要求它们也统一改为：

- 由 SDK 创建底层 `TcpStream/SerialStream`（或自建 I/O）
- 立刻 wrap 成 `MeteredStream`
- 再交给 `Framed` / 自研 session 使用

这样做到：

- 所有协议的 bytes 都来自同一个“底层 I/O 统计口径”
- UI 展示与对比无需解释“某协议按 frame 计量、另一个按 socket 计量”的差异

### 4.4 第三方库驱动：是否可切入 Transport wrap（深度剖析）

第三方库驱动分类（当前仓库）：

- Modbus：`tokio-modbus`
- OPC UA：`async-opcua`
- DNP3：`dnp3` crate
- EtherNet/IP：`rust-ethernet-ip`

可切入性结论矩阵（基于当前仓库代码与调用方式）：

| Driver | 当前连接入口（本仓库） | 能否注入/包裹 transport（Measured）？ | 状态 |
|---|---|---:|---|
| Modbus TCP | `tcp::connect(addr)` | **是**（tokio-modbus v0.17 提供 `tokio_modbus::client::tcp::attach<T: AsyncRead+AsyncWrite+...>(transport)`） | **计划：改造为 `TcpStream::connect` + `MeteredStream` + `tcp::attach`** |
| Modbus RTU | `open_native_async()` + `rtu::attach(stream)` | 是 | 可实现（Measured） |
| OPC UA | `connect_to_endpoint_directly(...)` | 否（当前 API 不暴露 transport） | **TODO**（已向上游提 issue，等待 connector/attach/from_stream） |
| DNP3 TCP/UDP/Serial | `spawn_master_tcp_client/udp/serial(...)` | 否（当前 API 不接受外部 socket/stream） | **TODO**（已向上游提 issue，等待 connector/attach/from_stream） |
| EtherNet/IP | `EipClient::connect/with_route_path(...)` | 否（当前 API 不暴露 transport） | **TODO**（已向上游提 issue，等待 connector/attach/from_stream） |

### 4.5 代码内 TODO 注释规范（必须落到 driver/supervisor 的连接创建处）

你要求的不只是“文档里列 TODO”，还要 **在代码里** 对应位置写清楚：这块需要做 Transport wrap/bytes 计量，但依赖上游第三方库提供 connector/attach/from_stream，等上游支持后再落地。

规范建议（写入代码注释的内容要包含 4 点）：

1) **为什么**：为了统一 `bytes_in/out` 的 Transport Bytes 语义（Measured）  
2) **缺什么能力**：上游库当前不支持 connector/attach/from_stream（无法注入/包裹 transport）  
3) **应该改哪里**：指出要改的具体连接创建语句/函数  
4) **解除条件**：等上游 issue 落地后（写上 issue 链接/编号）再实施

建议注释模板：

```rust
// TODO(observability-bytes):
// - Goal: wrap transport with MeteredStream/MeteredUdpSocket to provide measured bytes_in/out (Transport Bytes).
// - Blocker: upstream library does not expose connector/attach/from_stream yet, so we cannot inject the transport here.
// - Change point: <function/line> where the session/client/connection is created.
// - Unblock: implement once upstream adds <API>; see issue: <link or number>.
```

### 4.5 不支持注入的驱动：待办（TODO）

本设计要求 **统一走 Transport wrap（Measured transport bytes）**。因此对“不支持 connector/attach/from_stream”的驱动，直接列为待办，直到上游能力就绪：

- **TODO(opcua)**：在 `ng-gateway-southward/opcua/src/supervisor.rs` 的连接创建处添加 TODO 注释；等待 `async-opcua` 支持 transport connector / connect_with_stream 后，将底层连接改为 SDK `MeteredStream`。
- **TODO(dnp3)**：在 `ng-gateway-southward/dnp3/src/supervisor.rs` 中 `spawn_master_tcp_client/udp/serial` 调用处添加 TODO 注释；等待 `dnp3` 支持注入 tcp/udp/serial transport（connector/from_socket/from_stream）后落地。
- **TODO(ethernet-ip)**：在 `ng-gateway-southward/ethernet-ip/src/supervisor.rs` 的 `EipClient::connect/with_route_path` 调用处添加 TODO 注释；等待 `rust-ethernet-ip` 支持 connect_with_stream / connector 后落地。

### 4.6 Modbus（计划）：用 tokio-modbus `tcp::attach` 实现 Measured bytes（不依赖上游）

#### 4.6.1 目标

把 Modbus TCP 的连接创建改造成 “我们创建 stream → SDK wrap → attach”，从而把 `bytes_in/out` 计量权收敛到 SDK 的 `MeteredStream`（全局权威）。

#### 4.6.2 现状与依据

- 当前实现：`ng-gateway-southward/modbus/src/supervisor.rs` 中使用 `tokio_modbus::client::tcp::connect(addr).await`
- tokio-modbus v0.17 提供：`tokio_modbus::client::tcp::attach<T: AsyncRead + AsyncWrite + ...>(transport) -> Context`
  - 这意味着 Modbus TCP **无需等待上游**，可以直接接入 `MeteredStream<TcpStream>`

#### 4.6.3 改造步骤（文档级计划）

1) 在 supervisor 中用 `tokio::net::TcpStream::connect(addr).await` 建立连接
2) 在 SDK 层提供/使用 `MeteredStream<TcpStream>`（实现 `AsyncRead+AsyncWrite`）
3) 用 `tokio_modbus::client::tcp::attach(metered_stream)` 创建 `Context`
4) 保持现有 pool/重连语义不变（只是替换 Context 的构造方式）


---

## 5. 后端聚合与 API 设计（满足 UI：分页/筛选/排序）

### 5.1 设计原则

- **Prometheus 继续保持低基数**：channel 级 ok；device 级不放 label。
- **per-device 走 Hub 内存态 + REST 查询**：
  - 适合分页/排序/过滤；
  - 可在 hub 内做 LRU/TTL，控制内存与生命周期。

### 5.2 新增（建议）数据模型（DTO）

建议在 `ng-gateway-models` 增加（示意命名，可调整）：

- `SouthwardChannelObservabilitySnapshot`
  - 基础：复用 `ChannelStatsSnapshot`
  - 扩展：bytes（Transport Bytes）、bps（最近窗口）、error_rate、timeout_rate、p95 latency（若引入 HDRHistogram/ring buffer）
- `SouthwardDeviceObservabilityRow`
  - `device_id / device_name / device_type / status / state`
  - `last_activity / last_error / last_ok`
  - `bytes_in/out_total`、`bps_in/out`（窗口）
  - `io_success/failed/timeout`（窗口 or 累计）
  - `avg_latency_ms`（EWMA）+ `p95_ms`（若有）

> 备注：若暂不引入 p95，UI 仍可用 EWMA + last 作为第一版；但最佳实践是“至少 p50/p95”。

### 5.3 REST API（建议）

#### 5.3.1 Channel per-device 列表（分页/筛选/排序）

- **GET** `/api/observability/southward/channel/{channel_id}/devices`
- Query：
  - `page`、`pageSize`
  - `keyword`（match device_name/device_type）
  - `status`（enabled/disabled）
  - `state`（connected/disconnected/…）
  - `sortBy`（`bps_out`/`bps_in`/`error_rate`/`latency_ms`/`last_activity`…）
  - `sortOrder`（asc/desc）
- 返回：`PageResult<SouthwardDeviceObservabilityRow>`

#### 5.3.2 单设备详情（用于右侧表格 row 展开/抽屉）

- **GET** `/api/observability/southward/channel/{channel_id}/devices/{device_id}`
- 返回：`SouthwardDeviceObservabilityRow` +（可选）mini-trend 数据

#### 5.3.3 Channel 诊断导出（给 CTA 使用）

- **POST** `/api/observability/southward/channel/{channel_id}/diagnostics/export`
- 返回：json 或 zip（包含：channel config、driver config 摘要、最近错误、最近操作统计、bytes counters、建议动作）

> 权限：复用 channel read/ops 权限或新增 `observability:read` scope（最佳实践：read-only 默认开放给运维角色）。

### 5.4 WS（复用现有 /api/ws/metrics）

- 左侧 KPI + 趋势：复用 `/api/ws/metrics` 的 `scope=channel` / `scope=app`
- 若要支持更多趋势字段：
  - 方案 A：扩展 channel/app 的 snapshot DTO（加入 bytes、queue 等），UI 自己做 trend buffer（推荐）
  - 方案 B：新增 `/api/ws/observability` 专用 WS（不推荐，除非字段过多且要按需订阅）

---

## 6. Web UI 设计：Channel 详情监控页（Split Layout）

### 6.1 页面目标（用户任务）

- **快速判断**：这个 Channel 是否健康？瓶颈在带宽/延迟/错误/超时/设备？
- **定位 TopN**：哪个 device 最“吃带宽/最慢/最容易失败/最久未活跃”？
- **闭环操作**：从页面直接执行动作（reconnect/disable/healthcheck/export/跳转实时监控）。

### 6.2 页面结构（Split Layout，可左右/上下切换）

Split Layout 顶部固定工具条：

- Channel 标题：`{name} · {driver} · {state badge}`
- 时间范围：`最近 5m / 15m / 1h / 自定义`（决定趋势窗口与“窗口统计”）
- 刷新策略：`实时（WS）` / `暂停` / `手动刷新`
- Split 方向切换：`左右` ↔ `上下`（持久化到 preferences/localStorage）
- 诊断入口：`导出诊断` / `查看配置` / `打开实时监控(maintenance/monitor)`（可直接跳转并带上 channel_id）

#### 左半屏：Channel 指标区（KPI + 趋势 + CTA）

**KPI（第一行，卡片式，强调可行动）**

- 连接状态：Connected/Connecting/Disconnected + 最近一次变化时间
- 成功率：\( success / (success+fail+timeout) \)（窗口）
- 延迟：avg（EWMA）+ p95（窗口）
- 吞吐：
  - `bytes_out`（bps）+ `bytes_in`（bps）
- 采集效率：points_read_success / timeout（窗口）
- 重连次数：reconnect_total（累计）+ 最近窗口重连次数

**趋势（第二行，3~4 张图，默认 60 个点/1分钟窗口，跟 dashboard 一致）**

- 延迟趋势：avg/p95
- 错误与超时：fail_rate / timeout_rate
- 吞吐趋势：out_bps / in_bps
- 采集周期：collect_cycle_seconds（avg/p95）

**CTA（第三行，闭环动作）**

- 连接类：`Reconnect`（触发 driver reconnect）、`Restart Channel`（重建 driver instance）、`Disable/Enable`
- 诊断类：`Run Healthcheck`（立即执行）、`Export Diagnostics`（下载）
- 运维类：`Open Logs`（如已集成日志页/外部系统）、`Open Realtime Monitor`（跳转 `/maintenance/monitor` 并预选 channel）

> UX 最佳实践：CTA 旁边显示“风险提示”和“预计影响范围”（例如 restart 会短暂中断采集）。

#### 右半屏：per-device 指标分页表（分页/筛选/排序）

**表格要求**

- 规模：支持 1k+ 设备（分页加载）
- 交互：过滤（状态/关键词）、排序（bps/错误率/延迟/last_activity）、列自定义
- 行内可视化：关键列用 mini bar/mini sparkline（可选）
- 行展开：展开显示该设备最近错误摘要 + mini trend（可选）

**建议列（第一版就非常好用）**

- 基础：deviceName / deviceType / status / deviceState
- 活跃：lastActivity / lastError / lastOk
- 流量：bps_out / bps_in（窗口） + bytes_total（累计）
- 质量：error_rate / timeout_rate（窗口）
- 延迟：avg_ms / p95_ms（窗口）
- 点位：points_total / points_read_success（窗口）
- 操作：`定位`（滚动到左侧图联动）、`打开实时监控`、`禁用设备`、`重试/重连`

> 高级列（可在“诊断模式”展开）：重连次数、协议错误分类、队列/背压信息（若有）。

---

## 7. Web UI 设计：App 详情监控页（最佳实践 + 极致体验）

### 7.1 App 监控的“黄金四信号”落地到 UI

对于 Northward App（插件）而言，黄金四信号映射为：

- **Traffic**：messages/sec（uplink/downlink）、payload bytes/sec（若可得）
- **Errors**：fail/sec、dropped/sec、routing_errors、plugin errors
- **Latency**：plugin process latency（已有 histogram + avg），必要时补 p95
- **Saturation**：队列深度/blocked_seconds、buffer 使用率、重试风暴（reconnect/retry）

### 7.2 页面结构（建议：顶部总览 + 分区 Tab）

**顶部总览（固定）**

- App 标题：`{name} · {pluginType} · {state badge}`
- 连接状态：Connected/Disconnected + 最近一次错误
- 关键 KPI：
  - uplink: sent/dropped/errors（窗口）+ msg/s
  - latency: avg + p95
  - saturation: queue depth / blocked seconds（若补齐）

**Tab 设计（推荐 4 个）**

1. **Overview（总览）**
   - KPI + 趋势（延迟/吞吐/错误/丢弃/队列）
   - 最近错误列表（Top 5，含时间、分类、摘要、建议动作）
   - “健康建议”卡片（自动给出：队列满/插件未连接/重试过频/错误率过高）

2. **Delivery（数据投递/Uplink）**
   - 吞吐（msg/s）、成功/失败/丢弃趋势
   - Drop 原因分布（若能区分：queue full / not connected / timeout）
   - Top Devices（按被投递消息数/失败数）——可选（需要新增聚合）
   - CTA：调大队列/改 dropPolicy/开启 buffer（如果产品允许在线改）

3. **Control（下行/命令/WritePoint）**
   - Downlink 消息计数、成功率、平均耗时
   - 最近下行失败（含 request_id、目标设备、错误分类）
   - CTA：重试、查看请求详情、打开调试页（`/maintenance/debug`）

4. **Reliability & Performance（可靠性与性能）**
   - 重连次数/退避状态（RetryPolicy 可视化）
   - 队列背压（blocked seconds、近 1h 峰值深度）
   - payload 体积趋势（需要 bytes 计量时启用）
   - CTA：导出 app 诊断、复制“问题报告模板”

### 7.3 App 页的 CTA（必须闭环）

- 生命周期：`Restart App`、`Reconnect`、`Disable/Enable`
- 诊断：`Export Diagnostics`、`View Config`、`Open Logs`
- 验证：`Send Test Message`（仅在支持的插件/环境开放）、`Validate Subscription`

> 最佳实践：所有“危险动作”必须二次确认，并展示“影响范围”（例如会暂停数据上送）。

---

## 8. 实施计划（按优先级拆分，支持并行开发）

### 8.1 Phase 0：语义冻结（必须先做）

- 明确并冻结指标口径：
  - `bytes_in/out` = **Transport Bytes**（本设计 3.1 的唯一语义）
  - 方向定义
  - 统计窗口（例如 1m/5m/15m）

### 8.2 Phase 1：NGMetricsHub 扩展（权威聚合）

- 为 southward channel 增加 bytes counters + bps（窗口）：
  - `SouthwardChannelMetricHandles` 的 `snapshot_metrics()` 不再返回 0
- 新增 device-level 聚合 hub（不走 Prom labels）：
  - 支持分页/排序/过滤需要的索引字段（last_activity、bps、error_rate、latency 等）
- 新增对外查询接口（core→web）：
  - `get_channel_device_observability_page(channel_id, query)`

### 8.3 Phase 2：SDK 注入 & 驱动改造（Transport wrap 全覆盖）

- 在 `SouthwardInitContext` 注入：
  - `InstrumentedTransportFactory`（统一创建/包装 TCP/Serial/UDP）
  - `SouthwardTransportMeter`（把 bytes 汇聚到 hub）
- 自研驱动：停止按 frame/pdu 估算字节，统一改为使用 `MeteredStream/MeteredUdpSocket` 作为底层 I/O
- 第三方库驱动：按 4.4 的结论执行：
  - 能通过 “attach/from_stream” 接受外部 stream 的：直接改造使用 SDK transport
  - 不支持注入的：按 4.5 TODO 清单跟踪；在能力就绪前不提供 bytes（保持语义一致）

### 8.4 Phase 3：Web API（REST + RBAC）

- 增加 `/api/observability/southward/channel/{id}/devices`（分页）
- 增加 `/api/observability/.../diagnostics/export`
- 增加 RBAC rules（read-only）

### 8.5 Phase 4：Web UI（web-antd）

- 新增页面：
  - `src/views/southward/channel/observability/index.vue`
  - `src/views/northward/app/observability/index.vue`
- 复用 `/api/ws/metrics`（scope=channel/app）做 KPI+趋势
- 右侧 per-device 表格调用 REST 分页
- 列表页增加“监控”入口按钮

### 8.6 Phase 5：验证与对齐（必须做，保证 bytes 语义正确）

**验证策略（建议）**

- 单元测试（SDK）：对 `MeteredStream/MeteredUdpSocket` 做确定性测试：
  - 写入 N 字节 → bytes_out 增加 N
  - 读取 N 字节 → bytes_in 增加 N
  - 并发场景下计数单调递增、无负值、无溢出 panic（saturating）
- 集成测试（每个驱动至少一条 happy path）：
  - 启动一个 echo/mock server（TCP/UDP/Serial loopback）
  - 驱动发起一次 read/write/collect
  - 断言：channel/device 的 bytes_in/out 在该操作后 **非 0 且方向正确**（至少满足“链路真实占用”语义）
- 回归：确保现有 `/api/ws/metrics` 与 dashboard 不破坏（只扩字段，不改旧字段语义）

---

## 9. 需要改动/新增的代码点清单（便于开工）

### 9.1 菜单（已做/需确认）

- `ng-gateway-models/src/idens/menu.rs`：隐藏菜单项（Channel/App observability）

### 9.2 SDK

- `ng-gateway-sdk/src/southward/model.rs`：`SouthwardInitContext` 注入 observability/meter
- 新增：`ng-gateway-sdk/src/southward/transport/`（建议目录）
  - `metered_stream.rs` / `metered_udp.rs`

### 9.3 common metrics

- `ng-gateway-common/src/metrics/southward.rs`：
  - bytes counters（channel）
  - snapshot 不再返回 0
- 新增：`ng-gateway-common/src/metrics/southward_device.rs`（或同文件）：
  - device-level 聚合与查询

### 9.4 core（驱动与采集路径）

- 自研驱动 session：各 `ng-gateway-southward/*/src/protocol/session/*`（按实际路径）
- 第三方库驱动：
  - `ng-gateway-southward/modbus/src/{driver,supervisor}.rs`（仅当可注入 transport 时）
  - `ng-gateway-southward/opcua/src/{driver,supervisor}.rs`（TODO：等待上游 connector/attach/from_stream）
  - `ng-gateway-southward/dnp3/src/{driver,supervisor}.rs`（TODO：等待上游 connector/attach/from_stream）
  - `ng-gateway-southward/ethernet-ip/src/{driver,supervisor}.rs`（TODO：等待上游 connector/attach/from_stream）

### 9.5 web

- 新增：`ng-gateway-web/src/api/v1/observability/*.rs`（建议新模块）

### 9.6 UI

- `ng-gateway-ui/apps/web-antd/src/views/southward/channel/modules/schemas/table-columns.ts`：
  - 增加 `observability` operation code
- `ng-gateway-ui/apps/web-antd/src/views/northward/app/modules/schemas/table-columns.ts`：
  - 增加 `observability` operation code
- 新增两套详情页目录：
  - `src/views/southward/channel/observability/`
  - `src/views/northward/app/observability/`

---

## 10. 交付物（Definition of Done）

- **Channel 页**
  - Split Layout（左右/上下切换）
  - 左侧 KPI + 趋势 + CTA（可执行）
  - 右侧 per-device 表格：分页/筛选/排序（稳定、性能可控）
- **App 页**
  - 总览 + 多 Tab
  - KPI + 趋势 + 错误摘要 + CTA
- **指标闭环**
  - Channel bytes_in/out（Transport Bytes）语义清晰且非 0
  - per-device bytes 与质量指标可查可排序（能归属则必须归属；不能归属需明确标注）
  - 第三方库驱动：能注入则提供 bytes；不支持注入则按 TODO 跟踪，暂不提供 bytes（保持语义一致）

