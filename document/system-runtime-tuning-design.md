## NG Gateway：系统设置运行时调参（Settings 唯一权威）产品级设计与闭环计划

> 本文只覆盖你明确要做的运行时配置维度，并保证两个闭环：
>
> - **运行时立刻生效**（Hot Apply 或明确的组件/通道重启）
> - **重启后仍生效**（持久化落盘 + 启动加载顺序确定）

---

## 0. 范围与非目标

### 0.1 本次要做的运行时配置范围（必须全覆盖）

- **采集引擎**：`collection_timeout_ms`、`max_concurrent_collections`、`retry_policy`、`outbound_queue_capacity`
- **南向/北向启动同步等待**：
  - `general.southward.start_timeout_ms`
  - `general.northward.start_timeout_ms`
- **北向队列容量**：`queue_capacity`
- **南向点位基线 TTL（变更检测快照 GC）**：`general.southward.snapshot.device_change_cache_ttl_ms`（以及 `gc_interval_ms/gc_workers/max_devices_per_tick`）
- **日志输出**：
  - 输出到文件开关
  - 文件滚动策略（按时间/大小/两者）
  - 保留策略（保留天数、最大占用）
  - 日志格式（json/text）
  - 是否包含 span fields
- **日志清理功能**：清理策略 + 一键清理
- **Observability：Metrics / Prometheus**：
  - 是否启用 metrics 导出（Prometheus）
  - 指标粒度开关（越细越好）
  - Prometheus 与“供 UI 使用的指标”统一启用开关语义

### 0.2 明确不做（按你的要求）

- 日志采样/降噪
- Tracing
- per-channel / per-device QPS 限流
- Storage / DB / Cache
- Runtime / Resource（进程资源治理）
- Web / API（服务面配置项，如 rate limit/body limit 等）

---

## 1. 现状剖析（对齐你要改的点）

### 1.1 当前 Settings 模型：启动期快照

`ng-gateway-models/src/settings.rs` 的 `Settings(Arc<Inner>)` 目前是**启动时一次性加载**（`gateway.toml` + env），之后不可变。

你现在要的是：**Settings 成为全局唯一权威且运行时可变**，因此需要破坏式重构。

### 1.2 collector retry 需要统一语义与执行点

`retry_policy` 虽然是一个明确的结构化配置，但如果不统一到 SDK 的 `RetryController + RetryPolicy`，依然会出现：

- 明确语义（哪些失败算一次 attempt？哪些错误可重试？与 timeout 的关系？）
- 真实执行点（采集流程里没有按此策略做 retry）

因此必须规划并落地“最佳实践 retry 语义 + 代码实现”。

### 1.3 `collector.metrics_interval_ms` 的问题与重构方向

当前 `collector.metrics_interval_ms`：

- 从你提供的信息看“没任何地方用”，属于**配置漂浮字段**
- 从职责上也不应放在 collector：Prometheus 是 pull 模式，通常不需要 interval；而 UI 指标如果需要快照刷新，应属于 metrics 子系统

因此建议：

- 在 `gateway.toml`/`settings.rs` 新增 **`general.metrics`**
- **删除或废弃** `collector.metrics_interval_ms`（并迁移语义到 `general.metrics`）

---

## 2. 总体设计：Settings 唯一权威 + 运行时可变 + 持久化闭环

### 2.1 核心原则

- **唯一权威**：进程内所有组件读配置只能读 `Settings`
- **统一写入口**：只有一个地方可以写 `Settings`（避免写散导致一致性问题）
- **按子域（Sub-domain）单独更新**：运行时配置 API 必须“按子域隔离”，每个 API 只允许更新该子域字段，禁止一次性全量更新 Settings
  - 子域的目标不是“更多 API”，而是让 UI/后端的 apply 语义更可控：**小范围变更、可解释 impact、更少误伤**
- **强语义 impact**：每次变更必须返回“如何生效”
  - `hot_apply`
  - `restart_component`（例如 collector/northward/logging pipeline）
  - `restart_channel`（如果需要，精确到 channel_id）
  - `restart_process`（本次范围里尽量避免，但保留语义）
- **持久化闭环**：变更必须落盘；落盘失败必须明确告诉用户“重启会丢”

### 2.2 Settings 内部可变实现（性能优先）

你允许破坏式重构，推荐直接把“你要热调的字段”做成 lock-free 或低开销读：

- 数值/布尔：`AtomicU64/AtomicU32/AtomicUsize/AtomicBool`
- 枚举：`AtomicU8`（映射为 enum）
- 结构型（日志输出策略、metrics 粒度开关）：
  - **整体替换**用 `ArcSwap<T>`（读几乎无锁，写为 swap）

