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

补充（本方案的收敛目标）：
- 当 driver 日志通过桥接进入 host 后，将统一交由 `ng-gateway-common/src/logger.rs` 输出：
  - **控制台输出**（便于本地开发/排障）
  - **滚动文件输出**（生产必备）
  - **同时写入 LogHub**（用于 Web UI 实时日志）
  - 从而避免 driver 自己各自 try_init 输出到 stdout/file 造成“割裂 + 重复 + 难统一治理”。

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

最佳实践（与本方案一致）：
- `ng_driver_init_tracing` 仍然需要保留，但它的职责将从“driver 自己 fmt 打印”升级为：
  - **在 driver 内安装桥接用的 subscriber/layer**（捕获 `tracing` 事件）
  - **自动桥接 `log` -> `tracing`**（捕获第三方库 `log`）
  - **仅桥接回传到 host**，不再直接 stdout/file 输出（避免双份日志与额外开销）

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

`cdylib` 驱动场景下，**不能指望 driver “天然/自动”使用 host 的 `ng-gateway-common::Logger`**（全局静态不共享）。
但可以通过本方案的 **`log_sink` 桥接** 把 driver 日志回传到 host，并最终统一走 host 的 `ng-gateway-common/src/logger.rs` 输出与治理。

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

#### 6.1.1 与现有 `ng_driver_init_tracing` 的关系（强制收敛）

你目前在 SDK 里的 `ng_driver_init_tracing(debug: bool)` 主要用于“让 driver 自己把日志打印到控制台”。在本方案里，最佳实践是：

- **生产环境默认只走 host logger**：driver 日志桥接回 host 后，统一由 `ng-gateway-common/src/logger.rs` 输出到控制台+滚动文件，并写入 LogHub（实时 UI）。
- **driver 侧的 `ng_driver_init_tracing` 负责“捕获与桥接”，而不是“输出”**：
  - 捕获 `tracing` 事件（安装 bridge layer）
  - 捕获 `log`（安装 `tracing_log::LogTracer`）
  - 将日志进入 driver 内部队列 -> 后台批量回传 host（不再 fmt 输出）

这样可以最大化复用 host 的日志治理能力（格式、文件、动态调级、采集、实时分发），并避免 driver 自己输出导致的重复与不一致。

### 6.2 推荐的 ABI 形态（稳定、可扩展、避免桥接成为瓶颈/死锁点）

在 driver SDK 定义一组 C ABI，供 host 调用注册：

- `ng_driver_set_log_sink(sink: LogSinkV1) -> u32`
  - 返回码：0=ok，其他=不支持/版本不匹配

其中 `LogSinkV1` 建议包含：
- `abi_version: u32`
- `emit_json: extern "C" fn(ptr: *const u8, len: usize)`
  - payload 是 UTF-8 JSON（单条），结构见 6.3
- `emit_batch_json: Option<extern "C" fn(ptr: *const u8, len: usize)>`
  - payload 是 UTF-8 JSON（多条批量，固定使用 **JSON Lines**），用于降低 FFI 调用开销
- `flush: Option<extern "C" fn()>`（可选）
- `user_data: *mut c_void`（可选，若你们喜欢带上下文）

> 说明：用 JSON 作为跨边界载体，是为了最大程度保持 ABI 稳定与可演进；后续可改为 msgpack/flatbuffers，但先用 JSON 最省心。

#### 6.2.1 关键约束：driver 侧“只投递不阻塞”，host 侧异步处理

你提的点非常关键：**driver 日志桥接绝不能成为性能瓶颈或死锁点**。在 IoT 网关里，日志往往会发生在：
- hot path（采集循环、协议解析、I/O 回调）
- 持锁路径（驱动内部 `Mutex/RwLock` 保护的状态机）
- 异常路径（重试/超时/断线恢复）

如果在这些路径里直接调用 host 的 FFI 回调（即便回调本身很轻），也会引入：
- **不可控阻塞**：host 侧处理/排队/序列化/WS 推送可能慢
- **潜在死锁**：driver 持锁 -> 回调 host -> host 又触发需要同一锁/资源的逻辑（哪怕很间接）
- **尾延迟放大**：高频日志会放大调度与锁竞争

