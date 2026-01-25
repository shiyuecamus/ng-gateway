# NG Gateway 实时日志（全局 + Channel 级）与动态调级方案（产品级设计）

> 目标：在 Web UI 中**实时查看**网关运行时日志与 **Channel 级日志**，支持**动态调整日志级别**，并在**离开页面/退出组件**后自动恢复，或用 **TTL 超时**自动回滚。
>
> 本文刻意**不包含权限/安全**设计（按你的要求）。

---

## 1. 背景与现状（基于当前仓库代码）

### 1.1 网关侧（host）日志

当前网关侧已存在全局 logger：

- `ng-gateway-common/src/logger.rs`：基于 `tracing_subscriber` 的 console + rolling file（`logs/ng.log`）输出，并支持运行时 `Logger::set_level(Level)`。
- `ng-gateway-common/src/lib.rs`：启动时初始化 `Logger` 并 `set_global_default(subscriber)`。

特点：
- **全局单一 level**（用 `DynFilterFn` 按 `Level` 筛）。
- 目前并无“日志事件实时分发给 UI”的基础设施（仅 stdout + 文件）。

### 1.2 Web 侧实时通道能力

Web 服务使用 Actix，并已有成熟 WebSocket 基建：

- `ng-gateway-web/src/api/v1/ws/*`：例如 `/api/ws/metrics`、`/api/ws/monitor`
- 具备：升级封装、订阅/退订消息模型、ticker 合并发送等“产品级”实时推送写法

这意味着新增 `/api/ws/logs` 可以复用同样结构（连接管理、批量推送、心跳、tail+follow 等）。

### 1.3 驱动侧（cdylib）为什么“用 host logger 打不出来”

你现在在驱动 SDK 中有导出符号：

- `ng-gateway-sdk/src/southward/mod.rs`：`#[no_mangle] extern "C" fn ng_driver_init_tracing(debug: bool)`
- `ng-gateway-sdk/src/southward/loader.rs`：动态加载 driver 后，会 `library.get(b"ng_driver_init_tracing")` 并调用它

这件事不是“偶然写法”，而是被运行时强依赖：**如果不在 driver 里 init tracing，driver 的 tracing 日志在 host 里通常不可见**。

#### 根因（强结论）

你们的驱动是 `cdylib` 形式动态加载（`libloading`）。在 Rust 生态中，`cdylib` 往往是“自包含”世界：

- **每个 `cdylib` 会包含它自己的依赖代码与静态全局状态**（除非刻意把依赖做成共享动态库并在进程级共享符号）。
- `tracing` 的默认 dispatch/subscriber 依赖静态全局（`tracing-core` 的全局 dispatch）。当 host 与 driver 各自携带一份 `tracing-core` 静态时：
  - host `set_global_default()` 只影响 host 那份全局 dispatch
  - driver 里 `tracing::info!/debug!` 发到 driver 那份全局 dispatch
  - 于是你会观察到：“不在 driver 初始化 subscriber，driver 日志就没输出”

同类证据在你们代码里已经出现（Tokio runtime 也有 dylib 隔离问题）：

- `ng-gateway-sdk/src/southward/transport/mod.rs` 注释明确说明：跨 `cdylib` 的 Tokio runtime 上下文隔离会导致 “there is no reactor running”
- 这与 tracing 的“全局静态不共享”是同一类问题：**进程内的多个 dylib 世界不是天然共享 Rust 运行时的静态全局**。

#### 结论

`cdylib` 驱动场景下，**不能指望 driver 自动使用 host 的 `ng-gateway-common::Logger`**。

要做到“产品级统一日志体验（UI 实时查看、Channel 级过滤、动态调级、回滚）”，需要明确设计：**driver 日志如何进入网关的统一日志管道**。

---

## 2. 目标与非目标

### 2.1 目标

- **实时查看**
  - 网关全局日志实时查看
  - Channel 级日志实时查看（按 channel_id 聚合/过滤）
- **动态调级**
  - 全局调级（例如 INFO -> DEBUG）
  - Channel 调级（对某个 channel 临时提升到 DEBUG/TRACE）
- **恢复策略**
  - 离开页面/退出组件自动恢复
  - TTL 超时自动恢复（兜底）
- **可用性体验**
  - tail + follow
  - 批量推送 + 丢包计数（避免 WS 被打爆）
  - UI 虚拟列表，不卡顿

### 2.2 非目标（本方案不做）

- 权限/安全/RBAC/审计/脱敏（按要求全部不展开）
- 历史检索（ELK/Loki）与跨实例聚合（可留作后续）

---

## 3. 总体架构（推荐）