这样采集热路径不会被 RwLock 卡住。

### 2.3 Settings 统一控制入口（放在 `ng-gateway-common`，避免“过度 controller”）

你说得对：`SystemSettingsController` 作为独立“控制器对象”在文档层面会显得过度。

更符合你们当前工程形态（`NGAppContext` 已在 `ng-gateway-common/src/lib.rs` 作为全局控制中心）的最佳实践是：

- 在 `ng-gateway-common/src/` 新增一个 **settings 控制入口模块**（例如 `settings/control.rs`）
- 由 `NGAppContext` 暴露最小能力：**apply + persist + restart hooks**，同时保证“只有一个写入口”

建议的最小 API（示意，强调语义而非具体签名）：

- ✅ **最佳实践（强约束）**：对 web 层只暴露一个“不会被误用”的入口：**一次调用完成 apply + persist +（按 impact）触发生效动作**。

- `NGAppContext::apply_runtime_settings(domain, patch) -> ApplyResult`
  - `domain` 为强类型枚举（见第 7 节），保证“本次 patch 只能修改该子域字段”
  - 内部固定顺序（强约束）：
    - `apply_to_runtime(Settings)`：在持有 `NGAppContext` 写锁（`instance_mut()`）的情况下更新内存 Settings
    - `persist_to_gateway_toml(domain)`：**只回写该子域字段**（避免误解/审计噪声），采用原子写策略（第 3 节）
    - `apply_impact(impact)`：按结果触发 `collector.restart()` / `northward_events_pipeline.restart()` / `logging_pipeline.reload()` 等
  - 返回 `ApplyResult`：包含 `applied/persisted/impact/restart_targets/changed_keys/blocked_by_env` 等（第 7.2 节）

> 说明：把 apply 与 persist 拆成两个 public 方法容易被误用（忘记 persist、或 persist 的 domain 搞错、或 apply/persist 顺序不一致）。因此文档层面明确要求：web handler 只调用 `apply_runtime_settings` 这一种“不可出错”的入口。

> 这仍然不是审计/版本系统，只是把“写入/落盘/生效动作”收口到 `ng-gateway-common`，并让 web 层只做薄薄的 API 适配。

---

## 3. 持久化闭环：改完参数，重启后仍生效

### 3.1 持久化方式：**直接原子回写 `gateway.toml`**

按你的要求，本设计不引入 `gateway.runtime.toml`。运行时配置持久化统一采用：

- **直接回写 `gateway.toml`**

启动加载顺序（强约束）：

1) 读取 `gateway.toml`
2) （如果仍支持 env）env 覆盖一切（但这会导致“写回 gateway.toml 的值 != 实际生效值”）
   - 不强制用户避免 env 覆盖，但 **UI 必须提示**：“该字段被 env 覆盖，回写不会改变 effective”

### 3.2 我们是否有能力知道“哪些字段被 env 覆盖”？

有能力，但必须用**可推导的规则**实现“source 标注”，避免任何时候出现“手写字符串常量”的魔法值（尤其是 env 变量名、toml key path）。

本设计给出一个**高质量且可落地**的方法：用强类型 key + 统一推导规则，把“字段路径/Env Key/TOML Key 查询”全部收口。

#### 3.2.1 子域字段的强类型 Key（禁止散落字符串）

- 定义一个强类型枚举 `RuntimeSettingKey`（或按子域拆成多个 enum），每个枚举项代表一个**可运行时编辑字段**。
- 每个枚举项携带结构化路径：`&'static [&'static str]`（分段路径），例如：
  - `["general","collector","collection_timeout_ms"]`
  - `["general","southward","snapshot","gc_workers"]`
- 这些分段路径来自代码（enum/macro 生成），**不允许**在业务逻辑里出现 `"general.collector.collection_timeout_ms"` 这类字符串拼写。

> 备注：这不是“字符串常量散落”，而是“强类型 key 的唯一真源”。业务逻辑只和 enum 交互，字符串只在 enum 内部集中管理或由规则生成。

#### 3.2.2 Env Key 推导（从路径规则生成，避免手工维护映射表）

你们当前 `config::Environment::with_prefix("NG").separator("__")` 已经固定了规则，因此 env key 可从分段路径**规则推导**：

- `prefix = ENV_PREFIX`（复用 settings 加载处的同一常量，禁止散落 `"NG"`）
- `separator = ENV_SEPARATOR`（同上，禁止散落 `"__"`）
- `segments = path_segments.map(|s| s.to_ascii_uppercase())`
- `env_key = prefix + separator + segments.join(separator)`

这样：

- 不需要为每个字段手写 `NG__GENERAL__...` 的字符串常量
- 新增字段时只需要新增 enum 项（或 macro 生成），不会漏更新映射表

