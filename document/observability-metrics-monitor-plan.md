## NG Gateway 生产级可观测性（Metrics/Logs/Tracing）闭环方案与计划

> 目标：为 NG Gateway 建立一套**产品级**运行时可观测系统，覆盖高吞吐数据面与控制面：能在“弱网/设备异常/平台限流/背压拥塞”这种常态下快速回答：
>
> - **哪里慢？为什么慢？**（延迟、阻塞、重试、背压传播路径）
> - **丢没丢？丢在哪？**（丢弃、满队列、buffer 过期、策略命中）
> - **还能撑多久？**（队列水位、资源余量、重连风暴趋势）
> - **对哪个 Channel/Driver/北向 App 生效？**（隔离、归因、对比）

---

## 1. 现状评估（基于当前仓库实现）

### 1.1 已存在的观测能力

- **tracing 日志**：
  - 已有 `ng-gateway-common/src/logger.rs`，通过 `tracing-subscriber` 同时输出控制台与文件。
  - 支持**运行时全局** `Level` 调整（`NGAppContext::change_log_level`），但当前无法做到“仅对某个 Channel 实例提 Debug”。
- **OTel Metrics（仅 Metrics，不含 Trace）**：
  - `Settings.metrics` 已支持 `enabled/endpoint/export_interval/service_name`，默认 `enabled=false`。
  - `NGAppContext::init_metrics()` 使用 `opentelemetry-otlp` 导出（OTLP gRPC，默认 `http://localhost:4317`）。
  - `ng-gateway-common/src/metrics.rs` / `NGAppContext::init_metrics()` 已采系统指标（CPU/Memory/Disk）。
- **控制面状态快照（非 Prometheus 型 Metrics）**：
  - `ng-gateway-models/src/core/metrics.rs` 定义了 `GatewayStatus/GatewayMetrics/*Stats`，用于 REST 查询聚合状态（包含 northward/southward/collector/system_info）。

### 1.2 当前缺口（与你需求的直接映射）

- **队列与背压**：当前没有统一的“queue depth / drops / blocked time”产品级指标体系，也没有对每条关键队列做可视化闭环。
- **采集侧（Southward）**：已有 `ChannelMonitor` 观测连接状态变迁，但缺少：
  - 统一指标命名、延迟直方图、读点成功率、IO 字节统计、重连次数、在线率等。
- **北向链路（Northward）**：`AppActor` 已有 lock-free counters（sent/dropped/errors/latency），但缺少：
  - 标准化导出、成功率、分插件/分 app 的仪表盘与告警规则。
- **资源维度**：目前采了 CPU/Memory/Disk（且指标名偏 OTel 点分隔），但缺少：
  - 连接数、任务数、句柄数、网络 IO 带宽等“运维关键指标”，以及与背压指标关联定位能力。

---

## 2. 栈选型结论：全量 Prometheus（scrape）+ Grafana（看板 + 告警）

### 2.1 结论（唯一方案）

- **Metrics**：**只走 Prometheus（拉取 / scrape）+ Grafana** 闭环（仪表盘 + 告警）。
- **Logs**：继续用 `tracing` 输出结构化日志（建议 JSON），由 **Grafana Loki（Promtail/Grafana Alloy）** 收集。
- **Tracing（分布式追踪）**：短期可先不做“全链路 Trace”，但必须把 **Span 语义与层级标准**先定好；需要时再接入 Tempo（可选），但**不作为本方案交付目标**。

### 2.2 为什么不引入 OpenTelemetry（本项目指标不再使用 OTel）

你的约束很明确：**不需要兼容/迁移路径**，并且允许删除历史 OTel 代码。对 NG Gateway 这种边缘网关产品而言，Prometheus 方案有更明确的产品交付边界：

- **运维闭环最短**：`/metrics` 一条链路打通 Prometheus + Grafana，天然可告警。
- **资源与复杂度更可控**：不引入 OTel SDK/Exporter/Collector 的组合复杂度，减少运行时与依赖面。
- **更符合边缘场景**：scrape 模型在 K8s/同网段部署更常见，chart 里也已预留 PodMonitor/ServiceMonitor 与 `/metrics` 路径。

