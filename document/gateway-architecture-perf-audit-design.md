# NG Gateway：架构统一抽象 + 大规模性能与稳定性审计（详细设计与计划）

> 目标：把 `ng-gateway` 打磨成可在 **大规模（10k devices / 500k points / 100k msg/s）** 下运行的“最佳实践级”高质量产品。
>
> 本文覆盖范围：`ng-gateway-core`、`ng-gateway-common`、`ng-gateway-sdk`、`ng-gateway-northward/*`、`ng-gateway-southward/*`。
>
> 关键约束（已确认）：保持 **driver/plugin 自带 tokio runtime（隔离优先）**，因此重点是控制线程数、任务膨胀、背压一致性和热点零拷贝，而不是强行合并到 host runtime。

---

## 0. 范围与非目标

### 0.1 本次要解决的核心问题

- **统一抽象**：southward 与 northward 的 supervisor / 重试 / 状态机 / 连接生命周期存在大量重复，需抽象为可复用骨架。
- **性能瓶颈**：热点路径（采集转发、变更检测、写点控制面、插件发送队列）中 clone/alloc/锁/任务数可能成为 100k msg/s 的瓶颈。
- **稳定性风险**：存在 `unwrap/expect` 等潜在 panic 点，以及 FFI/宏路径的不可控崩溃风险。
- **可观测性闭环**：指标需要覆盖“背压/队列/丢弃原因/等待耗时/IO耗时/in-flight”，以便压测和线上调优。

### 0.2 明确不做（本阶段）

- 不做“大改架构重写”，坚持 **最小化变更**：以抽公共模块、替换局部热点实现为主。
- 不做“把 driver/plugin runtime 统一到 host runtime”的破坏性改造（但会提供后续可配置路线）。
- 不引入磁盘级 WAL/持久化队列（未来可规划，但不在本次核心交付内）。

---

## 1. 当前架构骨架（现状对齐）

### 1.1 workspace 结构与职责（抽象边界）

- **`ng-gateway-sdk`**：协议层/模型层 + 插件/驱动 ABI（object-safe trait）+ 宏生成 C ABI + runtime wrapper（隔离 runtime）
  - southward：`Driver`/`DriverFactory`/`Runtime*`/`RuntimeDelta`/`ValueCodec` 等
  - northward：`Plugin`/`PluginFactory`/`NorthwardData`/`NorthwardEvent`/模板与 mapping 等
- **`ng-gateway-core`**：运行时主干：驱动/插件加载、拓扑管理、采集、转发、控制面（WritePoint/Command）
- **`ng-gateway-common`**：日志/metrics/event bus/settings 控制等基础设施
- **`ng-gateway-southward/*`**：南向协议驱动实现（OPCUA/IEC104/S7/Modbus/…）
- **`ng-gateway-northward/*`**：北向平台适配插件（Kafka/Pulsar/ThingsBoard/OPCUA Server）

### 1.2 核心数据流（采集链路 + 控制链路）

```mermaid
flowchart LR
  Collector -->|Arc<NorthwardData>| SouthwardDataBus
  SouthwardDataBus --> GatewayForwarder
  GatewayForwarder --> NorthwardRouter
  NorthwardRouter --> AppActorQueue
  AppActorQueue --> PluginRuntime
```

控制链路（以 `WritePoint` 为例）：

```mermaid
flowchart LR
  PluginRuntime -->|NorthwardEvent::WritePoint| AppEventBridge
  AppEventBridge -->|bounded_queue| GatewayEventProcessor
  GatewayEventProcessor -->|per_channel_Semaphore| SouthwardDriverWrite
  SouthwardDriverWrite -->|WritePointResponse| NorthwardSendToApp
```

### 1.3 你当前实现中“已经很好”的点（值得保留并推广）

- **southward device snapshot 变更检测**：`DashMap` + entry 原地更新 + TTL GC + `Arc::make_mut` 做 in-place filter（热路径少 clone）
  - 代码位于：`ng-gateway-core/src/southward/manager.rs`
- **控制面写串行化**：`DashMap<channel_id, Semaphore>`，实现 per-channel 串行、跨 channel 并行（符合大规模写入）
  - 代码位于：`ng-gateway-core/src/gateway.rs`
- **OPCUA supervisor 使用 `ArcSwapOption`**：session 句柄热读无锁（非常适合“频繁读、偶尔写”的连接句柄）
  - 代码位于：`ng-gateway-southward/opcua/src/supervisor.rs`