**检测方式**（控制权判断）保持严格语义：

- 只要 `std::env::var_os(derived_env_key).is_some()` 就认为该字段 `env_overridden=true`
- 即使 env 值与文件值相同也仍标记 overridden（控制权在 env）

#### 3.2.3 File / Default 判断（用结构化 TOML 查询推导，避免“判断某个 key 字符串存在”）

`config` crate 不会给出每个字段来自 file/default 的精确信息，因此我们采用“读原始 `gateway.toml` + 结构化查询”的可解释规则：

- 读取 `gateway.toml` 为 `toml::Value`（或 serde 的 map 结构）
- 使用同一份 `RuntimeSettingKey.path_segments()` 逐段向下查询：
  - 若 toml 中存在该 key path：认为 `source=file`
  - 否则：认为 `source=default`

这个判断同样没有“魔法字符串 key”，因为查询路径来自强类型 key 的分段数组。

#### 3.2.4 对外输出（按子域）

按子域输出视图（见第 7 节 API）：

- 每个字段返回：
  - `value`
  - `source: default | file | env`
  - `env_overridden: bool`
  - `effective_writable: bool`（例如 env_overridden=true 则 false）
- PATCH 返回：
  - `blocked_by_env: [RuntimeSettingKey...]`（用于 UI 提示，不是审计/版本）
  - `persisted=false` 时必须显式返回 `persistence_warning`

运行时写入顺序（强约束）：

1) 先 apply 到内存 `Settings`
2) 再原子回写 `gateway.toml`
3) 返回 `persisted=true/false`

### 3.3 回写 `gateway.toml` 的原子性与失败语义（必须固定）

- `applied=true, persisted=true`：运行中生效 + 重启仍生效
- `applied=true, persisted=false`：运行中生效，但 UI 必须显式提示“重启会丢失”

**原子写最佳实践（必须做）**：

- 写入 `gateway.toml.tmp`（同目录，保证 rename 原子）
- `fsync(tmp)`（可选但建议）
- `rename(tmp -> gateway.toml)`（原子替换）
- （可选）保留 `gateway.toml.bak` 作为运维兜底（这不是版本管理，只是灾难恢复）

---

## 4. 配置结构变更（你要求的：gateway.toml + settings.rs）

### 4.1 新增 `general.metrics`（并迁移/替代 collector.metrics_interval_ms）

建议在 `gateway.toml` 新增：

```toml
[general.metrics]
enabled = true

[general.metrics.prometheus]
enabled = true
path = "/metrics"

# UI 指标：建议做成“快照”接口（避免每次 UI 刷新都全量 gather）
[general.metrics.ui]
enabled = true

# 指标粒度开关（越细越好，建议按模块拆）
[general.metrics.granularity]
queues = true
collector = true
southward_io = true
northward = true
drivers = true
runtime_topology = true
```

对应 `settings.rs`：

- 在 `General` 下新增 `metrics: MetricsConfig`
- 删除 `CollectorConfig.metrics_interval_ms`

### 4.2 新增日志输出配置（logging output）

建议在 `gateway.toml`：

```toml
[logging.output]
format = "text"         # text | json
include_span_fields = true

[logging.output.file]
enabled = true
dir = "./logs"

[logging.output.file.rotation]
mode = "both"           # time | size | both
time = "daily"          # hourly | daily
size_mb = 100
max_files = 200         # 额外护栏：最多保留多少个滚动文件

[logging.output.file.retention]
max_days = 7
max_total_size_mb = 2048
```

并规划“日志清理策略”可复用 retention（第 6 节会给出闭环执行）。

---

## 5. 运行时生效语义（必须完整）

### 5.1 采集引擎（Collector）运行时调参语义

#### 5.1.1 `collection_timeout_ms`（Hot Apply）

- **语义**：单次采集（每设备/每轮）允许的最大 wall-clock 时间
- **运行时生效**：
  - 对后续采集立刻生效
  - 对正在进行的采集：不强制中断（除非你们实现 cooperative cancel），但下一轮必生效

#### 5.1.2 `retry_policy`（统一走 `RetryController + RetryPolicy`，最佳实践语义）

为了避免“每个模块手写一套 retry 逻辑导致语义漂移”，采集引擎与南北向插件/驱动统一使用 SDK 的：
`RetryPolicy`（配置） + `RetryController`（预算控制器）。

- **attempt 定义**：一次 attempt = 对同一采集请求（同一 device 的同一轮采集）的“一次驱动读取调用序列”
- **重试触发条件（建议）**：
  - I/O 超时（driver call timeout 或 `collection_timeout_ms` 内的子超时）
  - 可恢复的网络/临时错误（连接瞬断、临时拒绝）
  - 不重试：参数错误/协议错误/数据格式错误/权限错误（不可恢复）