核心思想：把“实时日志查看”做成一个**独立的数据产品链路**，不依赖读文件，不依赖解析 stdout。

### 3.1 组件划分

1) **Gateway LogHub（内存态）**
- 全局 ring buffer（用于 tail）
- per-channel ring buffer（用于 Channel tail）
- 一个 broadcast / 多路复用 fan-out，用于实时推送

2) **Gateway Log Layer（tracing subscriber Layer）**
- 作为 `tracing_subscriber` 的一个 layer，拦截 host 侧 `tracing` events，写入 LogHub

3) **Driver Log Bridge（关键）**
driver 是 `cdylib`：它有自己的 tracing 世界。

推荐做法：在 driver SDK 内把 tracing events **桥接回 host**（通过稳定 ABI），让 host 统一进入 LogHub。

4) **WebSocket: `/api/ws/logs`**
- 支持订阅 `global` / `channel(channel_id)`
- 支持 `tail`（先发最近 N 条）+ `follow`
- 支持 client-side level 过滤（显示过滤）与 server-side 临时调级（生成更多 debug）

5) **Log Override Lease（TTL/离开恢复统一机制）**
- UI “调级”动作不会永久改变配置，而是创建一个 lease
- lease 到期或 WS 断开自动回滚

### 3.2 数据流

- host 自身日志：
  - `tracing` -> GatewayLogLayer -> LogHub -> `/api/ws/logs` -> UI

- driver 日志（cdylib）：
  - driver 内 `tracing` -> DriverBridgeLayer（或 subscriber）-> **FFI callback** -> host -> LogHub -> `/api/ws/logs` -> UI

这样最终 UI 看到的是**统一格式**、统一的 channel 维度日志。

---

## 4. Channel 级日志的“正确归因”：Span 绑定（强烈推荐）

如果想实现“Channel 级日志实时查看”，最佳实践不是要求每个 `debug!` 都手写 `channel_id`，而是：

### 4.1 在每个 Channel 运行入口建立 Span

在 southward channel 实例的核心 task/actor 入口建立 span，fields 包含：
- `channel_id`
- `channel_name`
- `driver_id` / `driver_type`

并让 Channel 内部所有执行都在该 span 下运行（`instrument` / `span.enter()`）。

### 4.2 LogHub 侧取 `channel_id`

LogHub 写入时，优先从当前 span（或 span extensions）读取 `channel_id`，作为事件的归因字段。

收益：
- 少量改动即可覆盖大量日志
- driver/host 都可用同一机制归因

---

## 5. 动态调级与恢复：Lease + TTL（产品化关键）

将“离开页面恢复 / 退出组件恢复 / TTL 超时”统一成一个概念：**Log Override Lease**。

### 5.1 Lease 模型（建议）

- `override_id`: UUID
- `scope`: `global | channel(channel_id) | driver(driver_id) | target(prefix)`
- `level`: INFO/DEBUG/TRACE…
- `ttl_ms`: 默认 5min（可配置上限）
- `created_at` / `expires_at`
- `keepalive`: 可选（客户端定期 renew）

### 5.2 回收规则

- **主动释放**：UI 组件卸载时发送 release
- **被动回收（兜底）**：
  - WS 断开时自动 release（如果 lease 与 WS session 绑定）
  - TTL 到期自动回滚（无论 WS 如何）

### 5.3 两个层次的“调级”

现实约束：`tracing` 的事件是否会被构造/执行，与 subscriber 的启用级别相关。想做到“只对某个 Channel 完全零开销地打开 DEBUG/TRACE”，在通用 tracing 体系里通常做不到绝对零成本。

因此推荐分两层：

1) **显示过滤（UI 侧）**
- UI 自己按 level/text filter 过滤显示
- 不改变 server 生成日志的量

2) **生成过滤（server 侧）**
- 通过 lease 临时提高 server（host 或某个 driver）的过滤级别，以便真正产生更多 debug/trace 信息
- TTL 到期自动回滚

---

## 6. Driver 日志桥接（解决 cdylib “看不到 host logger”的关键）

### 6.1 方案对比（结论：推荐 FFI callback 桥接）

#### 方案 A：保持现状（driver 内 try_init + stdout/file）
- 优点：实现最简单
- 缺点：
  - host/driver 日志体系割裂，UI 侧很难做统一的实时展示与 channel 归因
  - driver 的格式、level、输出位置很难统一控制

#### 方案 B：解析 stdout（host 读 stdout 再推 UI）
- 优点：不改 driver
- 缺点：不可控、难结构化、难按 channel 归因、容易被不同格式打崩；不推荐产品化

