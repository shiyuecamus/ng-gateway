## NG Gateway：统一日志子系统（Host + Southward Driver + Northward Plugin）最佳实践设计与 Phase 推进计划

> 本文目标：把 `ng-gateway` 的日志从“能用”提升到**产品级、可治理、可热调、可审计、成本可控**的统一日志系统。
>
> 核心交付：
> - **统一落盘路由**（Single Source of Truth）：`host.log` / `driver_<driver_type>.log` / `plugin_<plugin_type>.log`
> - **统一运行时日志级别治理**：global + channel + app（TTL lease，自动回滚）
> - **统一 `cdylib` 桥接**：Southward driver 与 Northward plugin 都通过 sink 把日志“进 host 管道”，不允许各自私装 subscriber
>
> 约束：你明确表示可以接受破坏式变更，因此本文按“最佳实践/高质量最终形态”设计，不追求兼容旧行为。

---

## 0. 背景与现状审计（以代码为事实基础）

### 0.1 你们已经具备的关键底座（很强）

当前 host 侧日志系统已具备产品级雏形：

- **Host 统一 subscriber**：`ng-gateway-common/src/log/host.rs`
  - console layer（stdout）+ split rolling file layer
  - 动态 filter（非 `EnvFilter`，而是自定义 Filter，可读 span context）
- **运行时日志级别控制（global + channel，TTL）**：`ng-gateway-common/src/log/control.rs`
  - `LogOverrideManager`：lease + cleanup loop
  - per-channel 覆盖通过 span 中的 `channel_id` 生效
- **Driver→Host 日志桥接**（完整闭环）：
  - driver 侧：`ng-gateway-sdk/src/southward/log.rs`（Layer 捕获 + 批量 JSONL flush + set_max_level）
  - host 侧：`ng-gateway-common/src/log/driver.rs`（FFI callback copy+enqueue + ingest loop re-emit）
  - host 落盘：`ng-gateway-common/src/log/split_file.rs`（目前按 `driver_type` 拆到 `{driver_type}.log`）
- **控制面 API（已落地）**：
  - global log level：`ng-gateway-web/src/api/v1/system.rs`
  - channel log level（TTL）：`ng-gateway-web/src/api/v1/channel.rs`
  - 日志文件 list/download/cleanup：`ng-gateway-web/src/api/v1/system.rs` + `ng-gateway-common/src/log/cleanup.rs`

### 0.2 现状的“决定性问题”（北向必须改）

北向插件是 `cdylib` 动态加载：`ng-gateway-core/src/northward/loader.rs`

- loader 会调用插件导出的 `ng_plugin_init_tracing(debug)`（SDK 宏生成）
- SDK 当前实现的 `ng_plugin_init_tracing` 会在插件里 `tracing_subscriber::fmt().try_init()`

这带来两个产品级问题：

1) **插件日志不受 host 控制面治理**  
   - host 的 global/channel override、落盘策略、下载清理 API，都无法自然作用于插件“内部 subscriber”输出的日志

2) **`cdylib` 全局 subscriber 的“隔离”是常态而非例外**  
   - 即便你希望插件日志共享 host subscriber，也很难可靠做到（Rust 动态库依赖版本/全局状态隔离等）
   - 最佳实践是：**插件只做 capture+bridge，host 是唯一权威 logger**

结论：北向必须像南向一样桥接进 host，且需要 app 维度治理（这是北向天然粒度）。

---

## 1. 设计目标与非目标

### 1.1 设计目标（必须同时满足）

- **Single Source of Truth（单一权威管道）**
  - 所有日志最终都进入 host 的统一 subscriber
  - host 负责：格式、过滤、落盘、清理、下载、（未来）OTel
- **可治理（运行时可控）**
  - global level（长期）
  - channel level（TTL override，自动回滚）
  - app level（TTL override，自动回滚）
  - 变化需要 best-effort 下发到 `cdylib` 侧（driver/plugin）
- **低开销（Hot path bounded）**
  - host filter/route：无锁或极低锁竞争（Atomic + span extensions + non-blocking writer）
  - `cdylib` bridge：队列有界（drop-old-keep-new），flush 批量化
- **安全（外部输入不可信）**
  - `driver_type`/`plugin_type` 可能来自第三方库：必须 sanitize 成安全 file stem
  - 日志文件清理/打包必须防 ZipSlip/路径穿越
- **语义明确（避免误导）**
  - 任何“按 app/channel 生效”的能力，必须建立在明确的字段/span 契约上
  - 不允许“看起来能调，但实际上调不到（因为缺字段/缺桥接）”