- **策略字段语义（与 SDK 一致）**：
  - `max_attempts`：最多额外重试次数（不含首次 attempt）
    - `max_attempts = 0`：不重试
    - `max_attempts` 省略：无限重试（但仍受预算限制，见下）
  - `initial_interval_ms` / `max_interval_ms` / `multiplier` / `randomization_factor`：指数退避 + jitter（由 SDK 实现）
  - `max_elapsed_time_ms`：策略侧的“最大总耗时预算”（可选）
- **预算闭环（非常关键）**：
  - 单轮采集总预算受 `collection_timeout_ms` 限制
  - 实现上需要把 `collection_timeout_ms` 作为 **hard budget**，对 `RetryPolicy.max_elapsed_time_ms` 取 `min`，确保 retry sleep + 再次调用永远不会超过采集总预算
- **并发语义**：
  - 重试不会突破 `max_concurrent_collections`；同一 group call 的重试应在同一 permit 内完成（避免重试导致并发膨胀）

> 这套语义的关键价值：**全局一致**（所有模块同一 RetryController）、**可解释**（UI/文档可直接复用 SDK 语义）、且不会引入并发雪崩。

#### 5.1.3 `max_concurrent_collections`

支持动态调整 semaphore permits（hot apply）

#### 5.1.4 `outbound_queue_capacity`

这是典型 bounded channel 容量，通常必须重建：

- **impact**：`restart_component`（collector pipeline 重建）
- **闭环要求**：
  - restart 之前必须落盘成功（避免重启后又回到旧容量）
  - restart 后重新创建 sender/receiver，并重新挂接 metrics（队列指标会重置或迁移）

> 你没有要求“队列满策略”，但为了闭环可预期，建议固定策略：**背压优先**（send await），并设置一个合理 send 超时（例如 1s~5s），超时则丢弃并计数。

### 5.2 南向/北向启动同步等待（Hot Apply，对未来操作生效）

#### 5.2.1 `start_timeout_ms`

- **语义**：创建/重启 channel 时，同步等待“驱动连接就绪”的最长时间
- **运行时生效**：对后续创建/重启 channel 立即生效；不强制对现存连接做重连

#### 5.2.2 `start_timeout_ms`

- **语义**：创建/重启 northward app 时同步等待“app 连接就绪”的最长时间
- **运行时生效**：对后续 app restart/enable 立即生效；不强制断开现存 app

### 5.3 北向队列容量

#### 5.3.1 `queue_capacity`（restart_component）

- **语义**：northward events bounded channel 容量
- **运行时生效**：需要重建 channel（restart northward event processor / manager pipeline）

### 5.4 Southward：点位基线 TTL（`device_snapshots`）与异步 GC（大点位场景）

> 背景：`ReportType::Change` 需要维护“点位基线”用于变更检测。大点位设备（例如 Modbus 10 万点）会导致基线长期占用内存。
> 本方案通过 “(u64(ms tick), NGValue) + 异步 GC” 让点位基线 **最终会被剔除**（不保证立刻剔除），以便在可接受的误报成本下控制内存。

#### 5.4.1 数据结构（与代码一致）

在 `ng-gateway-core/src/southward/manager.rs` 的 `DeviceDataSnapshot` 中，将点位存储改为元组：

- `telemetry: HashMap<i32, (u64, NGValue)>`
- `client/shared/server_attributes: HashMap<i32, (u64, NGValue)>`

其中：

- `u64(ms tick)`：该点位基线“最后一次被刷新”的单调毫秒 tick（用于 TTL 判断）
- `NGValue`：点位的基线值（用于变更检测）

#### 5.4.2 TTL 刷新语义（与变更检测对齐）

在 `ReportType::Change` 的过滤路径中：

- **只有当点位值发生变化**（或该点位首次出现）时，才会写入/覆盖 `(u64, NGValue)`，从而刷新 TTL
- 点位值相同不会刷新 TTL（保证“长期不变的点位最终会过期并被 GC 清掉”）

#### 5.4.3 异步 GC：保证“最终会被剔除”

Southward 管理器启动时会启动 GC（见 `NGGateway::init`）：

- 每 `gc_interval_ms` 扫描最多 `max_devices_per_tick` 个设备快照
- 每个设备快照里删除满足 `now - last_touch > device_change_cache_ttl_ms` 的点位
- `gc_workers` 个异步 worker 并发处理不同设备，避免单线程卡住

注意：

