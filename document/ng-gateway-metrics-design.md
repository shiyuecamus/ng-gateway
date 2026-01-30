## NG Gateway：Metrics（Prometheus + UI Snapshot）最佳实践设计与闭环 Phase 计划

> 目标：给 `ng-gateway` 的 Metrics 子系统提供一个**语义完整、可治理、可热调、成本可控、重启可持久化**的产品级设计。
>
> 本文以你们现有实现为底座（`NGMetricsHub`、`GET /metrics`、`GET /api/ws/metrics`），重点补齐控制面（Settings / 开关 / 粒度 / 护栏）与闭环实施计划。

---

## 0. 背景与现状对齐（你们已经有的能力）

你们并不是“从 0 开始做 metrics”，目前代码已经具备非常关键的最佳实践底座：

- **Prometheus 暴露（HTTP Pull）**：`ng-gateway-web/src/api/public/metrics.rs`
  - 根路径 `GET /metrics`，刻意挂载在 `/api` 之外，适配 K8s `ServiceMonitor/PodMonitor`
- **UI 聚合指标流（WebSocket）**：`ng-gateway-web/src/api/v1/ws/metrics.rs`
  - `GET /api/ws/metrics`，支持 `global/channel/app/device` scope
  - 服务端 coalescing（固定 tick），`MissedTickBehavior::Skip`
  - interval clamp（当前为硬编码 200ms~5000ms）
- **单进程唯一数据源**：`ng-gateway-common/src/metrics/mod.rs` 的 `NGMetricsHub`
  - 单 `Registry`（namespace `ng_gateway`）
  - scrape 前刷新 system/queue 等 scrape-time 指标
  - 同时生成 Prometheus exposition 与 REST/WS snapshot DTO（`ng-gateway-models/src/core/metrics.rs`）
  - 设计约束：禁止 device/point 进入 Prometheus labels（低基数）

**本文要补齐的是**：`general.metrics` 的配置域 + 统一开关语义 + 粒度开关的“真正 no-op”闭环 + WS 快照的共享后台任务与护栏治理。

---

## 1. 设计目标与非目标

### 1.1 设计目标（必须同时满足）

- **Single Source of Truth**：Prometheus 与 UI snapshot 必须从同一数据源派生（`NGMetricsHub`），杜绝“双算导致漂移”。
- **Low overhead by default**：
  - 热路径更新为 `Atomic`/预解析 handles（你们已做）
  - scrape/WS snapshot 的 CPU/内存开销必须有上界（cache + coalescing + 限流）
- **Bounded cardinality**：
  - Prometheus labels 的维度必须可证明上界有限
  - 禁止 device/point 进入 labels
- **统一开关语义（闭环）**：
  - 一个总开关 + Prometheus 子开关 + UI 子开关
  - 关闭后：endpoint 不可访问 + 指标不再更新（性能可控）
- **可运维**：
  - 关键护栏可配置且有默认值（interval、cache TTL、max subs、rows limit）
  - 行为明确：404/close/error code，避免“看起来开着但数据冻结”的误导

### 1.2 明确非目标（本设计不覆盖）

- OpenTelemetry metrics/traces、remote write、分布式 tracing
- 高基数长尾诊断（每 device / 每 point 的 Prometheus 指标）
  - 该类诊断必须走 `/api/ws/monitor` 或专用诊断 API，不进入 Prometheus

---

## 2. 配置模型（`general.metrics`，Settings 唯一权威）

> 约束：所有开关、粒度与护栏都必须进入 `Settings`，并通过 `PATCH /system/settings/metrics` 统一 apply + persist。
>
> 现状：`ng-gateway-models/src/settings.rs` 尚未包含 metrics 域，因此需要新增该域（破坏式重构可接受）。

### 2.1 推荐 `gateway.toml` 结构

```toml
[general.metrics]
# Total switch: controls BOTH Prometheus and UI metrics.
enabled = true

[general.metrics.prometheus]
enabled = true

# Best practice: keep `/metrics` stable for K8s.
path = "/metrics"

# Scrape protection: cache encoded text payload for a short TTL
# to avoid CPU spikes on multi-scrape / misconfigured probes.
scrape_cache_ttl_ms = 1000

[general.metrics.ui]
enabled = true

# Server-side guardrails for `/api/ws/metrics`.
default_interval_ms = 1000
min_interval_ms = 200
max_interval_ms = 5000
max_subscriptions_per_connection = 16

# Device scope payload guard (channel device list can be huge).
device_rows_limit = 2000

[general.metrics.granularity]
# Keep aligned with NGMetricsHub sub-hubs.
system = true
queues = true
collector = true
southward_manager = true
southward_channel = true
control_plane = true
northward_manager = true
northward_app = true

# UI-only: device rows stream (NOT a Prometheus label dimension).
ui_device_rows = true
```