---

## 2. 大规模负载下的关键瓶颈与风险（钱在风险）

### 2.1 “任务膨胀”风险：SDK runtime wrapper 每消息 `tokio::spawn`

现状（SDK 宏生成的 runtime wrapper）：

- driver wrapper：在 actor loop 内对 Collect/Execute/Write/Delta 都 `tokio::spawn`
- plugin wrapper：actor loop 单线程消费，但 `ping` 等也通过 spawn 进入 plugin runtime

风险（大规模下典型症状）：

- 高频 `WritePoint` / `Execute` 突发时，spawn 数量可能超过 runtime 的承载，产生调度开销与内存压力。
- 由于 driver/plugin 采用 **独立 runtime**，如果每个 runtime 默认 `new_multi_thread()`，线程数会爆炸（N drivers + M plugins）。

设计原则（本次保持隔离 runtime 的前提下）：

- **必须有界（bounded in-flight）**：对 Execute/Write/Delta 引入类似 collect_sem 的 in-flight 限制。
- **必须可控线程数**：每个 driver/plugin runtime 提供 worker_threads 可配置（默认小值 1~2），避免线程膨胀。
- **必须可观测**：暴露每个 driver/plugin runtime 的 in-flight / queue depth / spawn 拒绝或排队等待。

### 2.2 潜在 panic 点（生产不可接受）

现状存在的典型类型：

- host 侧：全局 context `OnceCell` 的 `.expect(...)`、signal 注册 `.expect(...)` 等
- SDK 宏侧：`Mutex::lock().unwrap()`、`serde_json::to_vec(...).expect(...)`、runtime `build().expect(...)` 等

风险：

- 任何一个驱动/插件的极端边界条件都可能导致整个进程退出，造成大规模设备离线。

治理目标：

- **host side**：全部替换为 `Result` + 明确错误上下文，避免 panic。
- **FFI/宏 side**：从“panic”改为“返回错误码/降级行为 + 上报错误字符串到 host 日志/指标”，保证进程可存活。

### 2.3 `NGAppContext` 全局读锁热点风险

现状：

- `ng-gateway-common/src/lib.rs`：`OnceCell<RwLock<NGAppContext>>`，访问需要 `.read().await`

风险：

- 在高频路径如果出现频繁读取，会形成不必要的 async 锁竞争与调度开销。

治理策略：

- 热路径数据（metrics_hub、manager 引用、runtime index）**显式依赖注入**，不要每次都去全局 ctx 拿。
- `NGAppContext` 保留作为启动与低频控制面入口即可。

### 2.4 EventBus 可扩展性问题（长期写锁）

现状（`ng-gateway-common/src/event/mod.rs`）：

- `register_handler` 中 `tokio::spawn` 启动 dispatcher 时，持有 `dispatcher.write().await` 并在 `start()` 循环中长期占用，导致运行期难以再安全注册 handler。
- `downcast_ref(...).unwrap()` 存在潜在 panic。

风险：

- 增量模块化扩展（插件/组件动态注册 handler）会受阻，且存在崩溃隐患。

治理策略：

- 把 handlers 存储从 `Vec + 长期 write lock` 改成 “只读快照”（例如 `ArcSwap<Vec<...>>`）或 “读锁短持有”模式。
- 去掉 unwrap，改为错误上抛或降级为 no-op 并记录错误。

### 2.5 clone/alloc 热点（性能瓶颈）

基于全局统计与重点文件审阅，clone/alloc 热点集中在：

- `ng-gateway-core/src/southward/manager.rs`：device snapshot 更新时的 `pv.value.clone()`、String key 构造（非热点路径已标注）
- `ng-gateway-core/src/gateway.rs`：WritePoint 的 value 转换 `value.clone()`、driver_label String
- `ng-gateway-sdk/src/value.rs` / `ui_schema.rs`：大量 clone（其中 `ui_schema` 属于低频控制面，影响较小）

治理策略：

- codec API 演进：从“按值输入 + 上层 clone”改为“按引用输入（`&NGValue`）”，把 clone 集中到边界层。
- 对热路径字符串：优先 `Arc<str>` 或 `Bytes`（可借用、可切片）而不是反复 `String::to_string()`。

---

## 3. 统一抽象设计（重点：supervisor / 重试 / 背压 / 观测一致）

### 3.1 统一 SupervisorLoop（southward/northward 通用）

目标：