结论：

- **删除/重构所有 OTel 指标代码**（含依赖与初始化）。
- 指标统一通过 **Prometheus registry + `/metrics` endpoint** 暴露。

### 2.3 部署形态（闭环架构）

#### 形态 1：Prometheus 直接 scrape 网关（最简单）

- 网关：暴露 `/metrics`（Prometheus 格式）
- Prometheus：定期 scrape
- Grafana：读 Prometheus + 告警
- Loki/Alloy：收集网关日志（stdout/file）

适用：K8s、同网段可达、现场允许 pull 模型。

#### 形态 2（可选）：边缘侧 Agent 中转（非必需）

- 网关：暴露 `/metrics`
- **Grafana Alloy（推荐）**：在边缘侧 scrape → remote_write 到 Prometheus/Mimir
- Grafana：读远端 Prometheus/Mimir

适用：现场 NAT、Prometheus 无法直接访问网关、需要统一采集/压缩/重标记。

---

## 3. 产品级 Metrics 设计原则（必须严格遵守）

### 3.1 指标类型选择（Counter/Gauge/Histogram）

- **Counter**：只增不减，用于计数（总请求、总错误、总丢弃）。
- **Gauge**：可增可减，用于瞬时状态（水位、连接状态、并发数）。
- **Histogram**：用于延迟/耗时/队列阻塞时间的分布（P50/P95/P99）。

### 3.2 命名、单位与一致性

推荐遵循 Prometheus 约定：

- 统一前缀：`ng_gateway_`
- 单位后缀明确：`_seconds`、`_bytes`、`_ratio`、`_count`
- Counter 必须以 `_total` 结尾
- 状态类用 `0/1` 或枚举映射的 `Gauge`

> 当前代码里 OTel 指标名有 `system.cpu.usage` 这种点分隔命名。短期可保留并在 Collector/Exporter 侧做 rename；中期建议统一为 Prometheus 风格。

### 3.3 标签（labels）与基数（cardinality）控制：生死线

**必须分层**对待你提出的“按 Device/Point 维度”的指标：

- **生产默认（低基数，必须）**
  - labels 仅允许：`channel_id`、`driver`、`app_id`、`plugin`、`result`（有限集合）、`queue`（有限集合）
- **诊断模式（受控高基数，可选）**
  - `device_id`、`device_name`、`point_id` 等只允许在：
    - 临时开启的 debug 指标集（带 TTL 自动关闭），或
    - REST 状态查询（`/status` 类）而不是 Prometheus labels

**明确禁止**：

- 把“错误字符串/异常 message”作为 label（会炸基数，拖垮 TSDB）
- 以“任意 topic / 任意 key / 任意 IP”做 label（不可控）

---

## 4. 指标体系总览（覆盖你提出的四大维度）

> 下面是“产品级指标清单（建议）”。其中很多指标当前仓库已有部分数据源（例如 `AppActor.metrics`、`GatewayStatus`、`ChannelMonitor`），核心工作是：统一命名、补齐缺失埋点、暴露为 Prometheus、配套 Grafana 面板与告警。

### 4.1 队列与背压（最关键，必须先做）

#### 4.1.1 统一队列观测模型（建议对每条关键 bounded queue 标准化）

- **`ng_gateway_queue_depth`**（Gauge）
  - labels：`queue`
  - 说明：队列当前长度（近似也可，但必须单调一致）
- **`ng_gateway_queue_capacity`**（Gauge）
  - labels：`queue`
  - 说明：队列容量（固定值，便于算水位）
- **`ng_gateway_queue_dropped_total`**（Counter）
  - labels：`queue`,`reason`（例如 `full`/`timeout`/`buffer_full`/`expired`）
- **`ng_gateway_queue_blocked_seconds`**（Histogram）
  - labels：`queue`
  - 说明：`DropPolicy=Block` 等待时间分布（P95/P99 能直接暴露“背压是否在生效”）

#### 4.1.2 建议纳入“关键队列清单”（最小集）