### 1.2 非目标（本设计暂不覆盖）

- 分布式 tracing（跨进程 trace id 传播）
- OpenTelemetry traces/logs export（可以预留，但不强制纳入本期）
- 基于用户/租户的日志隔离（可在未来以 `tenant_id` 扩展）

---

## 2. 统一日志域模型（字段契约与语义，必须写死）

> 关键原则：**过滤与落盘路由只依赖低基数字段**（cardinality 可证明上界）。

### 2.1 Source 与身份字段（必备）

统一字段（所有日志“最终形态”都应能表达）：

- `source`: `"host" | "driver" | "plugin"`
- `channel_id`: `i32`（可选，存在时表示可按通道覆盖）
- `app_id`: `i32`（可选，存在时表示可按 app 覆盖）
- `driver_type`: `str`（仅 `source=driver` 时允许）
- `plugin_type`: `str`（仅 `source=plugin` 时允许）

建议（可选但强烈建议）：

- `driver_id`: `i32`（可选，便于定位动态库实例）
- `plugin_id`: `i32`（可选，便于定位动态库实例）

### 2.2 低基数约束（必须禁止的字段进入路由/过滤）

禁止用于落盘路由（因为基数可能爆炸）：

- `device_id`、`point_id`、`point_key`、topic、任意自由字符串

允许用于事件字段（日志内容）但不得参与路由/过滤：

- `device_id`/`point_key` 可以作为 event fields 出现（用于排障），但不作为 file stem

### 2.3 Span 字段契约（决定 per-channel/per-app 是否可生效）

Host 侧动态过滤目前依赖 span extensions（`ChannelIdLayer` 把 `channel_id` 写入 extensions）。

本设计要求新增：

- `AppIdLayer`：把 `app_id` 写入 extensions
- 语义：如果 event 处于带 `app_id` 的 span 下，则它可按 app override 生效

硬规则：

- northward plugin 的主 span 必须包含 `app_id`（SDK 宏已创建 `"northward-plugin" app_id=...`，这是正确方向）
- southward driver 的主 span 必须包含 `channel_id`（你们已有规范文档：`document/southward-driver-logging-guidelines.md`）

---

## 3. 统一落盘路由（你指定的文件命名规范）

### 3.1 文件命名（固定）

统一落盘文件：

- `host.log`
- `driver_<driver_type>.log`
- `plugin_<plugin_type>.log`

> 注意：你们现有实现是 `{driver_type}.log`；本设计按你要求改为带前缀（破坏式变更可接受）。

### 3.2 路由优先级与决策树（必须明确）

落盘路由只基于 `source + driver_type/plugin_type`：

1) `source=driver` 且 `driver_type` 存在  
   → `driver_<driver_type>.log`
2) `source=plugin` 且 `plugin_type` 存在  
   → `plugin_<plugin_type>.log`
3) 其他情况（含 host 自身日志、或桥接缺字段的异常情况）  
   → `host.log`

### 3.3 sanitize 规则（必须写死，防路径穿越/不可控文件名）

`driver_type` / `plugin_type` 的 file stem 规则：

- lowercased
- 仅允许 ASCII `[a-z0-9_-]`
- 其他字符替换为 `_`
- trim `_`，空则为 `unknown`
- 最大长度：64（避免极长文件名/路径）

### 3.4 rotation/cleanup/list/download 的兼容性要求

你们当前日志文件扫描逻辑是基于 `.log`（`ng-gateway-utils/src/log_files.rs`），本命名仍满足：

- active：`host.log` / `driver_x.log` / `plugin_y.log`
- time rotation：`host.log.<date>` / `driver_x.log.<date>` / `plugin_y.log.<date>`
- size rotation（附加 `.1/.2/...`）保持一致

清理策略必须遵循：

- 永不删除“正在写入”的 active files（你们已做 protected 逻辑）
- 仅删除符合命名约定的文件（你们已做 `validate_safe_file_name`）

---

## 4. 运行时日志级别治理（global + channel + app）

### 4.1 Scope 与优先级（必须写死）

定义 3 个 override scope：

- Global
- Channel(channel_id)
- App(app_id)

优先级建议（从强到弱）：

1) Channel override（最强）
2) App override
3) Global effective level

理由：

- Channel 是“现场协议链路”粒度，通常比 App 更贴近问题源头
- App 的 debug 往往会牵涉大量 payload encode/queue/router，影响比 channel 更广