#### 方案 C（推荐）：driver tracing -> FFI callback -> host LogHub
- 核心：host 向 driver 注册一个**稳定 ABI**的日志回调；driver 把 tracing event 序列化成结构化记录，回调给 host。
- 优点：
  - 统一进入 LogHub，UI 实时与 channel 级过滤自然成立
  - 可以在 host 侧统一 batching/限流/丢包统计
  - 可以在 host 侧实现 lease + TTL 控制 driver 的日志级别（见 6.4）
- 缺点：需要改 SDK + loader + host（一次性工程量，但收益巨大）

### 6.2 推荐的 ABI 形态（稳定、可扩展）

在 driver SDK 定义一组 C ABI，供 host 调用注册：

- `ng_driver_set_log_sink(sink: LogSinkV1) -> u32`
  - 返回码：0=ok，其他=不支持/版本不匹配

其中 `LogSinkV1` 建议包含：
- `abi_version: u32`
- `emit_json: extern "C" fn(ptr: *const u8, len: usize)`  
  - payload 是 UTF-8 JSON，结构见 6.3
- `flush: Option<extern "C" fn()>`（可选）
- `user_data: *mut c_void`（可选，若你们喜欢带上下文）

> 说明：用 JSON 作为跨边界载体，是为了最大程度保持 ABI 稳定与可演进；后续可改为 msgpack/flatbuffers，但先用 JSON 最省心。

### 6.3 Driver -> Host 的日志记录格式（JSON）

建议 schema（字段越少越稳）：

- `ts`: RFC3339 或 epoch millis
- `level`: `"TRACE"|"DEBUG"|"INFO"|"WARN"|"ERROR"`
- `target`: Rust tracing target（crate/module）
- `message`: 格式化后的 message
- `fields`: object（可选，包含 event 的结构化字段）
- `span`:
  - `name`
  - `fields`（可选：span 的 fields，例如 `channel_id`）

host 收到后将其转换成内部统一 `LogEvent`：
- 补齐 `source = "driver"|"host"`
- 从 `span.fields.channel_id` 提取 `channel_id`（实现 Channel 归因）

### 6.4 Driver 动态调级（配合 lease）

要让 UI “临时调级”真的让 driver 产出更多日志，需要 driver 侧可运行时调级。

推荐在 driver SDK 内提供导出：

- `ng_driver_set_max_level(level: u8) -> u32`
- `ng_driver_get_max_level() -> u8`

driver SDK 内实现方式：
- 用 `AtomicU8` 存储当前最大 level
- subscriber/filter 在每个 event 判断一次（尽量低开销）
- host 的 lease manager 到期后调用 `set_max_level` 回滚

> 注意：这依然是 driver 自己的 tracing 世界，所以必须由 host 通过 FFI 明确驱动它。

---

## 7. LogHub（内存态）设计：tail + follow + 丢包统计

### 7.1 数据结构建议

- 全局 ring buffer：`VecDeque<LogEvent>`（固定容量，例如 10k）
- per-channel ring buffer：`HashMap<i32, VecDeque<LogEvent>>`（每个 channel 固定容量，例如 2k）
- 实时分发：`tokio::sync::broadcast::Sender<LogEvent>` 或自研 fanout

### 7.2 推送策略（避免 WS 被打爆）

推荐复用你们 `/api/ws/metrics` 的“ticker 合并发送”思想：

- server 端按连接维护一个待发送队列
- 每 50~200ms tick：
  - 批量发送 `logBatch`（最多 N 条）
  - 如果积压太多则丢弃旧的并累计 `dropped`

UI 展示“Dropped X”即可（体验上非常关键）。

---

## 8. WebSocket API：`GET /api/ws/logs`

### 8.1 订阅模型

一个 WS 连接允许多订阅（参考 `/api/ws/metrics` 的 multi-subscription）。

#### Client -> Server

- `subscribe`
  - `requestId`: string（用于 ack）
  - `scope`: `"global"|"channel"`
  - `channelId?`: number（scope=channel 必填）
  - `tail`: number（默认 200）
  - `minLevel`: `"TRACE"|"DEBUG"|"INFO"|"WARN"|"ERROR"`（默认 INFO）
  - `text?`: string（可选：简单包含过滤）
  - `override?`（可选：订阅时顺便创建 lease）
    - `level`
    - `ttlMs`
    - `targets?`（可选：仅提升某些 target 前缀）

- `unsubscribe`
- `ping`
- `renewOverride`（可选：续租）
- `releaseOverride`（可选：释放）

#### Server -> Client

- `subscribed`
  - `requestId`
  - `subscriptionId`
  - `overrideId?`
  - `effectiveLevel`（server 当前实际生成级别）