- 把 `opcua/s7/iec104/mc/... supervisor.rs` 的通用骨架收敛到 `ng-gateway-sdk` 的一个模块（组合式设计，避免泛型爆炸）。

建议 API 形态（组合优先）：

- `SupervisorLoop` 负责：
  - `CancellationToken` 协作取消
  - `RetryController` 基于 `RetryPolicy` 的退避/预算
  - `watch::Sender<ConnectionState>` 的状态广播
  - 可选 `ArcSwapOption<Handle>` 的“热读句柄”
- 协议实现只提供两个回调：
  - `connect_once() -> (Handle, EventLoop)`（一次连接尝试）
  - `run_event_loop(handle, event_loop, cancel_token) -> Outcome`（驱动一次连接生命周期）

关键设计点：

- **状态机一致**：Connecting / Connected / Reconnecting / Failed / Disconnected
- **失败策略一致**：区分“可重试/不可重试”，预算耗尽后进入 Failed 并停机或降级
- **Span 继承一致**：所有 spawn 必须 `.instrument(channel_span)`（参考 `document/southward-driver-logging-guidelines.md`）

### 3.2 统一控制面串行化与超时预算（WriteSerializers++）

现状 `WriteSerializers`（`DashMap<channel_id, Semaphore>`）是正确方向，建议抽成可复用组件：

- `PerKeySemaphore`（key = channel_id）
  - acquire 支持 timeout
  - 提供“剩余预算”计算（队列等待消耗掉总体 timeout）
  - 提供统一指标钩子（queue_wait_seconds）

收益：

- WritePoint/Execute/DebugAction 等控制面都能统一复用同一套语义与埋点，减少重复与行为差异。

### 3.3 保持独立 runtime 的前提下，提供“资源护栏”

对每个 driver/plugin runtime 增加护栏：

- **worker_threads 可配置**（建议默认 1~2）
- **max_inflight_execute / max_inflight_write / max_inflight_delta**（默认小值，必要时按协议调大）
- **强制有界队列**（已有 channel_capacity，但要配合 in-flight）

> 目标不是“追求并发越大越好”，而是避免在 100k msg/s 场景下被任务与分配拖垮，保证可预测的延迟与内存曲线。

---

## 4. 性能设计要点（面向 100k msg/s）

### 4.1 零拷贝与数据所有权策略

- **网关内部传递**：优先 `Arc<NorthwardData>`，避免大对象多次 clone（你当前已采用）
- **二进制 payload**：优先 `bytes::Bytes` 或 `Arc<[u8]>`，避免 `Vec<u8>` 反复分配
- **point_key/device_name**：优先 `Arc<str>` 作为跨模块共享字符串（你在 snapshot 与部分路径已使用）

### 4.2 锁策略（避免 await 下持锁）

硬规则：

- **禁止持有 DashMap guard 跨 `.await`**（你在 southward manager 多处已注释并遵守，建议推广为 repo 级规范）
- 对“频繁读、偶尔写”的句柄：用 `ArcSwapOption` 代替 `RwLock<Option<...>>`
- 对“写串行但读并发”的控制面：用 `Semaphore(1)` 而不是大锁

### 4.3 背压策略（全链路一致）

建议明确以下语义并通过指标验证：

- Collector -> Gateway：bounded queue，满时背压（await send）或可配置超时丢弃（记录 drop 原因）
- Gateway -> AppActor：按 `QueuePolicy`（Discard/Block+timeout）产生 drop（记录 drop 原因）
- Plugin -> Gateway（事件）：bounded queue，满时必须记录 drop（控制面事件通常不应丢，需要策略兜底）

---

## 5. 可观测性与压测闭环（不靠感觉改性能）

### 5.1 必须具备的核心指标（建议作为“最佳实践最小集”）

- **Queues**
  - depth / capacity
  - send_wait_seconds（如果有 await）
  - dropped_total（按原因分类：Full/Timeout/PolicyDiscard/Disconnected）
- **In-flight**
  - collector in-flight
  - per-driver collect/execute/write in-flight
  - per-plugin in-flight（process_data/publish）
- **I/O Latency**
  - driver collect/execute/write latency（success/fail 分开）
  - plugin publish latency（success/fail 分开）
- **Snapshot/Change Detection**
  - device_snapshots count
  - points baseline count（总量/每设备分布的近似）
  - snapshot_gc scanned/evicted counters

### 5.2 压测用例（两条链路）

