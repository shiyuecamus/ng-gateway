## NG Gateway Metrics 可观测体系 V2（单一权威数据源 + Prometheus/Snapshot 双输出）详细设计与计划

> 目标：在允许破坏性重构的前提下，把 NG Gateway 的运行时可观测能力收敛为**语义统一、性能极致、质量可验证**的一套体系：
>
> - **唯一权威指标数据源**：`ng-gateway-common/src/metrics`（以下简称 common metrics）
> - **Prometheus /metrics**：面向 Grafana/告警（时序、分位数、rate）
> - **Snapshot（REST/WS）**：面向 UI 实时态与定位入口（低开销、低基数、聚合视图）
> - **严格禁止双口径**：任何指标语义只允许定义一次；Prometheus 与 Snapshot 均从同一权威状态“投影”生成

---

## 1. 背景与当前问题（基于仓库现状）

### 1.1 已有基础（好消息）

- common 已存在全局唯一 Prometheus `Registry` 与 `/metrics` 端点：
  - `ng-gateway-common/src/metrics/mod.rs`：`REGISTRY` + `gather_prometheus_text()`
  - `ng-gateway-web/src/api/public/metrics.rs`：root `/metrics` 暴露
- 队列与背压（queue/backpressure）已经采用了**最佳实践雏形**：
  - 热路径只更新 `AtomicU64 depth`
  - scrape 前集中 `refresh_all_queue_depths()` 将 atomic 刷到 `IntGauge`

### 1.2 主要缺口（必须破坏性重构解决）

- **双口径/双存储**：Prometheus 指标（common）与 `GatewayMetrics/GatewayStatus`（models/core）并存，且更新节奏、字段语义可能漂移。
- **高频 Snapshot 代价过高风险**：
  - `get_status()` 里每次做 system refresh（`sysinfo.refresh_all()`）会导致 UI/WS 抖动与额外开销。
  - 如果未来把 snapshot 构建绑定到 `Registry::gather()`，会更糟（gather 会构造 metric families，不适合高频路径）。
- **指标体系尚未统一**：collector/southward/northward/control 仍缺少完整产品级指标与一致 labels 策略。

---

## 2. 设计原则（必须严格遵守）

### 2.1 单一真源（Single Source of Truth）

- **权威状态在 common metrics**：以原子/轻锁结构维护运行时指标与状态。
- **Prometheus 与 Snapshot 只做投影**：
  - Prometheus：面向时序分析/告警（Counter/Gauge/Histogram）
  - Snapshot：面向 UI 实时态（聚合、低基数、快照语义）

> Prometheus crate 的 `Gauge::get()` 等 API 可用于读取，但**不把 Prometheus 指标对象当作权威状态存储**。权威状态应由我们自己控制的原子结构承载，避免 exporter 实现细节与锁竞争影响核心路径。

### 2.2 低开销热路径

- 热路径只允许：
  - 原子增减（`fetch_add/fetch_sub/store/load`，`Ordering::Relaxed` 为主）
  - 少量枚举状态 set
  - 极少量受控锁（仅在“稀疏事件”或“后台聚合”）
- 禁止热路径：
  - 字符串拼接（除初始化/注册阶段）
  - Prometheus label 查表（`*_Vec.get_metric_with_label_values`）在热路径重复发生
  - `sysinfo.refresh_all()`、`Registry::gather()` 等重操作

### 2.3 标签与基数（Cardinality）是生死线

- 生产默认低基数 labels（必须）：
  - `queue`（有限集合）
  - `channel_id`（数量可控）
  - `driver`（有限集合）
  - `app_id`（数量可控）
  - `plugin`（有限集合）
  - `direction`（有限集合 tx/rx 或 uplink/downlink）
  - `result` / `reason`（有限集合）
- 明确禁止：
  - `device_id/point_id/topic/ip/error_message` 作为 Prometheus labels

### 2.4 命名与单位

- 统一前缀：`ng_gateway_`
- Counter：`_total` 结尾
- 单位：`_seconds`、`_bytes`、`_ratio`（0~1）、`_count`
- 状态类：用 `Gauge` 表达枚举（映射到整数）或 bool（0/1）

---

## 3. 模块边界与依赖方向（必须写死）

### 3.1 crate 职责

- **`ng-gateway-common`**
  - `metrics/`：唯一权威指标数据源（注册、热路径更新、scrape 刷新、snapshot 生成）
  - 对外暴露：`record_*`、`snapshot_*`、`gather_prometheus_text()`

- **`ng-gateway-models`**
  - 仅定义 DTO：`*Snapshot`（`Serialize/Deserialize`）
  - 禁止：Atomic/RwLock/采集逻辑/后台任务/Prometheus 依赖