- 这是 best-effort，不保证“点位过期立刻被删”
- 但它保证：只要点位不再发生变化（TTL 不刷新），最终会被扫描到并从内存剔除

#### 5.4.4 配置项（`gateway.toml`）

```toml
[general.southward.snapshot]
device_change_cache_ttl_ms = 600000  # 10min, 0 disables
gc_interval_ms = 60000               # 1min
gc_workers = 2
max_devices_per_tick = 256
```

建议范围（产品护栏，供 UI 校验）：

- `device_change_cache_ttl_ms`: \([10_000, 86_400_000]\)，0 表示关闭
- `gc_interval_ms`: \([200, 300_000]\)
- `gc_workers`: \([1, 16]\)
- `max_devices_per_tick`: \([1, 10_000]\)

### 5.4 日志输出（logging output）运行时变更

你要的日志输出项涉及 tracing subscriber 的重构（但完全可做成运行时生效）。

#### 5.4.1 目标：日志输出策略可热替换

要支持运行时切换：

- file on/off
- rotation/retention 参数
- format text/json
- include_span_fields on/off

推荐最佳实践实现方式：

- 使用 `tracing_subscriber::reload`（reloadable layer）
- 将“输出层（fmt + file appender）”包裹成可 reload 的 Layer
- 运行时变更时：构建新的 Layer 并原子切换

impact 语义建议：

- **format/include_span_fields**：`restart_component(logging_pipeline)`（本质是 reload subscriber layer，用户感知为立刻生效）
- **file enable/rotation/retention**：同上（reload）

### 5.5 日志清理（策略 + 一键清理）

#### 5.5.1 清理策略（自动）

- 由 `logging.output.file.retention` 定义：
  - `max_days`
  - `max_total_size_mb`
  - （可选）`max_files`
- 实现一个后台 task：
  - 固定周期（例如每 10 分钟）扫描日志目录
  - 按规则删除最旧文件
  - **强约束**：不得删除当前正在写入的 active 文件（通过文件名约定或文件锁/handles 规避）

#### 5.5.2 一键清理（手动）

- 提供 API：按策略立即执行一次清理
- 支持 dryRun（只返回将删除哪些文件与释放空间；不算“审计/版本”，只是预览）

---

## 6. Metrics / Prometheus：统一启用语义 + 极细粒度开关

### 6.1 统一启用语义（你问的“UI 指标是否也要统一开关？”）

建议统一为一个总开关：

- `general.metrics.enabled`
  - `false`：Prometheus endpoint 与 UI metrics（快照接口）全部禁用；同时停止后台快照任务
  - `true`：允许按子开关启用 prometheus 与 ui

子开关：

- `general.metrics.prometheus.enabled`
- `general.metrics.ui.enabled`

### 6.2 指标粒度开关（越细越好，且可运行时变更）

建议按模块拆成 bool：

- `queues`：所有 bounded queues 的长度/丢弃/等待等
- `collector`：采集成功率、耗时分布、重试次数分布
- `southward_io`：驱动读写耗时、错误率
- `northward`：上行 publish 耗时、失败率、事件处理耗时
- `drivers`：驱动调用计数、错误分类
- `runtime_topology`：channel/device/app 数量与状态分布

运行时生效策略：

- 对“是否导出”类（endpoint enable）：hot apply（立刻返回 404 或禁用 handler）
- 对“是否采集/记录该指标”类：hot apply（内部写入点前做 if-check；关闭则不更新，降低开销）

### 6.3 UI metrics 的最佳实践形态（避免 UI 刷新造成高开销）
- UI层的快照应受 `general.metrics.ui.enabled` 控制

---

## 7. 统一 API 入口：`system.rs`（整合 `logging.rs`，保持运行时配置操作一致）

你提出要统一入口，这是正确方向：运行时调参属于同一 control plane。

### 7.1 建议路由（v1）

- **总原则**：运行时配置 API **按子域单独 apply+persist**，禁止 `PATCH /system/settings` 这种“一次性更新所有”入口。

建议路由（全部归口在 `system.rs` 注册，但按子域拆分 handler）：

- `GET /system/settings/collector`
- `PATCH /system/settings/collector`
- `GET /system/settings/northward`
- `PATCH /system/settings/northward`
- `GET /system/settings/southward_snapshot`
- `PATCH /system/settings/southward_snapshot`
- `GET /system/settings/logging_output`
- `PATCH /system/settings/logging_output`
- `GET /system/settings/logging_cleanup`
- `PATCH /system/settings/logging_cleanup`
- `GET /system/settings/metrics`
- `PATCH /system/settings/metrics`

日志能力（整合并收敛 `v1/logging.rs` 到 `/system/settings`）：