- `logBatch`
  - `subscriptionId`
  - `items`: `LogEvent[]`
  - `dropped`: number（本批次前累计丢弃数）

- `error`
  - `requestId?`
  - `message`

### 8.2 tail + follow

订阅成功后：
- 先发 `tail` 条（来自 ring buffer）
- 然后进入实时 follow（来自 broadcast）

---

## 9. UI 设计（不含权限/安全）

### 9.1 全局实时日志页

功能建议：
- 顶部工具条
  - 级别筛选（显示过滤）
  - “临时提升 server 日志级别”面板（level + TTL + 倒计时 + 一键恢复）
  - 搜索（文本包含）
  - 暂停/继续（暂停时不自动滚动，但仍可缓存）
  - 清屏、导出最近 N 条
- 主体列表
  - 虚拟滚动（必须）
  - 高亮 ERROR/WARN
  - 显示 dropped 提示

### 9.2 Channel 详情页：实时日志 Tab

默认行为：
- 自动订阅 `scope=channel, channelId=当前channel`
- 默认 tail=200

调试面板：
- “临时调级”：DEBUG/TRACE + TTL
- 离开页面或组件卸载：
  - 主动 release override（如果存在）
  - 断开 WS（或取消订阅）
  - 即使没主动 release，TTL 也会回滚（兜底）

---

## 10. 推荐落地计划（按迭代拆解）

### Iteration 1：先把 host 日志实时化（不碰 driver）

目标：UI 能看到网关运行时日志实时流（host 部分），并支持全局调级（lease+TTL）。

- 新增 LogHub（host）
- 新增 host tracing layer -> LogHub
- 新增 `/api/ws/logs`（仅 global scope）
- UI 新增“全局实时日志页”

验收：
- tail + follow
- 批量推送 + dropped 提示
- 全局 lease 调级 + TTL 回滚

### Iteration 2：Channel 级日志归因（host 侧）

目标：host 产生的 channel 相关日志可按 channel_id 过滤查看。

- 在 southward channel task 入口建立 span（含 channel_id）
- LogHub 从 span fields 提取 channel_id
- `/api/ws/logs` 增加 channel scope
- UI 在 channel 详情页加“实时日志”Tab

验收：
- Channel 页只看到该 channel 的日志（至少 host 侧）

### Iteration 3：Driver 日志桥接（把 cdylib 世界并入统一日志）

目标：driver（cdylib）日志也进入 LogHub，实现真正的 Channel 级实时日志。

SDK（ng-gateway-sdk）：
- 增加 `ng_driver_set_log_sink(LogSinkV1)` 导出
- 增加 `ng_driver_set_max_level/get_max_level` 导出
- 修改 `ng_driver_init_tracing`：
  - 默认安装一个 subscriber/layer
  - 若已注册 log sink，则将 events 序列化为 JSON 并回调给 host
  - 否则 fallback 到 stdout（兼容）

Host（gateway side）：
- driver loader 加一步：加载成功后尝试调用 `ng_driver_set_log_sink` 注册回调
- lease manager 在调级时对相关 driver 调用 `ng_driver_set_max_level`

验收：
- driver 内 `tracing::debug!` 能在 UI 看到
- 对某个 channel 临时调到 DEBUG/TRACE 后，UI 能实时看到更详细 driver 日志
- TTL/离开页面后自动回滚

---

## 11. 测试与验证建议（工程落地必备）

### 11.1 单元/集成测试（建议方向）
- LogHub ring buffer：容量、覆盖策略、per-channel 分片正确性
- WS logs：订阅/退订、tail+follow、batch、dropped 统计
- Lease：TTL 到期回滚、断开回滚（如果绑定 WS session）

### 11.2 手工验收脚本（建议）
- 打开全局日志页：观察实时滚动
- 开启全局 DEBUG（TTL=1min）：观察更多日志，TTL 到期恢复
- 打开 channel 日志页：只看到 channel 相关日志
- 开启 channel DEBUG（TTL=1min）：观察更多 driver 细节日志；离开页面立即恢复（或 TTL 恢复）

---

## 12. 附：为什么要做“桥接”而不是继续 try_init

一句话：`cdylib` 驱动是一个“自包含世界”，host 的 tracing subscriber 不会天然作用于 driver；继续在 driver 里 try_init 只能把日志打印到 driver 自己的 stdout/file，无法自然进入网关的“统一实时日志产品链路”。

要做“Channel 级实时查看 + 动态调级 + TTL/离开恢复”，最稳健的路径是：

- **host 统一 LogHub + WS**
- **driver 通过稳定 ABI 把日志事件回传到 host**