### 2.2 配置语义说明（关键字段）

- **`general.metrics.enabled`**：
  - `false`：Prometheus 与 UI metrics 都不可用，且指标更新应变为 no-op
- **`general.metrics.prometheus.*`**：
  - 控制 `GET /metrics` 暴露与 scrape cache
- **`general.metrics.ui.*`**：
  - 控制 `GET /api/ws/metrics` 以及 interval/subscription/device rows 护栏
- **`general.metrics.granularity.*`**：
  - 控制“是否记录/刷新”某类指标（recording gate）
  - 对带 labels 的维度（per-channel/per-app）还必须触发 registry reconcile（见 4.3）

---

## 3. 统一开关语义（Exposure + Recording 两层闭环）

Phase 验收里“关闭后不再更新（性能可控）”必须靠设计保证，因此开关语义分两层：

### 3.1 暴露层（Exposure）：是否可访问

- **总开关关闭**：`general.metrics.enabled=false`
  - `GET /metrics`：返回 404（推荐 404，明确“未开启”）
  - `GET /api/ws/metrics`：握手后立即发 error frame（`code="metrics_disabled"`）并关闭连接
- **仅关闭 Prometheus**：`enabled=true` 且 `prometheus.enabled=false`
  - `GET /metrics`：404
  - `/api/ws/metrics`：仍可用（若 `ui.enabled=true`）
- **仅关闭 UI**：`enabled=true` 且 `ui.enabled=false`
  - `/api/ws/metrics`：`code="ui_metrics_disabled"` 并关闭
  - `GET /metrics`：仍可用（若 `prometheus.enabled=true`）

### 3.2 采集/更新层（Recording）：是否继续写入指标

所有 metrics 更新点必须经过一个**极低开销 gate**：

- 总开关关闭：直接 no-op
- 粒度开关关闭：该类别 no-op

同时，scrape-time refresh（例如你们 `refresh_for_scrape()` 刷新 system/queue）也必须受 gate 控制：

- `granularity.system=false`：不再刷新 system metrics
- `granularity.queues=false`：不再刷新 queue depth gauges

---

## 4. Prometheus 最佳实践：低基数约束 + 生命周期治理

### 4.1 命名与 labels 约束（写死）

- **命名空间**：统一 `ng_gateway_*`（registry 已设定 namespace `ng_gateway`）
- **labels 允许**（可证明上界）：`channel_id`、`app_id`、`plugin_id`、`driver`
- **labels 禁止**：`device_id`、`point_id`、`point_key`、任意自由字符串

### 4.2 Histogram 使用原则（防内存爆炸）

- 只有“确实需要分布/分位数”的延迟类指标才用 histogram
- histogram 不能与高基数 labels 组合
- 需要均值优先用 sum/count 派生（你们 DTO 已在做 `avg_*_ms`）

### 4.3 Series 生命周期治理（避免 zombie series）

你们已有 `register_*` / `unregister_*`，本设计要求把它做成**治理闭环**：

- **注册时机**：channel/app 进入 runtime registry 即注册，缓存 handles
- **反注册时机**：channel/app 被移除时必须反注册
- **粒度开关与反注册的关系（关键）**：
  - 关闭 `southward_channel` / `northward_app` 等“带 label series”的粒度后，必须触发一次 reconcile：
    - 立刻反注册现存 series，基数与内存占用立刻下降
  - 再次打开时，从 runtime topology 快照重建注册

推荐实现形态（允许破坏式重构）：

- 增加一个 `MetricsReconciler`
  - 输入：metrics config + runtime topology 快照（channels/apps/driver/plugin 等）
  - 输出：执行 register/unregister，使 registry 状态与 settings/topology **收敛**
- 触发时机：
  - topology 变更（add/remove/restart）
  - `PATCH /system/settings/metrics` 成功后

---

## 5. UI Metrics 最佳实践：从“每连接循环”到“共享后台任务”

### 5.1 现状问题

`/api/ws/metrics` 当前在每个连接里做周期性 snapshot build；当 UI 多开/多用户并发时：

- 重复工作线性增长（尤其是 device rows 大 payload）
- interval clamp 仍是硬编码，缺少配置闭环

### 5.2 最佳实践架构：`MetricsStreamHub`（按 scope/id 共享）

新增进程内 `MetricsStreamHub`（不是 Prometheus registry，而是 WS 快照服务）：