### 4.2 TTL lease 语义（与现有 channel TTL 对齐）

要求：

- set temporary override 会替换同 scope 旧 lease（行为确定性）
- cleanup loop 负责自动回滚（expiry 触发 effective level recompute）
- TTL 必须有 min/max 边界（防止“永久 debug”）

配置建议（新增到 `Settings.logging.control`）。
 
> 说明：TTL 与护栏属于 **override 机制的通用治理策略**，不应按 channel/app 重复配置。
> 同一套 `override_*` 约束同时作用于：
> - `Channel(channel_id)` 覆盖
> - `App(app_id)` 覆盖
> 
> 未来如果新增其他 scope（例如 `Tenant(tenant_id)`），也直接复用这套护栏，避免配置膨胀。

```toml
[general.logging.control]
# TTL defaults/guardrails for ALL override scopes (channel/app/...)
override_default_ttl_ms = 300000
override_min_ttl_ms = 10000
override_max_ttl_ms = 1800000

# cleanup tick (existing)
override_cleanup_interval_ms = 5000
```

### 4.3 动态 filter（host 侧）最终语义

host filter 的 enabled 判定伪代码（决定是否放行 event）：

```text
effective = overrides.effective_global_level()

if current_span has channel_id:
  effective = overrides.effective_channel_level(channel_id)
else if current_span has app_id:
  effective = overrides.effective_app_level(app_id)

allow if event.level <= effective
```

说明：

- channel/app 两者同时存在时：按优先级选择 channel
- 若 runtime override manager 不可用：退化使用 baseline（AtomicU8）保证系统仍能跑

### 4.4 下发到 `cdylib`（driver/plugin）的策略

目标：让 `cdylib` 侧 capture layer 在源头就过滤，减少编码/队列/拷贝成本。

现实约束：

- `cdylib` 侧无法知道 host 当前 span 的 channel/app 上下文（它只能看到自己记录事件时的 metadata/span）
- 所以 host 下发只能是“粗粒度上限”，不能是精确 per-scope filter

最佳实践策略：

- host 维护一个 **desired_max_level_for_cdylib**：
  - 取 `max(effective_global, max(all_active_channel_overrides), max(all_active_app_overrides))`
- 将该 max level best-effort 下发到：
  - 所有 southward drivers：`ng_driver_set_max_level(u8)`
  - 所有 northward plugins：`ng_plugin_set_max_level(u8)`（新增）

意义：

- 若任何 channel/app 被临时调到 DEBUG，则 `cdylib` 侧至少会开始 capture DEBUG 事件（否则 host 想看也看不到）
- 精确是否输出仍由 host filter 决定（根据 span 上下文）

---

## 5. `cdylib` 统一桥接设计（Driver + Plugin 共用一套思想）

### 5.1 总体原则（最重要）

- `cdylib` 内 **不得** 安装最终输出 subscriber（fmt/json/rolling file）
- `cdylib` 内只安装 “bridge layer subscriber”：捕获事件→编码→写入 sink（非阻塞）
- host 是唯一权威：filter、format、route、write、cleanup

### 5.2 统一 sink ABI（建议：从“driver 专用”升级为“cdylib 通用”）

你们现有 `LogSinkV1` 已足够通用（emit JSON/JSONL），建议破坏式重命名以消除“仅 driver”语义：

- `LogSinkV1`（保留结构，但语义改为通用 sink）
- `SOURCE_DRIVER` 扩展为：`SOURCE_DRIVER | SOURCE_PLUGIN`
- 新增字段：`plugin_type` 与 `app_id` 的桥接字段约定

> 如果你希望更彻底：可以把 wire schema versioned，明确 `WireEventV2`，允许未来扩展（trace_id/span stack 等）。

### 5.3 `cdylib` 侧：PluginBridgeLayer（对齐 DriverBridgeLayer）

新增插件侧 bridge layer（类似 `ng-gateway-sdk/src/southward/log.rs` 的 driver layer）：

- 捕获 event
- 从当前 span 抽取 `app_id`（并从 parent 继承，避免第三方库子 span 丢字段）
- 编码为 JSONL，字段包含：
  - `level_u8` / `target` / `message` / `fields`
  - `span`: `{ name, app_id }`
  - `source="plugin"`
  - `plugin_type="kafka"|"pulsar"|...`（由 SDK macro 固定常量提供）

插件导出符号（新增，供 host loader 调用）：

- `ng_plugin_set_log_sink(LogSinkV1) -> u32`
- `ng_plugin_set_max_level(u8) -> u32`
- `ng_plugin_init_tracing(debug: bool)`：语义改为 “install bridge layer”，不再安装 fmt subscriber