- **`ng-gateway-core`**
  - 业务逻辑与 runtime 对象生命周期
  - 只调用 common metrics 的 record API
  - 不维护独立 metrics 存储（删除 `Arc<RwLock<GatewayMetrics>>` 等）

- **`ng-gateway-web`**
  - `/metrics`：调用 common `gather_prometheus_text()`
  - `/api/ws/metrics`：调用 common `snapshot_*`，按节奏推送（coalesce）

### 3.2 单向依赖

`core/web` → `common::metrics` →（输出）→ Prometheus 文本 / Snapshot DTO（models）

禁止出现：

- `common` 依赖 `core`
- `models` 依赖 `common/prometheus/sysinfo`

---

## 4. 统一指标模型：`MetricsHub`（权威状态）

### 4.1 总体结构

在 `ng-gateway-common/src/metrics` 下新增/扩展：

- `hub.rs`：`MetricsHub` 全局实例（`static Lazy<MetricsHub>`）
- `system.rs`：系统/进程维度（scrape 前 refresh）
- `queue.rs`：队列/backpressure（已存在，继续强化）
- `collector.rs`：采集引擎指标（周期/耗时/成功率/并发）
- `southward.rs`：channel 连接/重连/采集周期/读点/IO（按 channel 聚合）
- `northward.rs`：app 连接/重连/消息/延迟/队列（按 app 聚合）
- `control.rs`：控制面写点排队/超时/执行耗时
- `snapshot.rs`：所有 Snapshot 组装（global/channel/app 等）

其中：

- **热路径数据结构**：原子计数 + 少量 `DashMap`/`OnceCell` 句柄缓存
- **Prometheus 句柄**：只作为输出层存在，并在“注册阶段”解析好 child metric handles
- **scrape-time refresh**：在 `gather_prometheus_text()` 里集中调用各域 refresh

### 4.2 句柄预解析（避免热路径 label 查表）

对 per-channel / per-app 的指标，采用：

- `DashMap<Key, Arc<Handles>>`
  - `Key`：`(channel_id, driver)` 或 `(app_id, plugin)`
  - `Handles`：预解析的 `Counter/Gauge/Histogram` 子指标对象

注册/解析发生在：

- channel/app runtime 创建时（`core` 调用 common metrics 的 `register_*`）
- 或首次 record 时（惰性），但必须保证解析只发生一次

### 4.3 Snapshot 的来源（绝不从 gather() 反推）

Snapshot 仅从 `MetricsHub` 的权威原子结构读取组装：

- **global snapshot**：汇总 totals + system/process + 关键队列水位 + manager 汇总
- **channel snapshot**：channel 状态 + 采集延迟分位数（近似或窗口化）+ drops/blocked + IO totals
- **app snapshot**：连接状态 + messages totals + drops/errors + latency 分布摘要

Prometheus 的 histogram 分位数建议交给 Grafana/PromQL；Snapshot 侧如果需要 P95 等：

- 方案 A（推荐，轻）：Snapshot 不直接给 P95，只给基础计数/最近均值/最近窗口 stats（例如 EMA）
- 方案 B（可选，重）：在 common 维护一个轻量 rolling window/CKMS/t-digest（复杂度较高）

---

## 5. 指标体系（Prometheus）详细设计

> 说明：以下为“最小产品级闭环”指标清单（与 `observability-metrics-monitor-plan.md` 一致），并按域给出 labels 与语义。实现时应在 common metrics 中定义，统一注册到全局 registry。

### 5.1 Queue / Backpressure（最高优先级）

- `ng_gateway_queue_depth`（Gauge）
  - labels：`queue`
  - 语义：队列当前深度（best-effort，一致性优先）
- `ng_gateway_queue_capacity`（Gauge）
  - labels：`queue`
  - 语义：固定容量
- `ng_gateway_queue_dropped_total`（Counter）
  - labels：`queue`,`reason`（`full|timeout|closed|buffer_full|expired`）
- `ng_gateway_queue_blocked_seconds`（Histogram）
  - labels：`queue`
  - 语义：send 侧等待时间分布（Block 策略或 send_timeout 的实际等待）

实现要求：

- 热路径仅更新 atomic depth 与 counter/histogram（其中 histogram observe 属于热点但可接受，需评估）
- depth gauge 由 scrape-time refresh 统一 set（已实现）
- 队列名称集合必须有限，注册时固定

### 5.2 System / Process（建议补齐 process 维度）

短期可保留 system 级（已存在）：

- `ng_gateway_system_cpu_usage_ratio`
- `ng_gateway_system_memory_usage_ratio`
- `ng_gateway_system_disk_usage_ratio`