因此推荐把“桥接”拆成两段：

- **driver 同步热路径（只投递）**
  - `tracing` layer / `log` logger 里只做一件事：把日志记录快速入队到 driver 内部的**有界队列**
  - 如果队列满：采用 **丢旧保新**（弹出最旧一条，再插入最新一条）
  - 这一段必须满足：**无 await、无阻塞、尽量少分配**（允许少量格式化，但不能阻塞）
  - 实现提示（设计层面）：
    - 需要“固定容量 + FIFO + 丢旧保新”的语义，推荐使用无锁/低锁的 bounded queue，并在 `push` 失败时先 `pop` 再 `push`
    - 不建议直接用 `tokio::mpsc` 的 `try_send` 默认语义（满时通常是丢新），除非额外包一层实现丢旧保新
  - 性能关键点（hot path 必读）：
    - **先过滤、后格式化**：必须先做 `enabled/level` 判断（例如 `AtomicU8` 最大 level + target 前缀匹配），通过后才允许做 message/fields 的格式化；否则在 INFO 场景也会为 DEBUG/TRACE 付出格式化成本
    - **限制单条日志大小**：对 `message`/序列化 payload 设置上限（例如 2~8KiB，超过截断并标记），避免异常情况下单条日志把队列与批量 buffer 撑爆
    - **避免锁竞争**：bridge queue 选择低锁/无锁实现，避免在高频日志下形成全局锁热点（特别是多线程 runtime）
    - **避免多次分配**：尽量一次性构造 payload（预分配 `String/Vec<u8>`，或复用 buffer）；不要在 hot path 里做 JSON 序列化（序列化放到后台 flush task）

- **driver 后台 flush task（异步/批量）**
  - 在 driver 自己的 Tokio runtime（你们已有 `NG_RUNTIME`）里启动一个后台任务，从队列批量 drain
  - 每批最多 N 条或最多 M 字节，优先走 `emit_batch_json`，否则退化为多次 `emit_json`
  - 这一段允许阻塞/慢一点，因为它不在采集/协议 hot path 上；但仍要做基本限流与丢弃统计

同时，host 侧也要遵循“只收不阻塞”的契约：
- `emit_json/emit_batch_json` **必须快速返回**（只做 copy + 快速入队到 host 内部有界队列；队列满同样按 **丢旧保新** 处理）
- host 自己再起异步 worker 做解析、归因、写 LogHub、WS batch 推送

最终效果：日志链路在两侧都有背压边界，且阻塞点被隔离到后台任务中，避免桥接成为系统瓶颈/死锁点。

#### 6.2.2 同时兼容 `log` 与 `tracing`（降低驱动开发心智负担）

驱动生态里两类日志库都很常见：
- 你们自研驱动与新代码更可能使用 `tracing`
- 第三方协议库/老代码大量使用 `log`

为了让“驱动开发者只要正常打日志就能进 UI”，推荐在 SDK 的 `ng_driver_init_tracing`（或桥接初始化）里自动做两件事：

- **`log` -> `tracing` 统一入口**
  - 在 driver 内安装 `tracing_log::LogTracer`（若已安装则忽略错误）
  - 这样 `log::info!/warn!` 会被转成 `tracing` event，最终进入同一条桥接管道

- **driver 内只需要一种 subscriber/layer**
  - 使用 `tracing_subscriber::registry()` + 自定义 layer（桥接到内部队列）
  - 不再叠加 `fmt` 输出层：避免双份日志与额外开销，且统一由 host 侧 logger 负责输出与落盘

> 结论：驱动开发者无需理解“桥接/回调/ABI”，只要使用 `tracing` 或 `log` 打日志即可被捕获并汇入 host 的 LogHub。

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

为防止内存失控，需要显式设计**容量上限 + LRU + TTL**（尤其是 per-channel）。

推荐结构：