### 5.4 host 侧：PluginIngestBridge（对齐 DriverIngestBridge）

新增 `ng-gateway-common/src/log/plugin.rs`（或合并到一个通用 `cdylib.rs`）：

- FFI callback：copy + enqueue（永不阻塞）
- ingest loop：parse JSONL → re-emit as host `tracing` event
- re-emit 时创建/复用 span：
  - span 名字建议稳定：`"plugin"`（或 `"cdylib"`），并带字段：
    - `source="plugin"`
    - `plugin_id`（如果有）
    - `plugin_type`
    - `app_id`（若有）
- 这保证：
  - host 的 `AppIdLayer` 能在 span extensions 缓存 `app_id`
  - host filter 能按 app override 生效
  - file layer 能按 source/plugin_type 路由到 `plugin_<plugin_type>.log`

### 5.5 队列与背压护栏（ingest capacity 统一治理）

driver/plugin 本质都是 **cdylib → host 的 ingest**，建议把容量配置合并为一个统一的护栏（已落地）：

```toml
[logging.control]
ingest_queue_capacity = 10000
```

原因：

- plugin 日志往往更“业务化”且可能更高频（payload encode / route / retries）
- 现场调试时可能同时打开某个 app DEBUG，会产生突发洪峰

策略：

- driver 与 plugin 的 ingest 队列都采用 drop-old-keep-new（保障“看到最新问题”）
- 两者都使用同一 `ingest_queue_capacity` 上限（避免配置膨胀，且便于运维统一调参）
- 并在 host 侧提供自监控日志/指标（见 9.4）提示发生 drop

---

## 6. Host 落盘实现要点（split file layer 升级）

### 6.1 现状与需要改的点

现状 split file layer 仅识别 `driver_type` 并路由到：

- host → `host.log`
- driver → `{driver_type}.log`

需要升级为：

- host → `host.log`
- driver → `driver_<driver_type>.log`
- plugin → `plugin_<plugin_type>.log`

### 6.2 路由字段抽取策略（最佳实践）

路由信息优先从 span extensions 读取（避免每条 event 解析 fields）：

- `source`（可从 span fields 或 event fields）
- `driver_type` / `plugin_type`

建议新增 `SourceExtractorLayer`（类似现有 `DriverTypeExtractorLayer`）：

- 将 `source` 与 `plugin_type` 缓存到 span extensions
- 保留 fallback：若 extensions 不存在，则从 event fields 解析（兼容少数未在 span 内的事件）

### 6.3 include_span_fields 的语义扩展

你们已有 `include_span_fields`，当前只输出 `channel_id` 和 span stack names。

建议扩展输出字段（用于落盘排障，不影响过滤）：

- `app_id`（若有）
- `source`（若有）

这样在 `plugin_<plugin_type>.log` 中能直接看到 `app_id`，无需再 grep message。

---

## 7. 控制面 API/UI 设计（新增 App 级别日志级别）

### 7.1 API（建议）

新增 app log level endpoints（与 channel 对齐）：

- `GET /api/v1/apps/{id}/log-level`
- `PUT /api/v1/apps/{id}/log-level`（body: `{ level, ttlMs }`）
- `DELETE /api/v1/apps/{id}/log-level`

返回 view 结构建议对齐 `ChannelLogLevelView`：

- `baseline`（global effective/baseline）
- `effective`
- `override`: `{ level, ttlMs, expiresAtMs } | null`
- `ttlRange`: `{ minMs, maxMs, defaultMs }`

### 7.2 UI（建议）

与南向 channel log level modal 复用交互模型：

- 入口：Northward App 列表或详情页
- Modal：
  - 选择 level
  - TTL（带 min/max 提示与 countdown progress）
  - “恢复跟随系统级别”

### 7.3 权限/RBAC

若已有 RBAC（repo 有 casbin），建议新增权限点：

- `apps:log_level:read`
- `apps:log_level:write`

---

## 8. 工程化规范（防止回归与误用）

### 8.1 北向插件开发规范（新增）

新增文档（未来可拆成 `document/northward-plugin-logging-guidelines.md`）核心规则：

- 插件不得 `tracing_subscriber::fmt().init()` / `try_init()`（违反统一日志）
- 插件任何 `tokio::spawn` 必须继承 span（`.instrument(Span::current())`）
- 插件必须在核心 span 下运行（SDK 宏生成 `"northward-plugin"` span 已满足，但 spawn 不能断）