- `GET /system/settings/logging_runtime`
- `PATCH /system/settings/logging_runtime`
- `GET /system/settings/logging_files`
- `POST /system/settings/logging_files/download`
- `POST /system/settings/logging_files/cleanup` - “一键清理/按策略清理”

说明：

- `PATCH` 必须在一个 API 内完成：**apply 到运行时 + persist 到 `gateway.toml` +（按 impact）触发生效动作**，并返回结果（第 7.2 节）
- 对 UI 来说：每个模块只关心一个子域，避免“点保存日志却把 metrics 也写回”的不可控副作用
- **硬切换（你的要求）**：删除现有 `v1/logging.rs` 的 `/logging/*` 路由与实现；日志相关能力统一以 `/system/settings/logging_*` 为唯一入口（并纳入 settings 的 apply+persist 规则与返回模型）

### 7.2 ApplyResult（闭环所需的最小返回模型）

每次子域 `PATCH /system/settings/{domain}` 返回：

- `applied: bool`（内存已应用）
- `persisted: bool`（落盘成功与否）
- `domain`：子域枚举（强类型，不用字符串）
- `changed_keys: []`：本次实际变更的字段 key（用于 UI 高亮与审计展示）
- `blocked_by_env: []`：被 env 覆盖导致拒绝写入的字段 key
- `persistence_warning?: string`：当 `persisted=false` 时必须返回（UI 直接展示）
- `impact`：
  - `hot_apply`
  - `restart_component`（列出组件名：collector/northward/logging_pipeline/metrics_snapshot）
  - `restart_process`（尽量不出现，但语义保留）
- `restart_targets?: []`

> 关键：`domain/changed_keys/blocked_by_env` 必须是强类型枚举序列（或可解析的结构化 key），避免 UI/后端互相用字符串对齐。

### 7.3 UI 详细设计：Preferences / System（按子域单独应用/保存）

对应代码位置：`ng-gateway-ui/packages/effects/layouts/src/widgets/preferences/blocks/system/`

#### 7.3.1 设计目标（你要求的闭环 + 高质量工程）

- **按子域拆块**：每个 block 只负责一个子域的展示/编辑/应用/保存
- **Apply = 生效 + 持久化**：用户点击“应用/保存”一次请求完成，避免“先 apply 再 save”的双按钮分裂语义
- **source 可解释**：字段旁显示 `default/file/env`，当 `env_overridden=true` 时禁用输入并提示原因
- **无魔法值**：
  - 前端不手写 domain 字符串 / key path 字符串去拼请求
  - 统一从“子域枚举 + key 枚举（或后端返回的 key）”生成请求与展示

#### 7.3.2 UI Block 划分与 API 映射

- **建议页面信息架构（IA）**：Preferences -> System（系统）页签下，按“分组 + 卡片/折叠块”呈现。

**A. Runtime Tuning（运行时调参）分组**

- **Collector**（采集引擎）：`GET/PATCH /system/settings/collector`
- **Northward**（北向队列 + start timeout）：`GET/PATCH /system/settings/northward`
- **Southward Snapshot**（TTL + GC）：`GET/PATCH /system/settings/southward_snapshot`
- **Metrics**（总开关/Prometheus/UI/粒度开关）：`GET/PATCH /system/settings/metrics`

**B. Logging（日志）分组（最佳实践拆分：至少 2 个子域 + 1 个运维块）**

- **Logging Runtime**（日志级别/override/TTL 等运行时控制）：`GET/PATCH /system/settings/logging_runtime`
  - 直接替代并删除现有“日志级别旧接口与旧路由”
- **Logging Output**（输出到文件/滚动/保留/格式/span fields）：`GET/PATCH /system/settings/logging_output`
- **Logging Files（运维）**：`GET /system/settings/logging_files` + `POST /system/settings/logging_files/download`
  - 若做清理：`POST /system/settings/logging_files/cleanup`

> 为什么日志要拆多个子域？  
> - **Runtime vs Output**：日志级别是“极高频且强运维属性”的运行时控制；输出策略（file/rotation/retention/format）是“更重、可能触发 reload”的管道配置。拆开能让 impact 更可控、权限更清晰、UI 更不误操作。  
> - **Settings vs Ops**：文件列表/下载/清理是运维操作，和设置 patch 的“配置闭环”不同。即使路径在 `/system/settings/...` 下，也应在模型层明确区分为 `ops`（不会写配置，不会触发 persist）。

**C. 前端文件结构规划（和现状对齐，可直接开工）**

当前 `blocks/system` 只有两个文件：

- `logging.vue`：日志级别设置 + 打开下载弹窗
- `log-download-modal.vue`：实现日志文件列表/下载
  - 本设计要求硬切换：前端必须改用 `/system/settings/logging_files` + `/system/settings/logging_files/download` + `/system/settings/logging_files/cleanup`，同时删除旧接口与旧路由