- **全局 ring buffer**
  - `VecDeque<LogEvent>`（固定容量，例如 10k）
  - 可选：对 event 增加 `expires_at`（或 `ts` + 全局 TTL）以支持“时间窗口 tail”

- **per-channel buffers（LRU + TTL）**
  - 目标：既能 tail，又不会因为 channel 数量/订阅数量爆炸导致常驻内存膨胀
  - 结构建议（逻辑层面）：
    - `LruMap<ChannelId, ChannelLogBuffer>`（限制最大 channel 数，例如 1k~5k）
    - `ChannelLogBuffer` 内部为 `VecDeque<LogEvent>`（限制每 channel 最大条数，例如 2k）
    - 每个 buffer 维护：
      - `last_access`（LRU 依据：有新日志/有订阅读取都算 access）
      - `last_insert_ts` / `min_ts`（用于 TTL 清理加速）
  - 清理策略：
    - **TTL**：周期任务（例如每 5~30s）扫描 LRU map 的一部分（或按 last_access 排序）清理过期事件
    - **LRU eviction**：当 channel 数超过上限，直接淘汰最久未访问的 channel buffer（并丢弃其 ring）
    - **idle eviction**：channel buffer 若空且 `last_access` 超过 `idle_ttl`，可直接移除，进一步控内存

- **实时分发（follow）**
  - `tokio::sync::broadcast::Sender<LogEvent>` 或自研 fanout
  - 注意：broadcast 本身有固定 ring buffer，慢消费者会丢；要把丢弃统计透传给 UI

> 经验值：把“条数上限”和“channel 数上限”同时设好，再加 TTL 作为兜底，基本可以避免内存失控。

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
  - 将 events **先快速入队到 driver 内部有界队列（丢旧保新）**，再由后台 flush task **批量**回调给 host（避免阻塞/死锁）
  - 自动安装 `log -> tracing` 桥接（`tracing_log::LogTracer`），兼容 `log` 与 `tracing`
- SDK 代码组织（降低驱动开发心智负担）：
  - 在 `ng-gateway-sdk/src/southward/` 新增 `log.rs`：封装 LogSink ABI、bridge layer、内部队列与 flush task
  - 在 `southward/mod.rs` 的 `ng_driver_factory!` 宏中自动生成导出符号（`ng_driver_set_log_sink`/`ng_driver_set_max_level`/stats 等）并调用 `log.rs` 的初始化函数
  - 驱动开发者无需显式依赖/配置：只写 `tracing::info!` 或 `log::info!` 即可被桥接

Host（gateway side）：
- driver loader：加载成功后调用 `ng_driver_set_log_sink` 注册回调（必选步骤）
- lease manager 在调级时对相关 driver 调用 `ng_driver_set_max_level`
- 建议初始化顺序（减少重复 init 的复杂度）：
  - **先**注册 log sink
  - **再**调用 `ng_driver_init_tracing`（桥接模式）
- host 侧落盘与控制台统一（最佳实践）：
  - `log_sink` 的回调实现只做“copy + 快速入队”（避免阻塞 driver）
  - 后台 ingest worker 负责：
    - 解析 JSON Lines
    - 归因（driver_id/driver_type/channel_id 等）
    - 写入 LogHub（供 `/api/ws/logs`）
    - **重新发射为 host `tracing` event**（例如 target=`driver::<driver_type>`），从而复用 `ng-gateway-common/src/logger.rs` 的 console + rolling file 输出

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

一句话：`cdylib` 驱动是一个“自包含世界”，host 的 tracing subscriber 不会天然作用于 driver；继续在 driver 里 try_init 只能把日志打印到 driver 自己的 stdout/file，无法自然进入网关的“统一实时日志产品链路”，也无法统一落盘、动态调级与实时分发治理。

要做“Channel 级实时查看 + 动态调级 + TTL/离开恢复”，最稳健的路径是：

- **host 统一 LogHub + WS**
- **driver 通过稳定 ABI 把日志事件回传到 host**
- **host 统一走 `ng-gateway-common/src/logger.rs` 输出到控制台+滚动文件，同时写入 LogHub 供 UI**