### 8.2 CI 规则（强烈建议）

- 对 `ng-gateway-northward/**` 与外部插件示例：
  - 检测 `tracing_subscriber::fmt()` 的出现（禁止）
  - 检测裸 `tokio::spawn(` 且无 `.instrument(`（报警/失败）

> 这类规则可以极大降低“靠 code review 人肉记忆”的风险。

---

## 9. 自监控与运维视角（必须能解释系统行为）

### 9.1 必须能回答的运维问题

- “为什么我把 app 调到 DEBUG 了还是看不到日志？”
  - 可能原因：插件未桥接/缺 app_id span/`cdylib` max level 未下发
- “为什么日志文件爆增？”
  - 可能原因：TTL 太长/误开 TRACE/某插件在 tight loop 打日志
- “是否丢日志？”
  - driver/plugin ingest 队列 drop-old：必须能观测 drop 次数

### 9.2 建议新增的内部指标（可选但强烈建议）

若你们已有 metrics hub，可加低基数 counters：

- `ng_logging_driver_ingest_dropped_total`
- `ng_logging_plugin_ingest_dropped_total`
- `ng_logging_driver_ingest_lines_total`
- `ng_logging_plugin_ingest_lines_total`

> labels 不要带 driver_id/plugin_id；最多带 `driver_type/plugin_type`（也可不带）。

### 9.3 日志文件治理（现有能力复用）

你们已有：

- list/download/cleanup API
- cleanup worker（retention + max_files）

本设计要求把 `driver_`/`plugin_` 前缀纳入“产品文档说明”，并在 UI 里展示更清晰的分组：

- host logs
- driver logs（按 driver_type）
- plugin logs（按 plugin_type）

### 9.4 关键告警建议（运维闭环）

建议在告警规则里加入（PrometheusRule）：

- drop_total 在 5m 内持续增长（说明日志洪峰或 host 无法处理）
- log dir 使用率过高（若有磁盘指标）

---

## 10. Phase 推进计划（按闭环交付拆解，越细越可执行）

> 目标：每个 Phase 都能独立验收，且不会出现“做到一半反而更难排障”的中间态。

### Phase 0：设计冻结与契约落地（1-2 天）

交付物：

- 本文档评审通过并冻结（字段、路由、优先级、API 形状）
- 明确破坏式变更列表（见 10.6）

验收标准：

- 团队对“host 为唯一权威 logger”达成一致
- 明确 `host.log / driver_* / plugin_*` 命名不可再变（否则影响运维/脚本）

### Phase 1：Host 落盘路由升级（driver_* 前缀 + plugin_* 预留）（2-4 天）

目标：先把 host file layer 从 `{driver_type}.log` 改为 `driver_<driver_type>.log`，并把 plugin 路由逻辑预埋（哪怕暂时没插件桥接）。

工作项：

- split file layer 增加 `source + plugin_type` 识别能力
- 路由规则按 3.2 实现
- sanitize 复用同一实现（driver/plugin）
- log files list/download/cleanup 逻辑验证对新文件名仍可用

验收标准：

- southward driver 日志文件名变为 `driver_<driver_type>.log*`
- host 日志仍是 `host.log*`
- 日志文件 UI list/download/cleanup 全流程可用

### Phase 2：统一 LogControl 扩展（新增 App override + TTL）（2-5 天）

目标：在 host 侧完成 app 维度动态级别治理，哪怕插件桥接尚未完全完成，也要能对 host 侧 northward 管理日志生效。

工作项：

- `LogOverrideScope` 增加 `App(i32)`
- `LogControlSettings` 增加 app TTL 配置（default/min/max）
- `LogFilter` 增加 app_id span 支持（新增 `AppIdLayer` 与 `AppIdExt`）
- effective level 决策树按 4.3 实现
- 增加 API + UI（Phase 2.5 可拆开）

验收标准：

- `global INFO + app DEBUG`：northward 管理相关日志在该 app span 下能输出 DEBUG
- TTL 到期自动回滚，UI 倒计时正确

### Phase 3：Northward Plugin 桥接（替换 ng_plugin_init_tracing）（3-8 天）

目标：让 `cdylib` 北向插件日志进入 host 管道，并受 app override 影响。

工作项（SDK）：

- 新增/实现 plugin bridge layer（捕获 event + app_id 继承 + JSONL flush）
- SDK 宏生成：
  - `ng_plugin_set_log_sink`
  - `ng_plugin_set_max_level`
  - `ng_plugin_init_tracing` 改为安装 bridge layer（禁止 fmt subscriber）