- `collector_outbound`：Collector → Gateway 的 `outbound_queue_capacity`
- `northward_events`：全局北向事件队列（`settings.general.northward.queue_capacity`）
- `northward_app_{app_id}`：每个 AppActor 的 data queue（`queue_policy.capacity`）
- `northward_app_buffer_{app_id}`：AppActor 的 buffer queue（`buffer_capacity`）
- `control_plane_write_serialization`：每 channel 的写序列化等待（不是 mpsc，但同样应统计“等待/超时”）

> 备注：Tokio `mpsc` 本身不直接暴露 depth，生产最佳实践是“在 wrapper 层维护 atomic gauge”，而不是尝试从内部结构取值。

### 4.2 采集侧（Southward：Channel/Driver/Device）

#### 4.2.1 Channel 连接与状态

- **`ng_gateway_southward_channel_connected`**（Gauge）
  - labels：`channel_id`,`driver`
  - 取值：Connected=1，否则 0
- **`ng_gateway_southward_channel_state`**（Gauge）
  - labels：`channel_id`,`driver`
  - 取值：用枚举映射（Connecting=1/Connected=2/Reconnecting=3/Disconnected=4/Failed=5）
- **`ng_gateway_southward_channel_reconnect_total`**（Counter）
  - labels：`channel_id`,`driver`

#### 4.2.2 采集延迟与吞吐

- **`ng_gateway_southward_collect_cycle_seconds`**（Histogram）
  - labels：`channel_id`,`driver`,`result`（success/fail/timeout）
  - 说明：一次采集循环总耗时（从调度开始到数据入 pipeline）
- **`ng_gateway_southward_point_read_total`**（Counter）
  - labels：`channel_id`,`driver`,`result`（success/fail）
  - 说明：读点总次数（聚合级）
- **`ng_gateway_southward_point_read_success_ratio`**（Gauge 或在 Grafana 用 PromQL 计算）
  - 推荐：在 Grafana/PromQL 计算，避免额外指标

#### 4.2.3 IO 字节与帧统计（可选但强烈建议）

- **`ng_gateway_southward_io_bytes_sent_total`**（Counter）
- **`ng_gateway_southward_io_bytes_received_total`**（Counter）
- **`ng_gateway_southward_frames_total`**（Counter）
  - labels：`channel_id`,`driver`,`direction`（tx/rx）,`result`

> 说明：不同协议驱动对“字节/帧”的定义不同，建议先做“连接层”IO，再逐步做到“协议层帧”。

#### 4.2.4 Device 在线率与诊断信息（需要“指标 + 状态查询”双轨）

你提出了：

- `Device XXX 上一次采集错误信息`
- `Device XXX 上一次采集错误信息时间戳`

这两项**不适合**做 Prometheus labels（会引入高基数与不可控字符串），推荐拆为：

- **Metrics（聚合）**：用于趋势、告警、容量与 SLA
- **Status/Debug API（细节）**：用于“定位某台设备发生了什么”

建议指标：

- **`ng_gateway_southward_device_active`**（Gauge）
  - labels：`channel_id`,`driver`
  - 说明：当前 Active 设备数（按 channel 聚合）
- **`ng_gateway_southward_device_total`**（Gauge）
  - labels：`channel_id`,`driver`
  - 说明：设备总数（按 channel 聚合）
- **`ng_gateway_southward_device_online_ratio`**（Gauge，或用 PromQL 计算）
  - 推荐用 PromQL：`active/total`
- **`ng_gateway_southward_device_reconnect_total`**（Counter）
  - labels：`channel_id`,`driver`
  - 说明：设备侧重连次数（如果协议层能区分；否则先做 channel 级重连）

建议状态查询（非 Prometheus 指标）：

- `GET /api/v1/southward/devices/{device_id}/diagnostics`
  - 返回：`last_error_code`（有限集合）、`last_error_message`（字符串）、`last_error_at`（时间戳）、`last_collect_cost_ms`、`last_publish_count` 等

> 说明：这类“每设备最后一次错误 message”属于**状态面数据**，更适合走 REST/WS 查询或持久化到本地 DB（按需），而不是走时序指标。

### 4.3 北向链路（Northward：App/Plugin）

#### 4.3.1 App 连接与状态