按本设计落地后，建议拆成更清晰的 3 个 block（文件名只是建议，重点是职责边界）：

- `logging-runtime.vue`
  - 替代/迁移自现有 `logging.vue` 的“日志级别/TTL 等运行时控制”
  - API：`/system/settings/logging_runtime`
- `logging-files.vue` + `log-download-modal.vue`
  - 把“文件列表/下载”从 runtime block 拆出来，避免混在同一个卡片里
  - API：`/system/settings/logging_files` + `/system/settings/logging_files/download` + `/system/settings/logging_files/cleanup`
- `logging-output.vue`
  - 新增：文件输出/滚动/保留/格式/span fields（这块当前 UI 还没有，需要新增）
  - API：`/system/settings/logging_output`

> 现状对齐：当前 `logging.vue` 通过旧的日志级别接口管理 baseline level。本设计要求硬切换：实现 `/system/settings/logging_runtime` 后，前端与后端路由统一迁移到新接口，并删除旧接口与旧路由。

#### 7.3.3 交互流程（每个子域一致）

- 页面进入：`GET /system/settings/{domain}` 拉取 `view`（含每字段 value + source + env_overridden）
- 用户编辑：仅修改本地 `draft`
- 点击“应用/保存”：
  - `PATCH /system/settings/{domain}` 提交 `patch`
  - UI 根据返回的 `ApplyResult`：
    - 若 `blocked_by_env` 非空：提示“这些字段被 env 覆盖，未写入”
    - 若 `persisted=false`：展示 `persistence_warning`（强调“重启会丢”）
    - 根据 `impact` 展示“已热生效 / 已重启某组件”结果
  - 之后再次 `GET` 刷新（避免 UI 与实际状态漂移）

#### 7.3.4 前端请求抽象建议（避免字符串拼接）

- 用一个统一的 composable（例如 `useSystemRuntimeSettings(domain)`）：
  - `domain` 是 TS enum/union（不是字符串任意值）
  - `get()` / `patch()` 方法只接受结构化 key（由后端返回或共享模型定义）
- API path 由 `domain -> path` 的**集中映射**生成（集中处可以使用 `as const` + TS 类型推导，避免散落拼接）

补充（针对你强调的“不要魔法值”）：

- 日志级别这类枚举值也不建议在组件里散落硬编码列表（例如 `'INFO'|'ERROR'|...` + `items=[...]`）
  - 更高质量的做法：后端在 view 中返回 `allowed_values`（或由共享模型生成 TS enum），前端直接渲染该列表

#### 7.3.5 UI 交互细节（页面/分组/状态机，保证“按子域单独应用/保存”体验一致）

- **分组展示**
  - 每个分组（Runtime Tuning / Logging）使用折叠面板或卡片网格
  - 每个子域是一个独立 Card/Block，内部包含：
    - 表单字段（带 `source` badge：default/file/env）
    - `Reset`（回到最近一次 GET 的值，仅本地）
    - `Apply/Save`（唯一主按钮；触发 PATCH，一次完成 apply+persist）

- **脏状态（dirty）**
  - Block 内任一字段与 last_loaded 不一致时显示 `Unsaved changes`
  - 只允许对“脏的 block”点击 Apply；避免用户误以为全局保存

- **env 覆盖（不可写）**
  - `env_overridden=true`：字段输入框 disabled，并显示提示（“由环境变量接管，需修改环境变量才能改变 effective”）
  - PATCH 若仍提交该字段，后端返回 `blocked_by_env`，UI 显示“已忽略/未写入”

- **持久化失败（必须显式提醒）**
  - `persisted=false`：以红色 Callout 展示 `persistence_warning`（例如“本次仅运行时生效，重启会丢失”）
  - 同时把 block 标记为“需要运维介入”（便于用户截图/报障）

- **impact 展示**
  - `hot_apply`：toast + “已立即生效”
  - `restart_component`：toast + 显示重启的组件列表（来自 `restart_targets`），并提示短暂抖动风险

> 可选增强：提供“Apply All Changed Blocks（逐块应用）”按钮，但实现上必须是**按子域顺序逐个 PATCH**，并汇总每个子域的结果；不要做“一个请求提交所有子域”的全量 PATCH（违背本设计核心约束）。
---

## 8. 生效动作：需要重启哪些“组件/通道”？（闭环矩阵）

> 下面矩阵只保留你本次范围内的项。