中期建议补齐 process 级（更利于定位“网关自身压力”）：

- `ng_gateway_process_cpu_usage_ratio`
- `ng_gateway_process_memory_rss_bytes`
- `ng_gateway_process_threads`
- `ng_gateway_process_open_fds`（Linux）

实现要求：

- 采集在 scrape-time 进行，并做缓存（避免 WS/snapshot 触发 refresh）

### 5.3 Collector（采集引擎）

- `ng_gateway_collector_collections_total{result=...}`（Counter）
  - result：`success|fail|timeout`
- `ng_gateway_collector_cycle_seconds{result=...}`（Histogram）
- `ng_gateway_collector_tasks_active`（Gauge）
- `ng_gateway_collector_semaphore_permits{state=...}`（Gauge）
  - state：`current|available`

### 5.4 Southward（按 channel 聚合）

- `ng_gateway_southward_channel_state{channel_id,driver}`（Gauge，枚举）
  - Connecting=1/Connected=2/Reconnecting=3/Disconnected=4/Failed=5
- `ng_gateway_southward_channel_connected{channel_id,driver}`（Gauge，0/1）
- `ng_gateway_southward_channel_reconnect_total{channel_id,driver}`（Counter）
- `ng_gateway_southward_collect_cycle_seconds{channel_id,driver,result}`（Histogram）
- `ng_gateway_southward_point_read_total{channel_id,driver,result}`（Counter）
  - result：`success|fail`
- `ng_gateway_southward_io_bytes_total{channel_id,driver,direction}`（Counter）
  - direction：`tx|rx`

说明：

- device 维度的“最后一次错误 message/时间戳”走诊断 API 或 snapshot（受控），不进入 Prometheus labels。

### 5.5 Northward（按 app 聚合）

- `ng_gateway_northward_app_state{app_id,plugin}`（Gauge，枚举）
- `ng_gateway_northward_app_connected{app_id,plugin}`（Gauge，0/1）
- `ng_gateway_northward_app_reconnect_total{app_id,plugin}`（Counter）
- `ng_gateway_northward_messages_total{app_id,plugin,direction,result}`（Counter）
  - direction：`uplink|downlink`
  - result：`success|fail|dropped`
- `ng_gateway_northward_message_latency_seconds{app_id,plugin,direction,result}`（Histogram）

### 5.6 Control Plane（写点/下行）

- `ng_gateway_control_write_requests_total{channel_id,driver,result}`（Counter）
  - result：`success|fail|timeout|rejected`
- `ng_gateway_control_write_queue_blocked_seconds{channel_id,driver}`（Histogram）
  - 语义：写请求在“序列化/排队”阶段等待时间
- `ng_gateway_control_write_execute_seconds{channel_id,driver,result}`（Histogram）
  - 语义：实际执行耗时

---

## 6. Snapshot（REST/WS）详细设计

### 6.1 Snapshot DTO 规范（models）

- 所有对外结构必须 `*Snapshot` 结尾。
- 只包含：
  - 数值类型、枚举、时间戳（建议 `chrono::DateTime<Utc>`）
  - 低基数维度（global/channel/app）
- 禁止：
  - 原子类型、锁、运行时对象引用、Prometheus 类型

推荐结构（示例命名，可按现有 `GatewayStatusSnapshot/ChannelStats/NorthwardAppStats` 演进）：

- `GatewayObservabilitySnapshot`
- `ChannelObservabilitySnapshot`
- `AppObservabilitySnapshot`

> 兼容策略：可先保留现有 Snapshot 名称，但把“生产者”迁移到 common，最终 models 只保留 DTO。

### 6.2 Snapshot 生产者（common）

在 `ng-gateway-common/src/metrics/snapshot.rs` 提供：

- `snapshot_global() -> GatewayStatusSnapshot`（或新 DTO）
- `snapshot_channel(channel_id) -> ChannelStatsSnapshot`
- `snapshot_app(app_id) -> NorthwardAppStatsSnapshot`

约束：

- 只读原子/缓存；绝不触发 `sysinfo.refresh_all()`、绝不调用 `Registry::gather()`
- 单次构建应接近 O(1)

### 6.3 WS `/api/ws/metrics` 的节流与合并（coalesce）

- 服务端固定 tick（建议默认 500ms，可配置 200ms~1s）
- 对每个连接/订阅 scope：
  - tick 到来时从 common 读取 snapshot 并发送
  - 若订阅变更，立即发送一次 snapshot
- 严禁把 device/point 列表或大对象通过该通道推送

---

## 7. 删除与重构清单（破坏性重构）

> 目标：消除双口径与重复采集，把所有 metrics/snapshot 生产集中到 common。