- **`ng_gateway_northward_app_connected`**（Gauge）
  - labels：`app_id`,`plugin`
  - 取值：Connected=1，否则 0
- **`ng_gateway_northward_app_state`**（Gauge）
  - labels：`app_id`,`plugin`
  - 取值：用 `AppState` 映射（Uninitialized/Starting/Running/Stopping/Stopped/Error）
- **`ng_gateway_northward_app_reconnect_total`**（Counter）
  - labels：`app_id`,`plugin`

#### 4.3.2 上/下行消息总计、失败总计、成功率与延迟

你需求里有“上/下行”，建议统一抽象为 `direction`：

- **`ng_gateway_northward_messages_total`**（Counter）
  - labels：`app_id`,`plugin`,`direction`（uplink/downlink）,`result`（success/fail/dropped）
- **`ng_gateway_northward_message_latency_seconds`**（Histogram）
  - labels：`app_id`,`plugin`,`direction`,`result`
  - 说明：插件处理一次消息（`process_data` 或下行处理）的耗时
- **成功率**：推荐 PromQL 计算，例如：
  - `rate(success_total[5m]) / rate((success_total + fail_total)[5m])`

> 备注：`AppActor.metrics` 当前已有 `sent/dropped/errors/avg_latency_ms`，非常适合作为“每 app 基础指标”的数据源。

#### 4.3.3 Manager 级指标（全局路由与事件）

- **`ng_gateway_northward_apps_total`**（Gauge）
- **`ng_gateway_northward_apps_active`**（Gauge）
- **`ng_gateway_northward_events_received_total`**（Counter）
- **`ng_gateway_northward_data_routed_total`**（Counter）
- **`ng_gateway_northward_routing_errors_total`**（Counter）

### 4.4 资源（CPU/内存/连接/任务/句柄/网络 IO）

建议资源指标分两层：

- **节点/容器级**：交给 node_exporter / cAdvisor（更全面、更准）
- **进程级（网关自身）**：网关暴露关键 process 级指标，便于和背压/吞吐关联

建议网关进程侧暴露：

- **`ng_gateway_process_cpu_usage_ratio`**（Gauge，0~1 或 0~100）
- **`ng_gateway_process_memory_rss_bytes`**（Gauge）
- **`ng_gateway_process_open_fds`**（Gauge，Linux）
- **`ng_gateway_process_threads`**（Gauge）
- **`ng_gateway_network_bytes_sent_total`** / **`ng_gateway_network_bytes_received_total`**（Counter，尽可能按进程统计；若难以精确，可先做系统级）
- **`ng_gateway_tokio_tasks_active`**（Gauge，若能在核心 TaskTracker/管理点聚合）

> 当前 `SystemInfo` 与 `GatewayMetrics` 已能给到 CPU/Memory/Disk 等快照数据；生产级闭环建议把“连续时序”做成 Prometheus metrics，把“细节快照”继续保留在 `/status`。

### 4.5 Web（API/WS）作为运维数据通道（服务 UI，必须纳入方案）

> 说明：你明确不需要 Web 层（HTTP/WS）本身的指标监控（例如 API QPS、WS 连接数等）。因此本节只定义：**Web API / WebSocket 如何把“网关核心指标与实时数据”稳定、低开销地送到 UI 与 Grafana**。

#### 4.5.1 三条通道：Prometheus / 聚合指标 WS / 设备实时 WS

- **Prometheus 指标通道（给 Grafana/告警）**
  - `GET /metrics`：暴露网关核心指标（system/queue/southward/northward/control），用于 Grafana 仪表盘与告警
- **聚合指标实时通道（给 UI 首页/详情）**
  - `GET /api/ws/metrics`（建议新增）：推送聚合指标的 snapshot + update，用于 UI 首页实时总览、Channel/App 指标详情
- **设备数据实时通道（给现场调试）**
  - `GET /api/ws/monitor`（已存在）：以 device 维度推送 telemetry/attributes，用于“实时数据查看/问题复现”

> 设计目标：既能“实时”，又能控制帧风暴与前端渲染成本。聚合指标和设备实时数据必须拆分，避免互相干扰。