- **采集链路**：Collector -> Southward -> forwarding -> Northward route
  - 目标：吞吐（msg/s）、P50/P95/P99 延迟、内存稳定、CPU 使用可解释
- **控制链路**：WritePoint -> serializer -> driver.write_point -> reply
  - 目标：并发写入下的排队等待曲线与超时比例，确保“跨 channel 并行、同 channel 串行”符合预期

---

## 6. 分阶段落地计划（最小化变更 + 可验收）

> 每一阶段都必须带“可验收标准”，避免长期悬空。

### Phase 1：稳定性兜底（消除 panic + 关键护栏）

- 全局扫描并移除 `unwrap/expect`（优先 core/common；SDK 宏侧改为错误码/降级）
- EventBus 去掉长期 write lock 与 unwrap
- 统一整理并固化“禁止持锁跨 await”规范（并在代码中补充注释与 guard drop）

验收标准：

- 在故意制造错误（驱动库缺符号、配置错误、连接失败等）情况下，进程不崩溃、错误可观测（日志 + 指标）。

### Phase 2：统一 SupervisorLoop（减少重复 + 行为一致）

- 在 `ng-gateway-sdk` 引入 `SupervisorLoop` 通用骨架
- 逐个收敛 southward 协议 supervisor（OPCUA/IEC104/S7/MC 等）
- 抽 `spawn_in_current_span` helper，降低驱动作者心智负担（参考 `document/southward-driver-logging-guidelines.md`）

验收标准：

- 所有驱动的连接状态机与重连行为一致；日志均带 `channel_id`，按通道日志级别过滤正确。

### Phase 3：SDK runtime wrapper 资源治理（隔离 runtime 但不失控）

- 为 driver wrapper 增加 Execute/Write/Delta 的有界 in-flight
- 为 driver/plugin runtime 增加 worker_threads 可配置（默认小值）
- 增加关键指标（in-flight、queue depth、spawn/try_send 失败原因）

验收标准：

- 100k msg/s 压测下，任务数与内存曲线可控；无明显调度抖动；控制面突发不会拖垮数据面。

### Phase 4：热路径 clone/alloc 优化（收益最大化）

- 演进 `ValueCodec` 等热路径 API：输入改为 `&NGValue`（减少上层 clone）
- 对热点字符串与 payload 逐步替换为 `Arc<str>` / `Bytes`
- 将“可借用的数据”尽量保持借用到边界层（插件编码/驱动协议编码处）

验收标准：

- 同等负载下 CPU 降低、alloc/clone 明显下降（以 flamegraph 或 metrics 验证）。

---

## 7. 代码组织与工程规范（让后续维护者也能写出同样高质量）

### 7.1 统一规范（建议以文档 + CI 约束落地）

- **严禁 unwrap/expect**（可在 CI 中对关键 crate 进行约束）
- **spawn 必须继承 span**（可在 CI 中检测 `tokio::spawn(` 行必须出现 `.instrument(` 或使用统一 helper）
- **不持锁跨 await**（DashMap guard / Mutex guard / RwLock guard）

### 7.2 抽象落点建议（模块归属）

- `ng-gateway-sdk`
  - `supervisor`：SupervisorLoop + 状态机/重试骨架
  - `runtime`：driver/plugin runtime wrapper 的资源护栏与通用工具
- `ng-gateway-core`
  - `control_plane`：WriteSerializers / timeout budget helper
- `ng-gateway-common`
  - `event`：EventBus 改造（可扩展、无 panic）
  - `metrics`：统一指标的注册/命名/粒度开关

---

## 8. 结论（你要的“最佳实践级产品”的可执行路径）

你现在的架构方向总体正确：object-safe SDK + bounded queues + 统一 core 运行时主干，并且在 southward snapshot、WritePoint 串行化、OPCUA 的 ArcSwap 这些点上已经体现出“高性能工程化思维”。\n\n真正要把产品做到大规模最佳实践，关键在于三件事：\n\n1) **消除 panic 与不可控行为**（FFI/宏侧尤其关键）\n2) **统一抽象**（supervisor / 重试 / span 继承 / 控制面串行化）让行为一致且可维护\n3) **资源护栏 + 指标闭环**（隔离 runtime 不等于放任线程与 spawn 爆炸）\n+
后续我会按 Phase 1→4 的顺序逐步落地，每个阶段都带“可验收标准”和“变更最小化”策略，确保稳定迭代不影响现有功能。