### 7.1 必须删除/迁移

- core 内独立维护的聚合 metrics 存储与任务（示例）：
  - `ng-gateway-core/src/gateway.rs` 中 `metrics: Arc<RwLock<GatewayMetrics>>`
  - `start_metrics_collection()` 定时写锁更新（迁移为 record API 或后台聚合由 common 统一负责）
- models 内任何非 Snapshot 的“运行时原子/锁”指标结构：
  - `NorthwardAppMetrics`、`NorthwardManagerMetrics` 这类结构应迁移到 common（models 仅保留 `*Snapshot`）

### 7.2 common 中新增/强化

- `MetricsHub` 权威状态与各域模块
- per-channel/per-app handles 缓存
- collector/southward/northward/control 全量指标定义与 record API
- snapshot 组装器

---

## 8. 迁移计划（Phase 0 ~ Phase 3）

> 每个阶段都必须有“验收标准”，并可独立上线。

### 8.1 Phase 0：基座固化（1~2 天）

- 固化 `ng-gateway-common::metrics` 的三件套：
  - `registry()` + `gather_prometheus_text()` + scrape-time refresh 框架
- `/metrics` 路由确认在 root 且稳定（已存在）
- 输出最小可用指标：
  - system（已存在）
  - queue（已存在/继续完善）

**验收标准**

- `GET /metrics` 稳定输出，Prometheus 可 scrape
- 指标命名符合统一前缀与单位规范
- 压测/运行时无明显性能回退

### 8.2 Phase 1：彻底统一 queue/backpressure（1~3 天）

- 把所有关键 bounded queue 接入 instrumented queue：
  - collector outbound
  - northward events
  - per-app data/buffer queue（若存在）
  - control write serialization 等
- 补齐 drop reason、blocked_seconds、capacity

**验收标准**

- Grafana Backpressure 面板可做：水位、drops、blocked P99
- 现场能够回答“丢没丢、堵没堵、堵在哪条队列”

### 8.3 Phase 2：Southward/Northward/Collector 全量指标 + Snapshot 生产迁移（3~7 天）

- 把 northward 的原子 metrics 从 models/core 迁移到 common
- 把 `GatewayStatusSnapshot` 生产迁移到 common（web/ws 与 commands 调用 common snapshot）
- 增加 collector/southward/northward/control 的 Prometheus 指标与 record 埋点

**验收标准**

- `/api/ws/metrics` 高并发订阅下无抖动（CPU 平稳、无频繁 sys refresh）
- Prometheus 指标与 Snapshot 语义一致（同一口径）
- 删除 core 中 `start_metrics_collection()` 等重复逻辑

### 8.4 Phase 3：告警/仪表盘闭环 + 文档固化（2~5 天）

- 仪表盘（最小交付集）：
  - Gateway Overview
  - Backpressure & Queues
  - Southward Channels
  - Northward Apps
  - Control Plane
- 告警（起步版）：
  - queue saturation
  - drops detected
  - blocked time spike
  - reconnect storm
  - publish success rate low

**验收标准**

- Grafana 一键定位：UI -> Grafana（带变量过滤）
- 告警规则能在故障演练中触发并定位到具体 channel/app/queue

---

## 9. 测试与性能验证（必须做）

### 9.1 正确性（语义一致）

- 单元测试：
  - record API 写入后 snapshot 输出符合预期
  - drop reason 映射固定且有限
- 集成测试：
  - `/metrics` 包含预期指标
  - `/api/ws/metrics` 订阅/退订/snapshot 正常

### 9.2 性能（极致路径）

- 热路径检查：
  - 只做原子操作（确认无 label 查表/字符串拼接）
- 高并发 WS：
  - 多连接订阅 global/channel/app，tick 500ms
  - CPU 使用与延迟稳定，无周期性尖峰（避免 sys refresh）

---

## 10. 里程碑与交付物

- **交付物 A**：统一指标实现（common metrics + record API + prometheus 输出）
- **交付物 B**：Snapshot 统一生产（common snapshot + WS/REST 使用）
- **交付物 C**：Grafana dashboard + alert rules（最小闭环）
- **交付物 D**：开发规范（命名/labels/热路径约束）与代码审查清单

---

## 11. 代码规范与 Review 清单（落地保障）

- 新增任何指标必须回答：
  - 指标类型（Counter/Gauge/Histogram）
  - 单位与命名
  - labels 集合是否有限？是否会爆基数？
  - 热路径是否需要 child handle 缓存？
- Snapshot 字段必须回答：
  - UI 需要它做什么？是否可以用 PromQL/Grafana 计算替代？
  - 频率与开销是否可控？
  - 是否会引入高基数（禁止 device/point 明细）