#### 4.5.2 `/api/ws/metrics`（聚合指标 WS）推荐协议与能力边界

推荐消息模型（低基数、可扩展）：

- Client -> Server：`subscribe` / `unsubscribe` / `ping`
  - `subscribe` 支持 scope：`global` / `channel` / `app`，并携带 `id`（当 scope != global）
- Server -> Client：`snapshot` / `update` / `error` / `pong`

性能与稳定性要求（最佳实践）：

- 后端做 **coalesce**：把高频更新合并为固定节奏（例如 200ms~1000ms）推送，避免 UI 卡顿
- 数据必须“聚合”：严禁在该通道里推送点位列表/大对象；device/point 级细节只走 `/api/ws/monitor` 或诊断 API
- 支持断线自动重连与补发 snapshot（前端已有 `useWebSocket` 方案可复用）

#### 4.5.3 `/metrics` 与鉴权边界（生产建议）

- `/metrics` 建议作为 **root public route**（与 `/health` 同级），方便 K8s 与 Prometheus scrape
- 若现场有安全要求，建议用部署侧手段保护（内网隔离 / ACL / mTLS sidecar / Ingress BasicAuth），不要把复杂鉴权逻辑耦合进指标导出层

### 4.6 UI 运维体验（混合模式：UI 实时关键指标 + Grafana 深度分析）

> 原则：UI 负责“**实时可用性** + **快速定位入口**”；Grafana 负责“**历史趋势** + **分位数** + **告警闭环**”。两者边界清晰，避免在 UI 内重造一套 Grafana。

#### 4.6.1 UI 首页（/home）实时总览（推荐 WebSocket 聚合推送）

首页建议做成“Gateway Overview”的轻量版本（卡片 + 状态灯 + 小趋势）：

- **关键卡片**（示例）
  - Southward：connected channels / reconnect rate / collect latency P95（近 1~5 分钟）
  - Northward：active apps / publish success ratio / dropped rate
  - Queues：关键队列水位（depth/capacity）与 drops
  - Process：CPU、RSS、threads/open_fds（Linux）
- **实时数据源**
  - 首选：订阅 `/api/ws/metrics` 的 `global` scope，频率 200ms~1s（可配置），后端做 coalesce
  - 历史趋势：跳转 Grafana（不要在 UI 内做长周期历史查询）
- **Grafana 边界**
  - 首页提供按钮：`打开 Grafana - Gateway Overview / Southward / Northward / Backpressure`

#### 4.6.2 南向通道管理页（Channel）指标详情（Drawer/Modal/Card）

在通道管理页提供“监控”入口（不强制重构成卡片，但建议支持卡片化详情）：

- 表格中为每行 Channel 提供：
  - `监控`（打开 Drawer/Modal）
  - `实时数据`（跳转到 `/api/ws/monitor` 的设备实时页，并预选 channel）
- 详情页/弹窗内容（按 channel 聚合）
  - 连接状态（state/connected/reconnect_total）
  - 采集周期耗时（Histogram 的 P50/P95/P99 展示为卡片）
  - 成功率/失败率（PromQL 计算或后端聚合）
  - IO bytes（若驱动可提供）
  - 关联队列水位（collector_outbound、control write serialization 等）
- 数据源策略
  - 实时：`/api/ws/metrics` 的 `channel` scope
  - 历史：跳转 Grafana 的 Southward Channels dashboard（带 channel_id 过滤）

#### 4.6.3 北向应用管理页（App）指标详情（Drawer/Modal/Card）

在北向应用管理页提供“监控”入口：

- 详情内容（按 app 聚合）
  - app connected/state/reconnect_total
  - messages_total（uplink/downlink）与 dropped/errors
  - plugin 处理耗时分位数（Histogram）
  - app queue/buffer 水位（若存在）
- 数据源策略
  - 实时：`/api/ws/metrics` 的 `app` scope
  - 历史：跳转 Grafana 的 Northward Apps dashboard（带 app_id/plugin 过滤）

#### 4.6.4 UI 与 Grafana 的明确边界（必须写死在产品设计里）