| 配置项 | impact（默认） | 运行时怎么生效 | 需要重启谁 |
|---|---|---|---|
| `collection_timeout_ms` | hot_apply | 影响后续采集 timeout | 无 |
| `retry_policy` | hot_apply | 影响后续采集重试（统一 `RetryController`） | 无 |
| `max_concurrent_collections` | hot_apply | 支持动态调整 semaphore permits | `collector` |
| `outbound_queue_capacity` | restart_component | 重建 bounded channel/pipeline | `collector`（或 collector->gateway pipeline） |
| `general.southward.start_timeout_ms` | hot_apply | 影响后续 channel create/restart | 无 |
| `general.northward.start_timeout_ms` | hot_apply | 影响后续 app restart/enable | 无 |
| `queue_capacity` | restart_component | 重建 northward events channel | `northward_events_pipeline` |
| `general.southward.snapshot.*` | hot_apply | 影响点位基线 TTL/GC 频率与并发度（内存回收速度与 CPU 开销） | 无 |
| logging 输出策略（file/rotation/retention/format/span fields） | restart_component | reload tracing output layer | `logging_pipeline` |
| metrics enabled / prometheus enabled / ui enabled | hot_apply | enable/disable handlers + snapshot task | `metrics_snapshot`（按需） |
| metrics granularity toggles | hot_apply | 在记录点做开关判断 | 无 |
| log cleanup policy | hot_apply | 后台任务读新策略 | 无 |

---

## 9. 端到端闭环示例（你最关心的：改参数后重启仍生效）

以 `collection_timeout_ms` 为例：

1) UI（Collector block）修改为 10000，调用 `PATCH /system/settings/collector`
2) 后端：
   - apply：更新 `Settings.general.collector.collection_timeout_ms`（atomic store）
   - persist：原子回写 `gateway.toml` 成功
   - impact：`hot_apply`
3) 重启后：
   - 启动加载：读取 `gateway.toml`（若无 env 覆盖，则 effective 即为 10000）
   - `Settings` 初始化时读到 10000
   - collector 运行中读取到 10000

闭环完成。

---

## 10. 分阶段实施计划（只围绕本次范围，且保证每阶段可验收）

### Phase 1：基础设施（Settings 可变 + 回写 `gateway.toml` + system.rs 框架）

- **Settings 破坏式重构**：
  - 将本次范围内字段改造成可热读/热写（Atomic/ArcSwap）
  - 给每个字段提供 `get/set` API（禁止散落写入）
- **持久化闭环**：
  - 原子回写 `gateway.toml`（同目录 tmp+rename，可选 bak）
  - persist 失败返回 `persisted=false`
- **新增 `system.rs` 并完成“按子域”路由注册**（先空实现也行，但框架先立住）
  - `GET/PATCH /system/settings/{domain}`（collector/northward/southward_snapshot/logging_output/logging_cleanup/metrics）
  - 禁止 `PATCH /system/settings` 这种全量入口

验收：

- 修改 `collection_timeout_ms` 运行时立刻生效
- 重启后仍生效（`gateway.toml` 已回写）

### Phase 2：Collector 完整语义落地（含 retry）

- 在采集流程中实现第 5.1 节的 retry 语义（attempt/可重试错误/退避/jitter/超时预算/并发不膨胀）
- 实现 `max_concurrent_collections` 的 支持动态调整 semaphore permits
- 实现 `outbound_queue_capacity` 的 pipeline 重建

验收：

- 人为制造 transient error 能看到 retry 生效（次数与退避符合配置）
- 修改并发/队列容量后按 impact 重启组件且继续工作
- 重启后配置仍生效

### Phase 3：Northward 队列容量闭环

- `queue_capacity`：重建 northward events channel（restart_component）

验收：

- 修改 queue_capacity 后能自动重启 pipeline 且服务不中断（或短暂抖动可接受，但要明确）

### Phase 4：Logging 输出策略 + 清理闭环

- 引入 reloadable logging pipeline
- 实现 logging.output 全量配置 + runtime apply
- 实现：
  - 自动清理任务（按 retention）
  - 一键清理 API（可 dryRun 预览）

验收：

- 运行中切换 json/text、打开/关闭文件输出、调整滚动/保留策略可立即生效
- 一键清理按策略删除且不会误删 active 文件
- 重启后仍生效

### Phase 5：Metrics（Prometheus + UI 快照）统一开关与粒度开关

- 新增 `general.metrics` 并迁移/删除 `collector.metrics_interval_ms`
- 实现总开关 + 子开关 + 粒度开关
- 实现 UI metrics 快照后台任务（受开关与 interval 控制）

验收：

- metrics.enabled=false：Prometheus 与 UI metrics 都关闭
- 粒度开关关闭后对应指标不再更新（性能可控）
- 重启后仍生效