- key = `(scope, id)`（global/channel/app/device）
- value = `tokio::sync::broadcast::Sender<Bytes/Value>`
- 生命周期：引用计数
  - 第一个订阅者出现：启动后台 tick task
  - 最后一个离开：自动停止 task
- tick interval：
  - 来自 settings 的 `min/default/max`
  - 可选：结合客户端 hint 取最小值，但必须 clamp
- coalescing：missed tick 直接 skip，只推最新快照

WS session 只负责：

- 订阅/退订（维护订阅集）
- 从 `MetricsStreamHub` 获取 receiver 并转发到客户端

这使得 “UI metrics 快照后台任务（受开关与 interval 控制）”真正成立：

- 后台任务是共享的（按 scope/id），不是每连接重复
- `ui.enabled=false` 时可统一停止所有 streams
- interval 来自 Settings，运行时可热更新

### 5.3 Device scope 护栏（防大 payload）

device rows 的本质是“低基数聚合 UI”，但 payload 可能非常大，因此至少需要：

- `device_rows_limit`（超过则截断/只发 topN，并在 payload 中标记 `truncated=true`）
- `granularity.ui_device_rows=false` 时：拒绝 device scope（或返回空）

> 后续若要更强：把 device scope 升级为分页协议（page/pageSize/排序字段），但这属于 UI/协议扩展，不强制塞进本 Phase。

---

## 6. `/system/settings/metrics`（控制面闭环）

### 6.1 API 目标

- `GET /system/settings/metrics`：返回 view（包含当前 effective 值、来源 default/file/env）
- `PATCH /system/settings/metrics`：一次完成：
  - apply：热更新 runtime metrics config（低开销读）
  - persist：原子回写 `gateway.toml`
  - impact：必要时触发 `MetricsReconciler` 与 WS streams 更新

### 6.2 ApplyResult 最小闭环字段（建议）

- `applied` / `persisted`
- `changed_keys` / `blocked_by_env`
- `impact`：`hot_apply` 或 `restart_component(metrics_snapshot)`（若你选择重建 stream hub）

---

## 7. Phase 计划（闭环交付 + 验收）

> Phase 目标：把“开关语义、粒度 no-op、Prom scrape 护栏、UI 共享后台任务”全部做成可验证的闭环。

### Phase 5.1：Settings 域落地（`general.metrics`）+ `/system/settings/metrics` 闭环

- 新增 `general.metrics` 到 `gateway.toml` 与 `Settings`
- 实现 `GET/PATCH /system/settings/metrics`（apply + persist + impact）

验收：

- Metrics block 可读
- `PATCH` 后立刻生效且重启后仍生效

### Phase 5.2：Prometheus 暴露开关 + scrape cache TTL

- `/metrics` 受 `metrics.enabled` 与 `prometheus.enabled` 双重控制
- 实现 `scrape_cache_ttl_ms`（短 TTL 缓存已编码 text payload）

验收：

- `metrics.enabled=false` 或 `prometheus.enabled=false`：`GET /metrics` 返回 404
- 高并发 scrape（例如 10QPS）CPU 抖动可控（cache 命中）

### Phase 5.3：粒度开关落地（recording gate + reconcile）

- 所有指标写入点增加 gate（总开关 + granularity）
- 对 per-channel/per-app 等带 labels 的粒度实现 reconcile：
  - 关闭时反注册现存 series
  - 打开时按 topology 重建

验收：

- 粒度关闭后对应指标不再变化（Prom family 不再更新）
- 对应 labeled series 被移除，而不是“冻结值”

### Phase 5.4：UI metrics 开关 + interval/limit 护栏配置化

- `/api/ws/metrics` 受 `metrics.enabled` 与 `ui.enabled` 控制
- interval clamp 与 subscription/rows limit 全部迁移到 settings

验收：

- `ui.enabled=false`：WS 返回 `ui_metrics_disabled` 并关闭
- 修改 interval/limit 后对新连接与已有连接按既定策略生效

### Phase 5.5：UI 快照共享后台任务（`MetricsStreamHub`）

- 引入 `MetricsStreamHub`（按 scope/id 共享 tick task）
- WS session 仅转发 broadcaster
- （可选但建议）增加自监控指标：连接数、streams 数、build 耗时/次数

验收：

- 多连接下 snapshot build 次数与连接数解耦（从 O(connections) 降为 O(scopes)）
- CPU 不随 UI 多开线性增长（共享任务 + coalescing 生效）

---

## 8. 验收清单（最终闭环）

- **总开关关闭**：Prometheus 与 UI metrics 都关闭（不可访问 + 不更新）
- **粒度开关关闭**：对应指标不再更新（性能可控）
- **重启后仍生效**：所有变更持久化到 `gateway.toml`