- UI 负责：
  - 关键实时状态、轻量趋势（秒级到分钟级窗口）、快速定位入口
  - 现场操作入口（启停 channel/app、临时 debug、打开实时设备数据）
- Grafana 负责：
  - 长周期趋势（小时/天）、直方图分位数、告警规则、事件关联分析
- 交互最佳实践：
  - UI -> Grafana 跳转携带 query 参数（例如 `var-channel_id=123`），形成“点一下就定位到对应面板”的闭环

## 5. “队列可观测性”落地关键：统一 Instrumented Queue（高性能、最小开销）

### 5.1 为什么必须做 wrapper

Tokio `mpsc` 的 bounded channel：

- 不能直接拿到精确 depth
- `try_send`/`send` 的“阻塞时间”也不会自动统计

而你最关键的诉求（queue depth/drops/blocked time）本质上要求：

- **发送/接收路径**都要更新 atomic gauges
- 对 `Block` 策略要统计**等待时间分布**

### 5.2 建议的实现策略（设计要点）

- 每条关键队列创建时注册一个 `queue_name`（有限集合）
- wrapper 内部维护：
  - `depth: AtomicU64`
  - `dropped_total: Counter`
  - `blocked_seconds: Histogram`
- `send/try_send/recv` 在极小开销下更新 depth（只做原子加减）

> 注意：depth 的一致性比绝对精确更重要；Prometheus 是趋势观测，足够支撑背压定位与告警。

---

## 6. tracing Span 语义与层级设计（必须先定标准）

### 6.1 Span 命名规范（建议）

- 使用点分层：`gateway.*` / `southward.*` / `collector.*` / `northward.*` / `control.*`
- Span name 必须表达“动作 + 作用域”，避免纯名词

建议最小层级：

- `gateway.init`
- `gateway.run`
- `southward.channel.run`
- `southward.device.collect`
- `collector.batch.process`
- `northward.route`
- `northward.app.process`
- `control.write_point`

### 6.2 字段（fields）规范（建议统一）

低基数字段（建议所有相关日志/Span 都带）：

- `channel_id`
- `driver`
- `device_id`（仅在设备级 Span 中）
- `app_id`
- `plugin_id` / `plugin`
- `queue`（队列相关事件）
- `result`（success/fail/timeout/dropped）
- `error_code`（有限集合）

高基数字段（仅 debug/诊断输出）：

- `device_name`、`point_key`、`topic`、原始 payload 摘要等

### 6.3 错误语义（建议）

- **可恢复错误**（重试/弱网）：`warn`，并记录 `retry_in_ms`、`attempt`
- **不可恢复错误**（配置错误/协议不匹配）：`error`，并记录 `error_code`
- 每个“关键动作 Span”应在结束时记录耗时（通过 `tracing` + metrics histogram 双轨）

---

## 7. 运行时“按 Channel 调整日志级别”方案（产品级可用）

### 7.1 tracing 过滤的现实约束

`tracing` 的过滤主要基于 **metadata（target/level）**，而不是基于运行时字段（如 `channel_id`）。因此要做到“只打开某个 channel 的 debug”且不影响全局性能，最佳实践是：

- **为每个 Channel 实例生成稳定的 `target`**
- 使用可热更新的 filter（reload）对特定 target 下放 DEBUG

### 7.2 推荐方案（强烈建议）

- 每个 Channel 实例在创建时确定一个 target（示例）：
  - `ng_gateway.southward.channel.<channel_id>.<driver>`
- 该 Channel 内的所有 `tracing::debug!/info!/warn!/error!` 都用这个 target 输出（或至少关键 IO/协议日志用这个 target）。
- Logger 侧使用可 reload 的 filter（例如 `EnvFilter`/`Targets` 的 reload handle）：
  - 默认：全局 `info`
  - 某 channel 开启调试：仅对 `ng_gateway.southward.channel.123.modbus` 设置 `debug`

### 7.3 API 与生命周期（产品化要求）

建议提供：

- `POST /api/v1/channels/{channel_id}/log-level`
  - body：`{"level":"debug","ttl_seconds":600}`