- 插件 span 必须带：
  - `source="plugin"`
  - `plugin_type=<const>`
  - `app_id`

工作项（core host loader）：

- `ng-gateway-core/src/northward/loader.rs`：
  - dlopen 后先注册 sink（类似 southward loader）
  - 再调用 `ng_plugin_init_tracing`
  - 并记录 `set_max_level` fn 指针，用于后续下发

工作项（common host log）：

- 新增 host 侧 plugin ingest bridge（copy+enqueue + parse + re-emit）
- re-emit 必须进入带 `app_id` 的 span（确保 filter 生效）

验收标准：

- 插件日志进入 `plugin_<plugin_type>.log`
- `global INFO + app DEBUG` 时：该 app 的插件 DEBUG 可见；其他 app 不受影响
- 调高任意 app 到 DEBUG 会触发 `ng_plugin_set_max_level` 下发（至少开始 capture）

### Phase 4：统一下发策略与护栏（driver+plugin）（2-4 天）

目标：把“desired_max_level_for_cdylib”与 ingest 队列护栏做成闭环，避免现场误用导致资源不可控。

工作项：

- 计算 desired max level（global + active channel/app overrides 的 max）
- 下发到：
  - drivers：已有
  - plugins：新增
- 将 ingest queue capacity 统一为一个护栏（`[logging.control].ingest_queue_capacity`）并同时作用于 driver+plugin ingest
- 新增 drop 计数与（可选）指标

验收标准：

- 任意 override 生效时，`cdylib` max level 能跟随变化
- 大量日志洪峰下不会 OOM（队列有界 + drop-old）
- drop 行为可观测（日志或指标）

### Phase 4.5：前端 UI 跟进（App Log Level）（1-2 天）

目标：在 UI 上提供 northward app 维度的临时日志级别调节入口（TTL + 倒计时），对齐 channel log level 的交互模型。

工作项：

- App 列表/详情页增加 “Log Level” 入口（Modal）
- 对接 API：
  - `GET /api/v1/northward-app/{id}/log-level`
  - `PUT /api/v1/northward-app/{id}/log-level`
  - `DELETE /api/v1/northward-app/{id}/log-level`

验收标准：

- `global INFO + app DEBUG` 时该 app 相关日志可见且 UI 显示 TTL 倒计时，到期后自动刷新

### Phase 5：工程化与文档闭环（持续）

工作项：

- 新增 `northward-plugin-logging-guidelines.md`
- CI 规则：禁止插件 fmt subscriber；spawn 必须 instrument
- 运维文档：解释三类日志文件、下载与清理建议、常见故障定位

验收标准：

- 新增插件作者“按规范写”即可获得正确的 per-app 日志治理与落盘
- 代码 review 不再依赖人肉记忆

### 10.6 破坏式变更清单（必须提前告知）

- 日志文件名变更：
  - `{driver_type}.log` → `driver_<driver_type>.log`
  - 新增 `plugin_<plugin_type>.log`
- northward `cdylib` 插件导出 ABI 变更：
  - 增加 `ng_plugin_set_log_sink` / `ng_plugin_set_max_level`
  - `ng_plugin_init_tracing` 语义变化（不再 fmt 输出）
- Settings 扩展：
  - logging.control 增加 app TTL 与 plugin ingest capacity
- API/UI 扩展：
  - 新增 app log level endpoints 与 UI 入口

---

## 11. 最终验收清单（上线前必须逐条验证）

### 11.1 功能闭环

- [ ] `host.log` / `driver_<driver_type>.log` / `plugin_<plugin_type>.log` 三类日志都能产生并按预期写入
- [ ] global log level 可热调
- [ ] channel log level（TTL）可热调且到期回滚
- [ ] app log level（TTL）可热调且到期回滚
- [ ] driver/plugin 的 `cdylib` max level 会随 override 变化 best-effort 下发

### 11.2 正确性（两组必测用例）

- [ ] **全局 DEBUG + app INFO**：该 app 的 DEBUG 不应输出（正确抑制）
- [ ] **全局 INFO + app DEBUG**：该 app 的 DEBUG 应输出（含插件内部第三方库日志）

### 11.3 成本与护栏

- [ ] 日志洪峰不会导致 OOM（队列有界）
- [ ] drop 行为可观测（至少能在日志/指标看到）
- [ ] sanitize 生效：恶意 plugin_type 不会造成路径穿越或生成奇怪文件名