- `GET /api/v1/channels/{channel_id}/log-level`
- TTL 到期自动回收（防止现场忘关导致性能与磁盘压力）
- 变更记录写入审计日志（谁在何时打开了哪个 Channel 的 Debug）

---

## 8. Grafana 闭环：仪表盘与告警（必须同时交付）

### 8.1 推荐仪表盘（最小可交付集）

- **Gateway Overview**
  - 运行状态、QPS/吞吐、总错误率、CPU/内存、关键队列水位
- **Backpressure & Queues**
  - 各队列 depth/capacity、水位百分比、drops、blocked time P95/P99
- **Southward Channels**
  - 每 channel 连接状态、重连次数、采集延迟 P95、读点成功率、IO bytes
- **Northward Apps**
  - 每 app 连接状态、消息成功率、dropped、处理延迟、重连次数
- **Control Plane**
  - 写点请求量、排队等待时间、超时率、按 channel 分布

### 8.2 推荐告警（起步版）

- **Queue saturation**：`queue_depth / queue_capacity > 0.8` 持续 N 分钟
- **Drops detected**：`rate(queue_dropped_total[5m]) > 0`
- **Blocked time spike**：`histogram_quantile(0.99, rate(queue_blocked_seconds_bucket[5m])) > X`
- **Northward publish success rate low**：成功率低于阈值（按 app）
- **Southward collect latency high**：采集延迟 P95 超阈值（按 channel）
- **Reconnect storm**：`rate(reconnect_total[5m])` 过高
- **Resource pressure**：RSS 接近上限、CPU 长期高位

---

## 9. 落地计划（分阶段、可验证、允许破坏式重构）

### 9.1 Phase 0：确定导出闭环与基础设施（1~2 天）

- 网关暴露 `/metrics`（Prometheus 格式）
- 部署 Prometheus + Grafana + Loki（或接入现有平台）
- 先把现有 system 的指标跑通闭环
- 打通运维数据通道（4.5）：`/metrics`（给 Grafana/告警）+ `/api/ws/metrics`（给 UI 实时总览/详情）

### 9.2 Phase 1：队列与背压指标体系（最优先）

- 引入统一 Instrumented Queue wrapper
- 把关键队列全部纳入 `queue_depth/capacity/drops/blocked`
- Grafana 上线 Backpressure Dashboard + 核心告警

### 9.3 Phase 2：Southward/Northward 指标补齐

- Southward：channel 状态、采集延迟、IO bytes、成功率
- Northward：app 连接状态、上/下行计数、失败率、延迟、重连
- 把 `AppActor.metrics` 对接到 Prometheus 指标导出
- UI 运维体验落地（4.6）：首页实时总览 + Channel/App 指标详情

### 9.4 Phase 3：tracing 语义标准化 + 按 Channel 动态日志级别

- 统一 Span 命名/字段规范
- 实现 per-channel log level + TTL + 审计
- 配套 UI/接口（面向运维与现场调试）

### 9.5 Phase 4（可选）：Trace 导出到 Tempo（需要时再上）

- 仅对关键路径做 Trace（采集 → 路由 → 上报、写点控制面）
- 采样策略（默认低采样，debug 时提升）

---

## 10. 删除 OTel 代码与依赖（本方案必须执行）

### 10.1 执行范围（必须删除）

- 删除 workspace 依赖：
  - `opentelemetry`
  - `opentelemetry_sdk`
  - `opentelemetry-otlp`
- 删除所有 OTel 指标初始化与调用：
  - `NGAppContext::init_metrics()`（OTLP exporter/PeriodicReader/Resource/service_name）
  - `global::meter(...)` 等
  - `ng-gateway-common/src/metrics.rs`（OTel Meter 采集系统指标）

### 10.2 替代实现（Prometheus）

- 在 `ng-gateway-common` 提供统一的 Prometheus `Registry` 与指标定义（Counter/Gauge/Histogram）。
- 在 `ng-gateway-web` 暴露 `GET /metrics`，返回 Prometheus 文本格式。
- 在 Helm chart（已有 `PodMonitor`/`ServiceMonitor`）里保持 `path: /metrics` 与 `port: http` 一致。

