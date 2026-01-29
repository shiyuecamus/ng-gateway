# NG Gateway：统一 Supervision（最终版）——极致性能的 Driver/Plugin 生命周期治理（含伪代码与落地计划）

> 本文是最终版设计：允许破坏式重构，不考虑最小变更/向前兼容。  
> 目标：把 southward/northward 所有“连接型组件”的生命周期治理（connect/init/run/reconnect/failed/stop）做成 **SDK 统一托管**，让 driver/plugin 作者只实现“不可统一的协议与业务逻辑”，并在热路径保持 **零虚调用、零额外分配、无锁热读句柄**。

---

## 0. 核心结论（先把争议点钉死）

### 0.1 `SupervisorLoop` 不应该由每个 driver/plugin 手动启动

如果 driver/plugin 仍需自己拼装并启动（span、broadcast、ArcSwap、retry、observer、spawn），抽象只统一了循环内部，没统一“接入闭环”，价值被稀释。  
**最终版要求：SDK 统一托管启动与闭环 wiring**，driver/plugin 作者只实现 traits（泛型/associated types）与业务方法。

### 0.2 `ProtocolSupervisor` 不跨 ABI 暴露：Factory 仍返回 `dyn Driver/Plugin`，但对象是 SDK wrapper

你们现有架构中，跨 ABI 动态加载边界稳定存在：

- `DriverFactory -> Box<dyn Driver>`
- `PluginFactory -> Box<dyn Plugin>`

而 “极致性能 + associated types” 的 supervision 内核必须是：

- `SupervisorLoop<P>` 对 `P` 静态分发（monomorphization）

因此最终版采用两条“硬边界”同时成立的方案：

- **跨 ABI 边界只暴露 object-safe 的 `dyn Driver/dyn Plugin`**（host 唯一依赖的动态接口）
- **但 `Driver/Plugin` 本身必须是 SDK “sealed” 的 host-facing API（外部 crate 禁止自行实现）**

也就是说：driver/plugin 作者不再实现 `Driver/Plugin` trait；他们实现的是“实现者类型”（component，普通 struct） + `Connector/Session/Handle`，并提供 `fn new(ctx)` 构造入口。  
`ng_driver_factory! / ng_plugin_factory!` 是唯一构建入口：**导出元信息 + 构建 `SupervisedDriver<T>` / `SupervisedPlugin<T>` 并返回 `Box<dyn Driver/Plugin>`**，从根上消灭“自己起 supervisor / 自己拼 wiring / 自己发状态”的分叉实现。

### 0.3 “连接成功后的动作（总召/订阅）”属于统一的 Session Init 阶段

OPCUA/IEC104/MQTT 等在连接成功后通常需要：

- OPCUA：create subscription / publish / (可选) 总召式 browse/read 或 monitored items
- IEC104：总召（GI）/对时/启动数据传输
- MQTT：subscribe topics / session present handling

最终版把它们定义为 **Session Init**：Supervisor 在“Connected(Ready)”之前必须执行 init 成功；失败按 **connect-phase** 规则分类与重试/预算处理，状态明确可观测。

---

## 1. 设计目标与硬约束

### 1.1 目标（必须）

- **全链路一致状态机**：Connecting / Initializing / Connected / Reconnecting / Failed / Disconnected
- **统一失败治理**：Retryable/Fatal/Stop 分类一致；预算耗尽进入 Failed（明确原因），不出现重连风暴
- **统一可观测性**：Span 传播硬约束（spawn 必 instrument）；状态/失败原因/退避/预算全部可观测；指标命名统一
- **极致性能**
  - 协议监督（connect/init/run）静态分发（泛型 + associated types）
  - 句柄热读无锁：`ArcSwapOption<Arc<Handle>>`
- 对外订阅只保留 **`watch<Arc<ConnectionState>>`**：统一语义与观测闭环（快照流，O(1) clone，消费者永远可读到最新状态）
- **接入零样板**：driver/plugin 作者不再写“启动 supervisor”样板；Factory 自动 wrap

### 1.2 硬约束（必须遵守）

- **跨 ABI 的稳定接口仍是 object-safe**：core 只依赖 `dyn Driver` / `dyn Plugin`
- **取消必须协作**：connect/init/run 都必须 cancel-safe（不能指望 drop future）
- **禁止热路径二次 spawn**（见 `document/southward-driver-logging-guidelines.md` 的思想扩展到 northward）

---

## 1.3 语义字典（必须读懂的抽象与边界）

本章把所有核心抽象的**语义/职责/生命周期/并发语义/对外可见性**一次写清楚，避免“名词对不上、实现走样”。

### 1.3.1 `Driver`（对 core 暴露的 southward 外观）

- **是什么**：core 唯一依赖的 southward 动态接口（`Box<dyn Driver>`），用于生命周期与数据/控制面的统一调度。
- **职责（控制面）**：
  - `start/stop`：启动/停止（由 SDK wrapper 统一实现）
  - `subscribe_connection_state`：提供连接状态快照订阅（`watch::Receiver<Arc<ConnectionState>>`）
- **职责（数据面）**：
  - `collect_data/write_point/execute_action` 等对 core 提供的入口
- **并发语义**：
  - 对 core 来说，`Driver` 可能被并发调用（collector 与控制面并发）。
  - 最终版要求：并发护栏（in-flight、队列策略）优先在 **SDK wrapper** 统一实现；协议作者不应在热路径随意 spawn。
- **生命周期**：
  - `Driver` 是“长寿命对象”（与 channel 实例同生共死），内部会跨多次重连产生多次 Session。
- **对外可见性**：
  - `Driver` 是跨 ABI 的 object-safe 边界，必须稳定。

### 1.3.2 `Plugin`（对 core 暴露的 northward 外观）

- **是什么**：core 唯一依赖的 northward 动态接口（`Box<dyn Plugin>`），由 AppActor 调用。
- **职责**：
  - `start/stop`：启动/停止
  - `subscribe_connection_state`：连接状态快照订阅（`watch::Receiver<Arc<ConnectionState>>`）
  - `process_data`：把 southward 数据发往平台（publish/uplink）
  - `events_tx`（或等价机制）：把平台下行业务事件（RPC/Command/WritePoint）发回 core
- **生命周期**：与 app 实例同生共死，内部跨多次重连产生多次 Session。

### 1.3.3 `SupervisedDriver<T>` / `SupervisedPlugin<T>`（SDK wrapper，最终版的“本体”）

- **是什么**：SDK 内置的统一托管层，实现 `Driver/Plugin`，并持有用户实现 `T` 与 supervision 内核。
- **职责（必须统一的地方都在这里）**：
  - 创建并绑定 span（`channel_id` / `app_id` 等）
  - 维护 `ConnectionState` 的快照并对外提供 `watch::Receiver<Arc<ConnectionState>>`
  - 创建 `HandleCell`（ArcSwap）并保证：**Connected(Ready) 才发布，断连立刻清空**
  - 注入 `RetryPolicy/RetryController/Budget`
  - 注入 Observer（metrics/tracing）
  - 启动并托管 SupervisorLoop（spawn + join + cancel）
  - 在 data-plane 入口统一热读 handle，并统一 NotConnected 语义
- **意义**：实现者不再手写“启动 supervisor”的样板；所有行为一致且可被 CI 强制。

### 1.3.4 Component（实现者写的“业务实现体”）

- **是什么**：协议/业务作者实现的具体类型（你“写的 driver/plugin”在最终版里就是它）。
- **职责**：
  - **实现 `Connector/Session/Handle`**（连接语义的唯一来源：connect/init/run/handle + error classify/summary）
  - 提供 `fn new(ctx)`（构造实现体，**同步且禁止 I/O**）
  - 实现 data-plane 方法（collect/write/execute/process_data 等），只关注业务
  - 绝不负责：状态机、退避预算、句柄发布、span/metrics wiring（全部由 wrapper 托管）
- **并发语义**：
  - 由 wrapper 决定是否并发调用 data-plane（例如通过 in-flight semaphore 控制）。
  - 实现者必须假设 data-plane 可并发（除非 wrapper 明确串行），内部用协议层串行化/连接池等保证正确性。

### 1.3.5 `DriverFactory` / `PluginFactory`（跨 ABI 的构造入口）

- **是什么**：动态加载边界的“构造器”，负责从 init context 创建实例。
- **最终版职责**：
  - `create_driver/create_plugin`：构造实现者类型 `T: *Component`，并立即用 SDK wrapper 包装：返回 `Box<dyn Driver/Plugin>`
  - runtime model 转换（channel/device/point/action/config schemas）仍由 factory 提供
- **明确不做**：
  - 不暴露 `Connector/Session/Handle`（associated types 不跨 ABI）
  - 不参与 supervision（不维护重连循环）

### 1.3.6 `Connector`（连接器：如何建立一次 Session）

- **是什么**：把“如何建连/认证/握手”抽象为静态分发接口。
- **职责**：
  - `connect()`：建立底层连接并返回 `Session`（尚未 Ready）
  - `classify_error(stage, err)`：严格分类 Retryable/Fatal/Stop（Connect/Init/Run 语义可不同）
  - `error_summary(err)`：生成 UI/告警友好摘要
- **生命周期**：通常与 `Component` 同寿命（可复用配置、共享依赖），每次 attempt 调用一次 connect。

### 1.3.7 `Session`（一次连接周期的运行时对象）

- **是什么**：一次连接成功后的“完整运行时对象”，持有 event loop 资源与状态。
- **职责**：
  - `init()`：**Session Init**（订阅/总召/恢复/预热），定义 Ready 的含义
  - `run()`：驱动连接生命周期直到断开/取消/失败
  - `handle()`：提供 handle（Ready 后才对外发布）
- **生命周期**：
  - 每次重连都会创建新的 Session；断线后 Session 被 drop（短寿命）。
- **并发语义**：
  - `init()` 与 `run()` 由 Supervisor 串行调用（不会并发），避免复杂竞态。

### 1.3.8 `Handle`（Ready 后的 data-plane 快路径句柄）

- **是什么**：从 Session 中抽出的“可用连接资源”，用于高频调用（publish/send/read/write）。
- **不是什么**：它不是 `Driver/Plugin`；不承载 start/stop/state/retry 等 control-plane 语义。
- **要求**：
  - 可被 `Arc` 持有并通过 `ArcSwap` 无锁热读
  - Connected(Ready) 才发布；断线立刻清空（避免过期连接继续被用）
- **是否需要 trait**：
  - 通常不需要。类型由 `Session::Handle` 关联类型确定，静态分发更快。

### 1.3.9 `SupervisorLoop`（生命周期治理内核）

- **是什么**：在一个任务里实现统一状态机：Connecting → Initializing → Connected → (Reconnecting)\* → Failed/Disconnected。
- **职责**：
  - 协作取消（connect/init/run/backoff 都可取消）
  - 退避/预算（RetryController）
  - 发布连接状态快照（`watch::Sender<Arc<ConnectionState>>`）
  - 发布/清理句柄（HandleCell）
  - 调用 observer（指标/日志/事件）
- **强约束**：
  - 所有 spawn 必须继承 span（`.instrument(span)`）

### 1.3.10 `Observer`（观测扩展点）

- **是什么**：把 metrics/log/event 从主逻辑里剥离出来的可插拔接口。
- **职责**：
  - 监听 state change / failure / backoff
  - 以低开销方式记录指标与结构化日志

### 1.3.11 `ConnectionState` vs legacy enum（已删除）

- **最终版**：统一到一个 `ConnectionState`（含 `Initializing`）。legacy enum（southward/northward 的旧连接状态 enum）与桥接层已按 Phase 4.0 **清零并删除**。
- **为什么要统一**：core 的 monitor/web/metrics 需要同一语义来展示与告警；否则每次新增阶段都要改两套体系。

---

## 2. 统一状态模型（强语义，统一展示）

> 现状（历史）：southward/northward 曾各自有旧连接状态 enum。当前已统一为 SDK 单一 `ConnectionState`，且不再保留薄桥接层。

```rust
#[derive(Clone, Debug)]
pub enum Phase {
    Disconnected,
    Connecting,
    Initializing,   // Session Init（总召/订阅/握手后置动作）
    Connected,      // Ready：句柄已发布，可提供 data-plane 服务
    Reconnecting,   // 退避等待中
    Failed,         // 预算耗尽或不可恢复错误
}

#[derive(Clone, Copy, Debug)]
pub enum FailureKind { Retryable, Fatal, Stop }

#[derive(Clone, Copy, Debug)]
pub enum FailurePhase { Connect, Init, Run }

#[derive(Clone, Debug)]
pub struct FailureReport {
    pub phase: FailurePhase,
    pub kind: FailureKind,
    pub summary: Arc<str>,                 // UI/告警友好
    pub code: Option<Arc<str>>,            // 可选：稳定错误码（用于聚合）
}

#[derive(Clone, Debug)]
pub struct RetryBudgetSnapshot {
    pub exhausted: bool,
    pub remaining_hint: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ConnectionState {
    pub phase: Phase,
    pub attempt: u64,
    /// Unix timestamp in milliseconds when this snapshot was emitted.
    /// Stable across process boundaries (UI/REST/WS safe).
    pub emitted_at_unix_ms: u64,
    /// Unix timestamp in milliseconds when the current phase was entered.
    /// Used to compute "stuck in Initializing for Xs" deterministically.
    pub phase_entered_at_unix_ms: u64,
    pub backoff: Option<std::time::Duration>,
    pub last_failure: Option<Arc<FailureReport>>,
    pub budget: RetryBudgetSnapshot,
}
```

> 关键：`Initializing` 是必须状态，否则“连接成功但订阅失败/总召失败”的阶段无法一致治理与展示。

### 2.1 对外状态订阅通道（唯一口径：`watch<Arc<ConnectionState>>`）

最终版只保留 **一条**对外订阅通道：`watch<Arc<ConnectionState>>`（快照流）。  
它承担“状态变化/失败/退避/预算耗尽”等监督信息的统一对外输出。消费者（core monitor/UI bridge）必须遵循以下规则：

- **snapshot-only 语义**：`watch` 只保证你能看到最新状态，不保证逐条消费每次状态变化；这正是运维/监控所需语义。
- **cheap clone**：对外 payload 必须是 `Arc<ConnectionState>`，禁止在状态发布路径做格式化与高频分配。
- **统一入口**：状态机唯一写入者是 `Supervised*` 内部的 `SupervisorLoop`；任何协议/插件不得另行广播“自定义连接状态”。

`subscribe_connection_state()` 的推荐签名（对外 API）：

```rust
/// Subscribe to the latest connection state snapshots.
///
/// Consumers should treat this as a "snapshot stream":
/// - The receiver always yields the latest snapshot.
/// - The payload is `Arc<ConnectionState>` so cloning is O(1).
fn subscribe_connection_state(&self) -> tokio::sync::watch::Receiver<Arc<ConnectionState>>;
```

> 备注：`watch` 本身就是 O(1) resync：消费者随时可以 `rx.borrow().clone()` 得到最新快照。

---

## 3. 统一 supervision 内核：Connector + Session（把“后置动作”纳入 Session Init）

最终版的抽象不再是 “connect_once + run_event_loop” 两个裸函数，而是 **连接返回一个 Session 对象**：

- Session 内部包含 event loop 资源
- Session 暴露 `handle()`（用于发布与 data-plane）
- Session 提供 `init()`（总召/订阅/后置握手动作）
- Session 提供 `run()`（驱动连接生命周期直到断开）

这样可以把 init 纳入统一管控，同时避免“connect_once 返回 (Handle, EventLoop) 但 init 需要两者协作”的 awkward API。

### 3.1 失败分类（必须由实现者显式提供）

Supervisor 不做“字符串猜测”。实现者必须提供错误分类，分别针对 Connect/Init/Run。  
类型定义复用第 2 章的 `FailurePhase` / `FailureKind`（避免重复定义导致语义漂移）。

### 3.2 Trait 设计（泛型 + associated types，零成本）

```rust
use tokio_util::sync::CancellationToken;
use tracing::Span;

/// Context injected into `Connector` and `Session` calls.
///
/// This contains control-plane signals and must be cheap to clone.
#[derive(Clone)]
pub struct SessionContext {
    pub cancel: CancellationToken,
    pub reconnect: ReconnectHandle,
    /// A span with stable labels (channel_id/app_id, type, ...).
    pub span: Span,
    /// Supervision attempt counter (monotonic, starts from 1).
    pub attempt: u64,
}

pub trait Session: Send + 'static {
    type Handle: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    fn handle(&self) -> &Arc<Self::Handle>;

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error>;
    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error>;
}

#[derive(Debug)]
pub enum RunOutcome {
    Disconnected,
    ReconnectRequested(Arc<str>),
    Fatal(FailureReport),
}

pub trait Connector: Send + Sync + 'static {
    type Handle: Send + Sync + 'static;
    type Session: Session<Handle = Self::Handle>;

    async fn connect(&self, ctx: SessionContext) -> Result<Self::Session, <Self::Session as Session>::Error>;
    fn classify_error(&self, phase: FailurePhase, err: &<Self::Session as Session>::Error) -> FailureKind;
    fn error_summary(&self, err: &<Self::Session as Session>::Error) -> Arc<str> { Arc::<str>::from(err.to_string()) }
    fn error_code(&self, _err: &<Self::Session as Session>::Error) -> Option<Arc<str>> { None }
}
```

> 重要说明（避免伪代码误导落地）：
>
> - `Handle` 的发布契约被制度化为 `&Arc<Handle>`：Supervisor 永远只做 `Arc::clone()`，不允许 `to_owned_handle()` 这类含糊接口。
> - 上面 trait 的写法表达的是“零成本单态化”的最终目标。实现时应优先使用 Rust 原生 `async fn in trait`（或等价 GAT/`impl Future` 方案），避免 `async_trait` 带来的装箱/间接调用开销。
>
> 这套 “Connector -> Session(init/run)” 是最终版的关键：把总召/订阅等后置动作变成一等公民，纳入统一状态机与预算治理。

---

## 4. SupervisorLoop（SDK 内核）：状态机 + 预算退避 + 句柄发布 + Span 强制

### 4.1 句柄发布（无锁热读）

```rust
pub struct HandleCell<H> {
    inner: arc_swap::ArcSwapOption<Arc<H>>,
}

impl<H> HandleCell<H> {
    pub fn store(&self, h: Option<Arc<H>>) { /* ... */ }
    pub fn load(&self) -> Option<Arc<H>> { /* ... */ }
}
```

### 4.2 SupervisorLoop 主流程（伪代码）

```rust
pub struct SupervisorLoop<C: Connector> {
    connector: C,
    retry: RetryController,
    cancel: CancellationToken,
    span: tracing::Span,
    /// Latest connection state snapshots (single source of truth).
    state_tx: tokio::sync::watch::Sender<Arc<ConnectionState>>,
    handle_cell: HandleCell<<C::Session as Session>::Handle>,
    observer: Box<dyn Observer>,
}

impl<C: Connector> SupervisorLoop<C> {
  pub async fn run(mut self) -> anyhow::Result<()> {
    let mut attempt = 0u64;
    // `publish(...)` MUST:
    // - update & send the latest `Arc<ConnectionState>` via `watch::Sender`
    // - notify `observer` (metrics/logging) ONLY on control-plane paths
    self.publish(Phase::Disconnected, attempt, None, None);

    loop {
      if self.cancel.is_cancelled() { self.cleanup_and_exit(attempt); return Ok(()); }

      attempt += 1;
      self.publish(Phase::Connecting, attempt, None, None);

      // --- CONNECT ---
      let mut sess = match self.connector.connect(self.ctx(attempt))
        .instrument(self.span.clone()).await {
        Ok(s) => s,
        Err(e) => {
          let class = self.connector.classify_error(FailurePhase::Connect, &e);
          let report = self.mk_report(FailurePhase::Connect, class, &e);
          if let Some(next) = self.on_failure(class, attempt, report.clone()).await? { continue; }
          return Err(anyhow::anyhow!("supervisor failed (connect)"));
        }
      };

      // --- INIT ---
      self.publish(Phase::Initializing, attempt, None, None);
      if let Err(e) = sess.init(self.ctx(attempt)).instrument(self.span.clone()).await {
        let class = self.connector.classify_error(FailurePhase::Init, &e);
        let report = self.mk_report(FailurePhase::Init, class, &e);
        self.handle_cell.store(None);
        if let Some(next) = self.on_failure(class, attempt, report.clone()).await? { continue; }
        return Err(anyhow::anyhow!("supervisor failed (init)"));
      }

      // 只有 init 成功后才发布 handle 并进入 Connected(Ready)
      // Handle publish is always an O(1) Arc clone.
      self.handle_cell.store(Some(Arc::clone(sess.handle())));
      self.retry.on_success(std::time::Instant::now());
      self.publish(Phase::Connected, attempt, None, None);

      // --- RUN ---
      let outcome = match sess.run(self.ctx(attempt)).instrument(self.span.clone()).await {
        Ok(o) => o,
        Err(e) => {
          let class = self.connector.classify_error(FailurePhase::Run, &e);
          match class {
            FailureKind::Stop => RunOutcome::GracefulStop,
            FailureKind::Fatal => RunOutcome::FatalFailure,
            FailureKind::Retryable => RunOutcome::RetryableFailure,
          }
        }
      };

      // 断连立刻清句柄（避免热读到过期连接）
      self.handle_cell.store(None);

      match outcome {
        RunOutcome::GracefulStop => { self.publish(Phase::Disconnected, attempt, None, None); return Ok(()); }
        RunOutcome::FatalFailure => {
          let report = Arc::new(FailureReport{ phase: FailurePhase::Run, kind: FailureKind::Fatal, summary: Arc::<str>::from("fatal"), code: None });
          self.publish(Phase::Failed, attempt, None, Some(report));
          return Err(anyhow::anyhow!("supervisor failed (run fatal)"));
        }
        RunOutcome::DisconnectedRetryable | RunOutcome::RetryableFailure => {
          let report = Arc::new(FailureReport{ phase: FailurePhase::Run, kind: FailureKind::Retryable, summary: Arc::<str>::from("disconnected"), code: None });
          if let Some(next) = self.on_failure(FailureKind::Retryable, attempt, report).await? { continue; }
          return Err(anyhow::anyhow!("supervisor failed (budget exhausted)"));
        }
      }
    }
  }
}
```

> 注：`to_owned_handle()` 已删除。最终版强制：`Session::handle()` 返回 `&Arc<Handle>`，避免实现者自行发明 owned/clone 语义导致性能与正确性走偏。

### 4.3 强制 Span 传播

- SupervisorLoop 内部所有 spawn 必须由 SDK 提供的 `spawn_in_span()` 完成，禁止 driver/plugin 自己 spawn supervisor。
- `connect/init/run` 的 future 也统一 `.instrument(span)`，确保第三方库日志具备 id 字段（channel_id/app_id）。

---

## 5. 终极接入：SDK Wrapper 托管一切（Driver/Plugin 作者只写 traits）

这一节会被完全重写：**最终版的接入不是“实现 Driver/Plugin + 自己起 supervisor + Factory 里包一层 runtime wrapper”，而是“只实现 Component/Connector/Session；Driver/Plugin 被 SDK sealed；Factory + 宏是唯一入口且负责闭环 wiring”。**

### 5.1 最终形态：`Driver/Plugin` 是 SDK sealed 的 host-facing API（外部 crate 禁止实现）

最终版要解决的不是“有没有抽象”，而是“抽象是否能强制闭环”。因此必须把 `Driver/Plugin` 设计成 **外部无法实现** 的接口：  
这样才能从机制上保证所有实例都经过 `Supervised*` 托管层，彻底消灭绕过 supervision 的实现分叉。

语义上：

- `dyn Driver/dyn Plugin`：**只**是 host 调用入口（跨动态加载边界存在）
- driver/plugin crate：实现的是 `*Component + Connector/Session/Handle`（静态分发、极致性能）
- SDK：提供 `SupervisedDriver<T>` / `SupervisedPlugin<T>`（唯一实现 `Driver/Plugin` 的类型）

### 5.2 外部实现者只写三类东西（完全静态分发）

#### 5.2.1 Component（业务实现体）

实现者只关心“协议与业务”，不关心状态机与 wiring。

- **必须提供 identity**：`driver_kind/plugin_kind` 为 `&'static str`（零分配、低基数 labels）
- **必须提供 connector 构建**：捕获 cfg/依赖，但不做 I/O
- **data-plane 方法必须只做业务**：不得自己维护重连 loop / publish state / retry budget

#### 5.2.2 `Connector`（一次建连如何创建 Session）

`Connector` 决定 connect 阶段的细节与错误分类（Connect/Init/Run 三阶段）。

#### 5.2.3 `Session`（一次连接周期的运行时对象）

`Session` 必须显式拆出 `init()`：

- `connect()` 只负责拿到“可初始化的 session”
- `init()` 做订阅/总召/预热/恢复，**成功后才允许 Ready**
- `run()` 驱动直到断开/失败/取消
- `handle()` 提供热路径句柄（由 wrapper 通过 `ArcSwapOption` 发布/清空）

### 5.3 `SupervisedDriver<T>` / `SupervisedPlugin<T>` 的职责（唯一闭环托管层）

`Supervised*` 必须同时完成三件事，缺一不可：

- **生命周期闭环**：统一状态机 + Connect/Init/Run 分阶段失败分类 + budget/backoff 决策一致
- **性能闭环**：热路径只做一次间接调用（`dyn Driver/Plugin`），内部对 `T` 完全单态化；句柄热读无锁（`ArcSwapOption`）
- **可观测闭环**：所有阶段耗时/失败/退避/预算都通过 `Observer` 结构化上报；禁止在热路径拼字符串/高频打日志

关键实现约束（最终版强制）：

- **`cdylib` 现实约束：tokio/tracing 很难与 host 可靠共享**  
  只要 host 与 `cdylib` 各自静态链接了一份 tokio（实际工程里极常见），tokio 的 TLS / 全局状态会在两个库里“分裂”，导致插件侧 `Handle::current()` 等基于 TLS 的能力无法看到 host runtime。  
  tracing 也类似：插件侧如果走 `tracing_subscriber::try_init()`，那是“初始化插件自己的 subscriber”，并不会天然汇入 host。  
  **因此最终版必须保留“插件内 runtime + host 日志桥接”这一层**：`NG_RUNTIME`、`RuntimeAwareDriver/RuntimeAwarePlugin`、`ng_driver_set_log_sink/ng_driver_set_max_level`（以及 northward 的 `ng_*_init_tracing`）。
- **runtime 的最佳实践**：每个 `cdylib` 只允许存在 **一个** runtime（`static Lazy<Runtime>`），并被该库内所有实例共享；禁止每个 channel/app 实例各自新建 runtime（那会造成线程/计时器/IO driver 膨胀）。
- **spawn 统一入口**：由 supervision 层统一 spawn，并强制 `.instrument(span)`；但是“spawn 到哪个 runtime”取决于部署形态：  
  - builtin（静态链接进 host 的驱动/插件）：跑在 host runtime  
  - `cdylib`（动态加载）：跑在该 `cdylib` 的 `NG_RUNTIME`
- **Handle 发布规则**：只有 `init()` 成功后才 publish handle；任何断连/失败立刻清空 handle

### 5.4 “性能闭环”到底闭在哪里（从观测到调参）

最终版把“性能治理”视为 supervision 的一部分，而不是分散在各处的经验写法：

- **观测输出（由 `Supervised*` 产生）**
  - phase 迁移（Connecting/Initializing/Connected/Reconnecting/Failed/Disconnected）
  - 失败分阶段（Connect/Init/Run）+ kind（Retryable/Fatal/Stop）+ code/summary（低分配）
  - 退避与预算（backoff 秒数、剩余预算、耗尽点）
  - 阶段耗时（connect/init/run 的 wall time）
- **闭环输入（由 host 注入到 init context / runtime model）**
  - `RetryPolicyByStage`（按 connect/init/run 分阶段策略）
  - `collect_max_inflight` / buffer 容量 / drop policy（由配置与观测共同决定）
  - 采集周期与分组策略（collector 基于观测的 latency/timeout/失败率做策略优化）

最终版要求：这些闭环输入都必须能通过 runtime model 热更新（hot-apply），并由 `apply_runtime_delta` 进入 `Supervised*` 的 control-plane，而不是各协议自己开“小配置通道”。

---

## 6. Factory 与 `ng_*_factory!` 宏（最终版）：唯一构建入口 + 零样板 + 零额外层

> 本章是你问的核心：最终的 `ng_driver_factory! / ng_plugin_factory! / DriverFactory / PluginFactory` 应该怎么设计，才能同时满足“极致性能 + 强语义 + 闭环”。

### 6.1 最终版三条硬规则

- **硬规则 A：外部 crate 不允许实现 `Driver/Plugin`**  
  `Driver/Plugin` 是 SDK sealed 的 host-facing API；外部只能实现 `*Component/Connector/Session`。
- **硬规则 B：Factory 是“构造 + 低频转换”，不承载运行时语义**  
  Factory 只负责：创建 supervised 实例、提供 schema、做 runtime model 转换；不维护 supervisor，不自建 runtime，不引入 actor/mpsc 层。
- **硬规则 C：宏是唯一导出点**  
  `ng_driver_factory! / ng_plugin_factory!` 负责导出所有动态加载需要的符号（版本/元信息/schema bytes/create_factory），并生成最终的 factory 实现（避免人为写错）。

### 6.2 `DriverFactory` / `PluginFactory` 的最终语义（对 host 的最小稳定面）

最终版将 Factory 定义为“host 侧需要的一切，但仅限低频路径”：

```rust
pub trait DriverFactory: Send + Sync {
    /// Create a new channel-scoped driver instance (no I/O; I/O belongs in start()).
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>>;

    /// Low-frequency model conversions (import/apply-delta paths).
    fn convert_runtime_channel(&self, channel: ChannelModel) -> DriverResult<Arc<dyn RuntimeChannel>>;
    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>>;
    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>>;
    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>>;
}

pub trait PluginFactory: Send + Sync {
    /// Create a new app-scoped plugin instance (no I/O; I/O belongs in start()).
    fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>>;

    /// Low-frequency config conversion (import/apply-config paths).
    fn convert_plugin_config(&self, config: serde_json::Value) -> NorthwardResult<Arc<dyn PluginConfig>>;
}
```

**关键点**：

- `create_*` 返回的一定是 `Supervised*<T>`（因为外部无法实现 `Driver/Plugin`，只能由宏生成的 factory 返回）
- 这些 trait 只服务低频路径：实例化/导入/热更新；热路径完全走 `Supervised*` 的句柄快路径

### 6.3 `ng_driver_factory!` / `ng_plugin_factory!` 的最终职责与生成物

最终版宏要做两件事：**生成导出符号** 与 **生成 factory 实现**。

#### 6.3.1 宏输入（最终版强制形态：形成完整闭环）

宏输入的主要字段（driver/plugin 同理，仅 type/name 不同）：

- `name`：驱动/插件名称
- `description`：驱动/插件描述（可选但强烈建议提供）
- `driver_type` / `plugin_type`：驱动/插件类型（**唯一稳定 key**）
- `component = MyModbusDriver` / `component = MyPlugin`：实现者类型（实现 `Connector/Session/Handle`，并提供 `fn new(ctx)` 构造入口）
- `metadata_fn = build_metadata`：元信息/Schema 生成函数（返回 `DriverSchemas` / `PluginConfigSchemas`）
- `model_convert = MyConverter`：模型转换器（必传参数；如果不填，则使用 SDK 默认模板 converter）
- `channel_capacity = ...`：ABI 调度层参数（可选，不填有默认值）
- `collect_max_inflight = ...`：Southward collect 并发预算（可选，不填有默认值）

**迁移期兼容说明（已硬删除 legacy 宏入口）**

- 旧宏输入形态（例如 legacy `factory` 参数）已从 `ng_driver_factory! / ng_plugin_factory!` 中移除。
- 任何继续使用旧形态的代码将会在编译期直接失败（`compile_error!`），不会再走任何兼容/回退实现。
- 当前唯一允许的宏输入形态：`component + model_convert`（以及 `metadata_fn` 等最终版字段）。

##### 6.3.1.1 `model_convert` 的 trait 设计（必须明确，否则闭环会断）

Southward converter（driver 侧）负责把 DB/API model 转成 runtime trait objects（低频路径）：

```rust
/// Convert database/API models into runtime trait objects (low-frequency path).
///
/// Constraints:
/// - MUST be deterministic and side-effect free.
/// - MUST NOT perform any network/blocking I/O.
/// - MUST validate model and return actionable errors.
pub trait SouthwardModelConverter: Send + Sync + 'static {
    fn convert_runtime_channel(&self, channel: ChannelModel) -> DriverResult<Arc<dyn RuntimeChannel>>;
    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>>;
    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>>;
    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>>;
}
```

Northward converter（plugin 侧）负责把 JSON config 转成可 downcast 的 `PluginConfig`（低频路径）：

```rust
/// Convert plugin config JSON into a typed, downcastable `PluginConfig` object.
///
/// Constraints:
/// - MUST validate schema and return actionable errors.
/// - MUST NOT perform I/O.
pub trait NorthwardModelConverter: Send + Sync + 'static {
    fn convert_plugin_config(&self, config: serde_json::Value) -> NorthwardResult<Arc<dyn PluginConfig>>;
}
```

SDK 必须提供默认模板实现（例如 `DefaultSouthwardModelConverter` / `DefaultNorthwardModelConverter`），宏在未显式指定 `model_convert` 时绑定默认实现，以保证“零样板接入”。

然后一行宏完成所有导出与工厂实现：

```rust
ng_driver_factory!(
    name = "Modbus",
    description = "Modbus protocol driver",
    driver_type = "modbus",
    component = MyModbusDriver,
    metadata_fn = build_metadata,
    model_convert = MyConverter,
    channel_capacity = 500,         // optional
    collect_max_inflight = 8,       // optional
);
```

plugin 侧同理：

```rust
ng_plugin_factory!(
    name = "Kafka",
    description = "Kafka northbound adapter",
    plugin_type = "kafka",
    component = MyPlugin,
    metadata_fn = build_metadata,
    model_convert = MyConverter,
    channel_capacity = 10000,       // optional
);
```

#### 6.3.2 宏生成的导出符号（最终版最小集）

保持你们 loader 现有的“探测 + gating”能力，并把 `cdylib` 必需的“运行时/日志桥接”明确收敛为标准符号集（**强制约定，禁止各库各写各的**）：

- `ng_*_api_version()`：API 版本
- `ng_*_sdk_version()` / `ng_*_version()`：版本信息
- `ng_*_type()` / `ng_*_name()` / `ng_*_description()`：元信息
- `ng_*_metadata_json_ptr(out_ptr, out_len)`：静态 bytes（零分配跨边界）
- `create_*_factory() -> *mut dyn *Factory`：返回宏生成的 factory
- （driver 额外）`ng_driver_set_log_sink(...) / ng_driver_set_max_level(...)`：日志桥（由 host 控制）
- `ng_*_init_tracing(debug: bool)`：初始化插件侧 tracing（仅用于“插件侧日志产出”，真正的日志汇聚由 log sink bridge 完成）
- `NG_RUNTIME`（doc-hidden 静态）：每个 `cdylib` 一个 tokio runtime，库内共享

**明确保留并“重新定义语义”（最终版最佳实践）**：

- `NG_RUNTIME`：不是“各自为政的 runtime”，而是 **cdylib 的执行载体**（解决 tokio TLS 分裂问题）。库内所有工作（supervisor loop、协议 eventloop、actor 消费）必须跑在这里。
- `RuntimeAwareDriver/RuntimeAwarePlugin`：不是“业务层 actor”，而是 **ABI runtime adapter**：把 host 的 `dyn Driver/Plugin` 调用转发到 `NG_RUNTIME` 上执行，并提供：
  - 取消（CancellationToken）
  - backpressure（bounded queue / try_send）
  - 并发护栏（Semaphore / max_inflight）
  - 可观测字段注入（channel_id/app_id 等）

> 性能说明：在 `cdylib` 形态下，host 与插件之间不可避免存在一次“跨 runtime 调度”。最终版的目标不是消灭它（做不到），而是让这次调度成为**唯一且可控**的开销，并把所有生命周期治理与观测闭环全部统一到 SDK。

### 6.4 host 侧如何与 supervision 闭环对接（必选 wiring）

最终版要求 host 在创建 `*InitContext` 时就把闭环输入注入进去：

- `observer: Arc<dyn Observer>`：**已绑定低基数 labels**（southward: `channel_id + driver_kind`；northward: `app_id + plugin_kind`）
- `retry_policy: RetryPolicyByStage`：来自 runtime model / app config
- `runtime_handle/spawn`（如需）：用于极少数必须在 host runtime 上执行的辅助任务（但 supervision 主循环必须由 SDK wrapper spawn）

`Supervised*` 在 `create_*` 时绑定这些输入，在 `start()` 时启动 SupervisorLoop。  
这样从“创建 → 运行 → 观测 → 热更新调参”的闭环就不再散落在 driver/plugin 作者的代码里，而是由 SDK 强制一致实现。

---

## 7. Retry/Budget（统一、可配置、默认最优）

本章是你提出的“深度剖析 + 去重”的关键点：**计划里的 Retry 设计与现有代码确实存在重复**，而且现网已经在大量使用。

### 7.0 现状代码剖析（必须先统一认知）

#### 7.0.1 SDK 已经有统一实现（不是“待设计”）

现有实现就在 `ng-gateway-sdk/src/retry.rs`，并已被全局复用（southward drivers / northward plugins / core collector / 系统设置 hot-apply）：

- `RetryPolicy`（配置模型）：指数退避参数 + `max_attempts`（次数预算）+ `max_elapsed_time_ms`（时间预算）
- `RetryController`（控制器）：内部持有 `backoff::ExponentialBackoff`，并用 `retries_used` 叠加实现次数预算
- `RetryDecision::{RetryAfter, Exhausted}`（决策结果）

也就是说：**文档里再设计一套 `RetryPolicy/RetryController/Budget` 会直接重复现有代码**，并造成“语义漂移”（计划说一套、代码跑另一套）。

#### 7.0.2 Retry 配置来源：你的判断是对的（来自 channel / init context）

- **Southward**：实际使用的是 `ChannelModel.connection_policy.backoff`（`ng-gateway-sdk/src/southward/model.rs` 的 `ConnectionPolicy.backoff: RetryPolicy`）。现网 supervisor 代码也已经这样写：`RetryController::new(&channel.connection_policy.backoff)`（见各 driver 的 `*/src/supervisor.rs`）。
- **Northward**：`NorthwardInitContext.retry_policy: RetryPolicy`（`ng-gateway-sdk/src/northward/mod.rs`），各插件 supervisor 直接用它构造 `RetryController`。
- **Core Collector**：也使用同一套 `RetryPolicy/RetryController`（见 `ng-gateway-core/src/collector.rs`）。

#### 7.0.3 现状痛点（决定“最佳实践”应该怎么改）

现状最大的问题不是“有没有 Retry”，而是：

- **状态语义不一致**：不同 supervisor 对 `Failed` / `Reconnecting` 的发送时机不一致，有的耗尽预算后直接 return，有的会额外发送 `Failed("retry budget exhausted")`。
- **错误载荷成本偏高**：历史上的 `Failed(String)` 连接状态载荷让观测/消费端（历史上是 `ChannelMonitor` / `AppActor` 的 monitor 任务）每次 `.clone()` 都会复制字符串；在抖动场景会放大 GC/alloc 压力。
- **缺失“阶段语义”**：目前没有把失败明确归类为 Connect/Init/Run（也缺 `Initializing`），导致 UI/告警/运维无法精准定位“连上了但订阅失败/总召失败”这一类问题。
- **成功语义分散**：业务里到处手写 `retry.reset()` 的触发点（例如“是否 seen_active 决定 reset”），长期维护会导致同类协议行为不一致。

因此，最终版的“最佳实践”应该是：**以现有 `retry.rs` 为唯一基线做升级**，而不是新造一套。

### 7.1 策略来源（重要：默认仅兜底）

最终版的原则是：**策略来源必须来自 runtime model / init context**。

- **Southward**：`RuntimeChannel::connection_policy().backoff`（见 `ConnectionPolicy.backoff`）
- **Northward**：`NorthwardInitContext.retry_policy`

SDK `SupervisedDriver/SupervisedPlugin` 在构造时会把上述策略固化在组件实例里，并在 `start()` 时用于创建 retry controller。

### 7.2 最佳实践升级：把 “Retry” 提升为可观测、可解释、可预测的“连接预算”

> 允许破坏式重构，因此这里给出“最终形态”，不考虑迁就旧 enum/旧字符串错误。

#### 7.2.1 结构化预算（推荐替换 `Failed(String)` 语义）

- **目标**：让 retry/backoff/失败原因可以被统一观测（metrics/log/ui），并避免在事件广播 clone 中复制大字符串。
- **建议**：把连接状态从 `*ConnectionState::Failed(String)` 升级为结构化 payload（内部字段用 `Arc`/`Copy`，clone 成本恒定）。

设计建议（示意）：

- `FailurePhase = Connect | Init | Run`
- `FailureKind = Retryable | Fatal | Stop`
- `FailureReport { phase, kind, summary: Arc<str>, code: Option<Arc<str>> }`
- `ConnectionState { phase, attempt, backoff, last_failure: Option<Arc<FailureReport>>, ... }`

并把 “budget exhausted” 变成一等原因：`FailureKind::Fatal + code="budget_exhausted"`（或单独枚举）。

#### 7.2.2 RetryController 最佳实践 API（收敛 reset/成功点）

现有 `RetryController` 只有 `reset()` 与 `on_failure()`；为了让各协议不再“各写各的成功点”，建议升级为：

- `on_success()`：明确表达“连接达到 Ready（Connected）或稳定运行达到阈值”时的成功语义（内部 reset backoff + 清 budget 计数）
- `on_failure()`：仍返回 `RetryDecision`，但需同时返回“当前 attempt/backoff snapshot”，用于统一状态发布与 observer

这样 `SupervisorLoop` 可以严格做到：

- `Connected(Ready)` 之后，任何断开都走同一条 `on_failure()` 路径
- connect/init/run 三阶段错误都能带上 stage 分类，发布一致的 state

#### 7.2.3 分阶段策略（connect/init/run 可用不同窗口）

工业协议里“连不上”和“跑着跑着断”经常需要不同策略：

- **Connect**：短窗口、快速失败（避免占用线程/句柄），例如 `initial=200ms, max=5s, max_elapsed=30s`
- **Init**：中窗口（例如订阅/总召可能依赖设备慢响应）
- **Run**：长窗口但需要抑制风暴（例如 `max_interval=60s` + jitter）

最终版建议把 `RetryPolicy` 扩展为：

- `RetryPolicy`（默认/通用）
- `RetryPolicyByStage { connect: Option<RetryPolicy>, init: Option<RetryPolicy>, run: Option<RetryPolicy> }`

并由 `RuntimeChannel.connection_policy` / `NorthwardInitContext` 提供。

---

## 8. Observer/指标/日志（统一命名与低开销）

本章是第二个“深度剖析”的关键点：**你文档里提的 Observer 不是从 0 开始**，现网已经存在“观察者模式”的落地形态，只是它分散在不同层（supervisor / core monitor / app actor）。

### 8.0 现状代码剖析：当前 Observer 在哪里？

#### 8.0.1 Southward：历史上 `ChannelMonitor` 承担了“Observer”职责（已收口）

历史上 core 侧存在 `ChannelMonitor`（已删除），承担了典型 Observer 的副作用职责：

- 订阅 `Driver::subscribe_connection_state()`（`watch<Arc<ConnectionState>>` 快照流）
- 去重 state 变化（`last_state`）
- 更新 Prometheus（connected gauge / reconnect count / connect_failed / disconnect）
- 在 `Connected`/`Disconnected|Failed` 迁移时发 `DeviceConnected/DeviceDisconnected` northward 事件

当前已收口为 **ObserverConsumer**：

- southward side effects 统一在 `ng-gateway-core/src/southward/observer.rs`（`SouthwardChannelObserverFactory`）
- 不再需要额外的 monitor task 去订阅 state

#### 8.0.2 Northward：`AppActor` 同时扮演“Observer + 数据面调度者”

`ng-gateway-core/src/northward/actor.rs` 中：

- 历史上通过 `AppActor::spawn_connection_monitor()` 订阅插件连接状态并更新指标、flush buffer（已删除该 monitor）
- 同时 `send_data()` 与 worker loop 也会读取连接态决定是否丢弃/缓冲

也就是说：**Observer 的副作用（metrics/log/event/buffer flush）目前主要在 core 层实现**，并没有一个统一的“监督事件总线”把所有连接生命周期信息结构化输出。

#### 8.0.3 Supervisor 层的重复工作（需要被收口）

各协议/插件 `*/src/supervisor.rs` 里又各自：

- 自己决定什么时候发 `Connecting/Reconnecting/Failed`
- 自己拼接错误字符串（会分配）
- 自己打日志（不统一、容易风暴）

这就是文档中 Observer 设计必须解决的“重复点”。

### 8.1 Observer（最终版统一扩展点：状态机回调，而不是散落的日志/指标）

SupervisorLoop 内只产生统一的状态快照（`watch<Arc<ConnectionState>>`），不直接写 prometheus；由 Observer 负责（推荐在 core 内以订阅任务消费状态快照实现 Metrics/Logging）：

- `on_state_change(ConnectionState)`
- `on_failure(FailureReport)`
- `on_backoff(Duration, budget)`

**当前代码落地状态（已闭环）**：

- SDK 已提供并启用（通过宏注入）：
  - `ng-gateway-sdk/src/supervision/observer.rs`
    - `Observer`（`on_state` / `on_failure` / `on_backoff`）
    - `ObserverFactory`（按实例绑定低基数 labels）
    - `SouthwardObserverLabels` / `NorthwardObserverLabels`
  - `ng-gateway-sdk/src/supervision/loop.rs`
    - `publish_state()` 调用 `observer.on_state(...)`
    - failure / backoff 路径调用 `observer.on_failure(...)`
  - `ng-gateway-sdk/src/southward/mod.rs` / `ng-gateway-sdk/src/northward/mod.rs`
    - `ng_driver_factory! / ng_plugin_factory!` 从 `InitContext.observer_factory` 取 factory 绑定 labels，并使用 `SupervisorLoop::new_with_span(..., observer, span)` 注入
-- Core 已提供 host-owned 实现（已形成闭环，且不再依赖订阅 monitor task）：
  - `ng-gateway-core/src/observability/supervision.rs`
    - `LoggingObserver`：统一输出结构化连接生命周期日志（低频）
  - `ng-gateway-core/src/northward/actor.rs`
    - `NorthwardAppObserverFactory`：在 observer 回调里更新 prom + flush buffer（非阻塞 `try_send`）
  - `ng-gateway-core/src/southward/observer.rs`
    - `SouthwardChannelObserverFactory`：在 observer 回调里更新 prom + connected_channels + 发设备连接事件（非阻塞 `try_send`）
  - `ng-gateway-core/src/southward/manager.rs` / `ng-gateway-core/src/northward/manager.rs`
    - 构造 `SouthwardInitContext/NorthwardInitContext` 时注入 per-instance `observer_factory`

**尚未完成（按 8.4 继续收口）**：

- `AppActor::send_data()` 等热路径仍会读取 `plugin.subscribe_connection_state()` 的快照；后续按计划改为 core 侧维护 atomic 快读（避免 clone/borrow 链路）

### 8.2 指标建议（最终版最小集）

统一命名 `supervisor_*`，labels 控制基数：

- southward：`channel_id` + `driver_kind`
- northward：`app_id` + `plugin_kind`

指标：

- `supervisor_phase{...}`（gauge / info）
- `supervisor_connect_success_total{...}`
- `supervisor_failure_total{phase=connect|init|run, kind=retryable|fatal, ...}`
- `supervisor_backoff_seconds{...}`（histogram）
- `supervisor_budget_exhausted_total{...}`

> 不允许把 endpoint/topic 之类高基数放 label。

### 8.3 Observer 注入与组合（metrics/logging/tracing）——落地方案（强制）

你要求 southward/northward 的连接治理统一到 SDK `supervision`，那么 **观测面也必须统一**：状态变化、失败分类、退避、预算耗尽全部从同一条 Observer 管道产出，禁止每个协议/插件各自打点、各自打日志。

#### 8.3.1 设计目标（性能优先）

- **热路径零分配**：Observer 回调不得 `format!/to_string`，不得持锁；只允许读取/累加/记录
- **最少 clone**：载荷只允许 cheap clone（`Arc/Copy`）；禁止复制大对象
- **可完全关闭**：关闭时为 NoopObserver（零额外开销）
- **统一命名与低基数 labels**：严格遵守 8.2（`channel_id/app_id + kind`）

#### 8.3.2 注入点与生命周期（必须收口）

- **唯一构建入口**：由 host（core）侧统一构建 `ObserverFactory`
- **每个实例绑定 labels**：在启动 `SupervisedDriver/SupervisedPlugin` 时，factory 生成一个“已绑定 labels”的 Observer：
  - southward labels：`channel_id` + `driver_kind`
  - northward labels：`app_id` + `plugin_kind`
- wrapper 把 Observer 传入 `SupervisorLoop`，并保证：
  - Observer 仅在“状态迁移/失败路径”被调用（禁止进入 data-plane 热路径）

> 说明：Observer 允许 `Box<dyn Observer>`（可插拔边缘），但 **禁止**把 dyn 调用放进 collect/write/process_data 等热路径。

#### 8.3.3 组合策略（后续实现方向，按需落地）

- **MetricsObserver**
  - 写入 Prometheus：`supervisor_phase / supervisor_failure_total / supervisor_backoff_seconds / supervisor_budget_exhausted_total ...`
- **LoggingObserver**
  - 仅在 phase 变化 / failure 发生时打结构化日志（包含 channel_id/app_id），严禁高频刷屏
- **TracingObserver（可选）**
  - 把 connect/init/run 生命周期纳入 tracing span（严格继承 channel/app span）
  - 默认关闭（避免无谓开销），但设计必须预留注入点
- **CompositeObserver**
  - `CompositeObserver(Vec<Arc<dyn Observer>>)`：按开关装配（空 vec 即 Noop）

### 8.4 最佳实践：把 core 现有的 monitor/actor 变成“Observer 的实现”，而不是“状态机的实现者”

最终版的结构应该是：

- **状态机/退避/预算/句柄发布**：只在 SDK 的 `SupervisorLoop` 内实现（唯一实现）
- **指标/日志/事件/缓冲刷新**：只作为 Observer（订阅监督事件）实现（可在 core 内）

这样可以做到：

- southward/northward 所有协议不再各写 supervisor.rs
- core 不再需要在多个地方猜测状态语义（只消费结构化事件）
- 观测面不会因为协议差异而“行为不一致”

> 这也是你要求的“最佳实践极致质量”：**把 side effects 与 state machine 严格分离**。

---

## 9. 强制工程规范（CI 级别，防止回退）

### 9.1 禁止自写 supervisor 主循环

对 `ng-gateway-southward/**` 与 `ng-gateway-northward/**`：

- 禁止出现自定义 “reconnect loop/state machine” 模板（用脚本/AST/lint 约束）
- supervisor 的启动必须通过 SDK wrapper（`SupervisedDriver/SupervisedPlugin`）

### 9.2 spawn/span 规则

任何 spawn 必须走 SDK `spawn_in_span()` 或 `.instrument(span)`，并保证 span 含 `channel_id` 或 `app_id`。

### 9.3 取消语义

connect/init/run 必须协作取消；sleep/backoff 必须可取消；退出必须清句柄。

### 9.4 零拷贝 / 最少 clone（强制，热路径红线）

**目标**：supervision 内核与 data-plane 热路径做到“零额外分配、最少 clone”，任何不可避免的 clone 必须是 **cheap clone（Arc/Copy）**。

**硬规则（CI/Code Review 必须挡住）**

- **禁止热路径 `String`/`Vec` 构造与 `to_string()`**：错误信息、失败原因、日志字段必须预先结构化或用 `Arc<str>` 保存；只允许在“失败路径/最终落库/最终展示”做一次性转换。
- **失败报告使用 `Arc<FailureReport>` + `Arc<str>`**：避免在 retry/backoff 循环中重复复制大字符串。
- **Handle 必须可 lock-free 热读**：只允许 `HandleCell(ArcSwapOption)` + `Arc<Handle>`，禁止在 collect/write/process_data 中出现锁。
- **watch 快照的 clone 必须 cheap**：
  - `ConnectionState` 必须通过 `Arc` 对外传播，clone 必须是 O(1)
  - 禁止在状态发布路径内做任何格式化与高频分配
- **span/log 字段不得引入高频分配**：`driver_kind/plugin_kind` 等常量必须为 `&'static str` 或 `Arc<str>` 缓存；禁止每次重连/每条消息拼接字符串。

**落地检查清单（每次迁移必须自检）**

- [ ] collect/write/process_data 热路径无 `String::from`/`format!`/`to_string`
- [ ] 重连循环内无 `Vec` 扩容/clone 大对象（只允许 clone `Arc`）
- [ ] `ConnectionState` 的 `last_failure`/`code` 等字段均为 `Arc`，并且只在失败发生点构造一次
- [ ] 指标 label 不包含高基数（endpoint/topic/device_id 等），且 label 值为预分配/静态值

### 9.5 主动 Reconnect 统一机制（必须统一，禁止各协议自造）

很多驱动/插件需要“**主动触发**重连”（例如：协议栈检测到不可恢复状态、心跳失败、server 强制重连、上游下发 reset 指令）。

**目标**：把“主动重连”变成 supervision 的一等公民事件，统一治理（状态/退避/预算/观测），并保持零额外开销。

**统一方案（Phase 1 必备）**

- SupervisorLoop 持有一个 **bounded control-plane 信号通道**（建议 `mpsc::Sender<ReconnectRequest>`，容量=1，`try_send`，可“覆盖旧请求/丢弃重复”）
- `Ctx`/`Handle` 暴露一个 `request_reconnect(reason)` 的轻量入口：
  - handle 侧只有一个 `Sender` 的 clone（cheap）
  - data-plane 在需要时 `try_send`，禁止 await（避免把热路径变成慢路径）
- SupervisorLoop 在 `Connected/Run` 阶段 `select!`：
  - `Session::run()` 正常退出 → 按 `RunOutcome` 决策
  - 收到 `ReconnectRequest` → 视为 **可重试断开**：
    - 清 handle
    - publish `Reconnecting`（可带 backoff=0 或策略最小值）
    - 进入统一 backoff/budget/retry 决策

**约束**

- 禁止 driver/plugin 自己实现 reconnect loop（仍受 9.1 约束）
- 主动重连也必须走 budget：预算耗尽进入 Failed，不允许无限自救

---

## 10. 落地计划（破坏式重构版，按“先建骨架再迁移”）

### Phase 0：对齐现状与目标（先把“重复点”清零）

> 这是你要求“避免遗漏细节”的关键阶段：先把“计划 vs 现状代码”对齐，才能防止落地时出现两套并行实现。

- **清点现存 supervision 实现点（必须全量列举）**
  - southward：所有 `ng-gateway-southward/*/src/supervisor.rs`
  - northward：所有 `ng-gateway-northward/*/src/supervisor.rs`
  - core：历史上的 `ChannelMonitor`（southward）与 `AppActor` monitor/flush 逻辑（现已迁移为 ObserverConsumer）
- **统一 Retry 的单一事实来源**
  - 明确 `ng-gateway-sdk/src/retry.rs` 是唯一实现
  - 文档删除/合并“再造一套 retry 模块”的计划点（见本次更新）
- **定义“最终状态模型”并冻结语义**
  - 引入 `Initializing` 与 Connect/Init/Run 失败阶段
  - 冻结 `budget exhausted` 的对外语义（必须可观测、可解释）

### Phase 1：SDK supervision 内核落地（以现有 retry 为基线）

- 新增 `ng-gateway-sdk::supervision` 模块：
  - `state.rs`：`ConnectionState/Phase/FailureReport`
  - `connector.rs`：`Connector/Session/Ctx/RunOutcome`
  - `retry.rs`：**复用并迁移**现有 `ng-gateway-sdk/src/retry.rs`（允许破坏式移动文件/调整 API），并扩展 stage-aware 语义
  - `handle.rs`：`HandleCell`
  - `subscription.rs`：对外状态订阅契约（`watch<Arc<ConnectionState>>`）与发布规范
  - `loop.rs`：`SupervisorLoop<C>`
  - `observer.rs`：Observer trait + Noop（先落地接口，不做 host 注入）
  - `wrapper_southward.rs`：`SupervisedDriver<T>`
  - `wrapper_northward.rs`：`SupervisedPlugin<T>`
  - `abi_runtime.rs`：**`cdylib` ABI runtime adapter**（`NG_RUNTIME` + `RuntimeAwareDriver/RuntimeAwarePlugin` 的最终形态）
  - `macros.rs`：重写并收敛 `ng_driver_factory!` / `ng_plugin_factory!`（必须生成 ABI adapter + supervised wrapper + model_convert 绑定）
  - `model_convert.rs`：`SouthwardModelConverter/NorthwardModelConverter` + 默认模板实现（闭环关键）

**验收标准**

- 单测：retry determinism（jitter=none）、budget 耗尽边界、状态机迁移合法性（含 Connect/Init/Run 分阶段）
- fake connector/session 组件测试：脚本驱动 connect/init/run 的各种结果，验证 state/backoff/failed
- fake session 在 `run()` 内触发 `request_reconnect(reason)`：必须走统一路径（清 handle → Reconnecting → backoff/budget → Connecting），且 budget/观测口径一致
- `cdylib` 形态集成测试（必须真实 dlopen）：
  - host 可加载/探测/创建 factory
  - `start/stop` 可用，且 supervisor loop 跑在 `NG_RUNTIME`（可用 thread_name 断言）
  - 日志桥接可用：插件侧 `tracing` 输出能进入 host ingest

### Phase 1.5：Observer 注入落地（对应 8.3，必须先做）

把 8.3 从“设计”变为“工程可用”：让 host 能为每个 channel/app 注入 observer，统一 metrics/logging。

**落地内容**

- 在 host（core）侧新增 `ObserverFactory`（或等价构建入口）：
  - 输入：`channel_id/app_id` + `driver_kind/plugin_kind`
  - 输出：`Box<dyn Observer>`（通常是 CompositeObserver）
- SDK wrapper（`SupervisedDriver/SupervisedPlugin`）支持从 host 注入 observer：
  - 默认仍为 Noop（便于测试/离线运行）
  - 生产环境由 core 注入 MetricsObserver/LoggingObserver
- 提供 `CompositeObserver`（可空，空即 Noop）
- **禁止**各协议/插件自行打 `supervisor_*` 指标（统一收口）

**验收标准**

- 任意一个被监督的组件，在状态迁移/失败时会触发 observer（且不影响 data-plane 热路径）
- 指标命名与 labels 满足 8.2，且无高基数 label

### Phase 1.6：统一 state enum（已完成）

> 这是“最佳实践必须做”的破坏式变更：不再用 `Failed(String)` 做状态载荷。

- 在 SDK 中引入统一 `ConnectionState`（含 `Initializing`、阶段化失败、budget snapshot）
- southward/northward 对外接口统一返回 `watch::Receiver<Arc<ConnectionState>>`
- core 的观测消费侧（ObserverConsumer）只消费统一 state（不再分两套枚举）
- UI/REST/WS 的状态展示统一映射（避免“南北两套状态解释不一致”）

**重要说明（迁移期临时兼容层，必须在后续删除）**

（历史）Phase 1.6 ~ Phase 2/3 迁移期间曾允许存在旧 enum/薄桥接/bridge task；当前已按 Phase 4.0 清零并删除。
- 以上内容 **只是为了分阶段迁移保证可编译/可运行**。最终验收前必须：
  - 删除旧 enum（及其所有 `pub use` / UI types）
  - 删除所有 legacy bridge 与其任务
  - 所有 supervisor 原生发布 `watch<Arc<ConnectionState>>`（且携带 attempt/backoff/failure/budget）
- **破坏式推进方法（强制执行）**
  - **先删除** SDK 内 legacy enum 的类型定义与所有 `pub use` 导出（包括 SDK 根 `lib.rs` 的 re-export）
  - **允许编译错误爆炸式暴露**：core/models/ui/docs 中所有残留引用会一次性显性化，便于统一收敛与对齐语义（比“边改边兼容”更可靠）
  - UI 同理：先从 `ng-gateway-ui/packages/types` 删除 legacy enum 的 TS 常量/类型，让 TS error 暴露所有使用点

**验收标准**

- 同一套 UI/告警规则同时适用于 southward 与 northward
- 任何失败都能明确告诉运维：失败阶段（Connect/Init/Run）+ kind（Retryable/Fatal/Stop）+ 是否预算耗尽

### Phase 2：Southward 迁移（全量，删掉各协议 supervisor.rs）

顺序建议：OPCUA → IEC104 → S7 → MC → 其余（modbus/dnp3/…）

#### Phase 2.0 迁移模板（每个协议必须严格按清单执行）

> 目的：把“协议差异”压缩到 `Connector/Session/Handle` 三件套，所有生命周期/退避/观测/句柄发布由 `SupervisorLoop` 统一完成。

- **A. 现状剖析（只读）**
  - 列出该协议当前 `supervisor.rs` 的状态迁移图（Connecting → Connected → ...）
  - 标注“Ready 的真实定义”（例如 OPCUA：subscription/monitored items 完成；IEC104：总召成功；S7：握手 + 读写通道可用）
  - 标注当前 retry reset 点（何时 `retry.reset()`，成功判定是什么）
  - 标注主动重连触发点（心跳失败/库内回调/错误码）
- **B. 定义 Handle（data-plane 快路径句柄）**
  - 目标：`Handle` 必须是 `Arc` 可持有、可跨 task 使用、可 lock-free 热读
  - 把现有各协议的 `ArcSwapOption<Session/Client>` 迁移为 SDK `HandleCell<Handle>`
  - 协议内不再持有全局 `ArcSwapOption`（避免“双重句柄发布点”）
- **C. 实现 `Connector::connect()`（只做建连，不做 Ready）**
  - 仅建立 TCP/TLS/Session/认证等“最小可运行连接”
  - 禁止在 connect 阶段做 subscribe/总召等后置动作
  - 为 connect 阶段定义错误分类（可重试/不可恢复/停止）
- **D. 实现 `Session::init()`（定义 Ready）**
  - 把订阅/总召/对时/预热等全部移入 init
  - init 成功后才允许发布 handle 并进入 `Connected(Ready)`
  - init 失败必须发布 `FailurePhase::Init`，并按 budget/backoff 统一决策
- **E. 实现 `Session::run()`（运行直到断开/取消/失败）**
  - 将现有 event loop 驱动逻辑迁移到 run
  - run 必须协作取消（select cancel token），退出时由 supervisor 统一清 handle
  - 将“连接丢失/库内重连通知”等映射到 `RunOutcome`
- **F. 主动重连（9.5）统一接入**
  - 从 handle 暴露 `request_reconnect(reason)`（`try_send`，禁止 await）
  - 在 run 阶段统一 select 处理 `ReconnectRequest`
- **G. 删除旧 supervisor.rs 并收敛观测**
  - 删除协议内 reconnect loop / backoff / 任何自建 state 广播逻辑
  - 协议内只保留：错误分类、必要的业务日志（低频）、以及 data-plane 逻辑
- **H. 测试与基准（必须补齐）**
  - 单测：connect/init/run 三阶段分别失败 → state 与 budget 行为一致
  - 单测：budget exhausted → 进入 `Failed(code=budget_exhausted)` 且不再重连
  - 压测：Connected 热路径 collect/write 无额外锁与分配（至少相对旧实现不退化）

#### Phase 2.1 每个协议迁移必须做（执行项）

- 把“连接成功后动作”移入 `Session::init()`（订阅/总召/对时等）
- data-plane 逻辑使用 wrapper 注入的 `Arc<Handle>`，热读无锁
- **实现 9.5 主动 Reconnect（Southward 侧必须落地）**：
  - `SupervisorLoop` 引入 bounded `ReconnectRequest` control-plane 通道
  - `Ctx/Handle` 提供 `request_reconnect(reason)`（`try_send`，禁止 await）
  - run 阶段 `select!` 统一处理主动重连（清 handle → Reconnecting → backoff/budget）
- **统一 connect_timeout/read_timeout/write_timeout 的语义与注入点**
  - southward 的 `ConnectionPolicy.connect_timeout_ms/read_timeout_ms/write_timeout_ms` 必须进入 `Connector/Session`（不再由各协议自己私有配置）
  - 任何 I/O 超时都必须能被归类为 `FailurePhase::*` 并被 observer 记录

**验收标准**

- core 的观测消费侧能一致看到 state 更新（Connecting/Initializing/Connected/...）
- “全局 INFO + 通道 DEBUG”时，协议与第三方库日志可按 channel 输出
- budget 耗尽进入 Failed 且不再重连

### Phase 3：Northward 迁移（全量，删掉各插件 supervisor.rs）

顺序建议：Kafka/Pulsar/MQTT/Thingsboard（任选一个先做模板）→ 全量推广

#### Phase 3.0 先解决“northward 的现实复杂度”（否则会迁移到一半卡死）

现状 northward 有两类典型形态：

- **单连接单循环**（最容易）：MQTT/ThingsBoard（通常一个 client + uplink/downlink 都在同一会话）
- **多子系统/多循环**（必须明确语义）：Kafka/Pulsar（producer 与 consumer 分离，且 consumer 有独立重试策略）

最终版最佳实践建议：

- `ConnectionState` 表达 **“对外可用性（Ready）”**

#### Phase 3.1 迁移要点（执行项）

- MQTT subscribe 放入 `Session::init()`，成功后才 Ready
- consumer loop / downlink loop 在 `Session::run()` 内统一管理，禁止无界 spawn
- **复用 9.5 主动 Reconnect（Northward 侧完成落地）**：
  - 任何“主动断开/主动刷新连接”只能调用 `request_reconnect`
  - 统一 budget 语义：预算耗尽进入 Failed
- **把 AppActor 的热路径连接检查从连接态订阅 clone 改成 atomic 快读**
  - 现状 `AppActor::send_data()` 与 worker loop 会读取 `plugin.subscribe_connection_state()` 的快照并维护本地 atomic，data-path 再去读 atomic（零分配、零 clone），将来统一改为：
    - monitor 任务更新一个 `AtomicBool connected`
    - data-path 只读 atomic（零分配、零 clone）

**验收标准**

- AppActor 订阅 state 能一致更新
- per-app 日志过滤成立（全局 INFO + app DEBUG）

### Phase 4：统一 Web/API/metrics 展示

#### Phase 4.0 清理迁移期兼容层（必须完成后才能认为“迁移完成”）

> 对应 6.3.1 的“迁移期兼容说明”：legacy 分支允许存在，但**必须清零并删除**，否则架构会永久双轨、成本与风险持续累积。

**执行项**

- **删除宏 legacy 分支（强制）**
  - 已删除 `ng_driver_factory! / ng_plugin_factory!` 中旧输入形态（例如 legacy `factory` 参数）及其生成逻辑（改为编译期硬失败）
  - 确保所有 driver/plugin 仓库都已迁移到 `component + model_convert` 最终形态
- **收口并移除旧 Factory trait 对外扩展点（强制）**
  - `DriverFactory/PluginFactory` 若仍保留，仅允许作为 **SDK 内部兼容适配层**（外部 crate 不允许继续依赖/实现）
  - 迁移完成后 **删除** 对外 re-export/公开文档示例与 legacy ABI 分支，避免新实现继续走旧路
- **删除所有迁移桥接代码（强制）**
  - 删除所有 legacy bridge 与其任务
  - 删除 legacy connection state enum（Rust + UI types）

**验收标准**

- 全仓不存在 legacy 宏输入用法（全量 grep 为 0）
- 全仓不存在 `create_*_factory` legacy 分支逻辑（仅保留最终版语义）
- `DriverFactory/PluginFactory` 不再作为外部扩展入口（文档与 API 口径一致）

#### Phase 4.1 状态与错误展示（UI/REST/WS）

- southward/northward 统一展示 `ConnectionState`
- UI 必须展示：
  - phase（Connecting/Initializing/Connected/Reconnecting/Failed/Disconnected）
  - attempt 与 backoff（便于判断是否“卡在退避/卡在初始化”）
  - last_failure（phase/kind/summary/code）
  - budget（是否耗尽/剩余提示）

#### Phase 4.1.1 UI 侧改动清单（尽量细：按文件/模块列出）

> 你要求“可以先删代码让错误暴露”：UI 这里也同理。先删旧类型/旧渲染分支，让 TS 报错把所有使用点一次性暴露出来，然后逐一对齐到新结构。

**A. 先删（让错误暴露）**

- `ng-gateway-ui/packages/types/src/channel.ts`
  - 删除 legacy connection state 常量与相关联合类型
  - 将 `ChannelInfo.connectionState` 从 `string`/旧枚举 **改为新结构化类型**（见 B）
- `ng-gateway-ui/packages/types/src/app.ts`
  - 删除 legacy connection state 常量与相关联合类型
  - 将 `AppInfo.connectionState` 改为新结构化类型

**B. 引入统一结构化类型（UI types 层）**

- 新增（建议）`ng-gateway-ui/packages/types/src/connection.ts`（或放在 `base.ts` 中，但推荐单独文件）
  - `ConnectionPhase = "Disconnected" | "Connecting" | "Initializing" | "Connected" | "Reconnecting" | "Failed"`
  - `FailurePhase = "Connect" | "Init" | "Run"`
  - `FailureKind = "Retryable" | "Fatal" | "Stop"`
  - `ConnectionState`（与后端对齐的 view model）：
    - `phase: ConnectionPhase`
    - `attempt: number`
    - `backoffMs?: number`
    - `lastFailure?: { phase: FailurePhase; kind: FailureKind; summary: string; code?: string }`
    - `budget?: { exhausted: boolean; remainingHint?: number }`
    - （可选）`sinceMs?: number` / `updatedAtMs?: number`（按后端最终 API 选择）
- 在 `ChannelInfo` / `AppInfo` 中统一：
  - `connectionState?: ConnectionState | null`（最终版建议不再允许 `string` 兜底；破坏式让错误暴露）

**C. UI 渲染与交互（表格/组件）**

- `ng-gateway-ui/apps/web-antd/src/adapter/vxe-table.ts`
  - 重写 `CellConnectionState`：
    - 从 `row.connectionState` 读取 `ConnectionState.phase` 而不是字符串
    - 新增 `Initializing` 的颜色/文案（例如 processing/info）
    - 建议加 Tooltip：展示 attempt/backoff/lastFailure.summary（运维可读）
    - 删除“默认把 unknown string lowerCase 映射翻译”的兜底逻辑（破坏式收敛）
- `ng-gateway-ui/apps/web-antd/src/views/southward/channel/modules/schemas/table-columns.ts`
  - `field: 'connectionState'` 可保持不变（只要后端字段名不变）
  - 若后端改名（例如 `connection`），此处同步修改字段名
- `ng-gateway-ui/apps/web-antd/src/views/northward/app/modules/schemas/table-columns.ts`
  - 同上

**D. i18n（新增/调整文案）**

- `ng-gateway-ui/packages/locales/src/langs/zh-CN/ui.json`
- `ng-gateway-ui/packages/locales/src/langs/en-US/ui.json`
  - `connectionState` 下新增：
    - `initializing`
  - 若加入 tooltip/结构化字段展示，新增：
    - `connectionState.attempt`
    - `connectionState.backoff`
    - `connectionState.lastFailure`
    - `connectionState.failurePhase.connect/init/run`
    - `connectionState.failureKind.retryable/fatal/stop`
    - `connectionState.budgetExhausted`（或等价）

**E. REST/WS 数据契约（UI 依赖后端）**

- 如果 channel/app 列表 API 当前返回 `connectionState: "Connected"`（字符串）
  - 最终版必须改为返回结构化 `connectionState: { phase, attempt, ... }`
  - UI types 与渲染按新结构对齐
  - **推进策略**：先改后端结构并删除旧字段/旧字符串，让前端直接报错，然后集中修复 UI

#### Phase 4.2 指标与告警（Prometheus）

- metrics 统一以 `supervisor_*` 命名汇总
- 告警规则建议（示例）：
  - `phase=Failed` 持续 X 分钟
  - `failure_total{kind="fatal"}` 突增
  - `budget_exhausted_total` 非 0（必须人工介入/配置修复）
  - `backoff_seconds` P95 长期过高（怀疑网络/端点不稳定）

---

### Phase X：切换到纯 C ABI 函数表（vtable）（长期路线，逐步替换 dyn trait FFI）

> 目标：把当前 `create_*_factory() -> *mut dyn DriverFactory/PluginFactory` 的 trait-object FFI，
> 逐步替换为 **纯 C ABI 的函数表（vtable）**：`extern "C" fn` + `repr(C)` 结构体 + 显式的对象句柄。
> 这样可以获得更稳定的 ABI、可控的内存/错误边界，并为未来 “跨语言插件/驱动” 打基础。

#### Phase X.0 设计约束（必须先定死，否则迁移会反复）

- **ABI 稳定**：所有导出结构体必须 `#[repr(C)]`，字段只增不减；用 `abi_version` + `struct_size` 做兼容协商
- **内存边界**：跨边界只传 `*mut c_void`/`*const c_void`（opaque handle）；由 plugin 提供 `drop_*` 回收
- **错误边界**：跨边界不传 Rust `Error` trait object；统一 `NgStatus` + 可选 `get_last_error()`（线程局部或对象内缓存）
- **并发边界**：host 不假设可重入；所有调用要么由 host 侧序列化，要么 vtable 显式声明线程安全能力（flags）
- **日志/观测**：继续使用现有 log sink bridge；vtable 仅保留“注册 sink / 调整 level”等纯 C 函数

#### Phase X.1 定义函数表（vtable）与对象模型（仅新增，不破坏现有 ABI）

- **Driver 侧**
  - 定义 `NgDriverVTableV1`（示意）：
    - `create(ctx_json_ptr, ctx_len, out_handle)` / `drop(handle)`
    - `start(handle)` / `stop(handle)`
    - `subscribe_state(handle) -> NgWatchHandleV1`（或 `get_state_snapshot(handle, out_json)`）
    - `collect(handle, items_ptr, items_len, out_data_vec)` / `execute(...)` / `write(...)`
  - 定义与之配套的 `NgDriverHandleV1`（opaque）与 `NgWatchHandleV1`（opaque）
- **Plugin 侧**
  - 定义 `NgPluginVTableV1`：
    - `create(ctx_json_ptr, ctx_len, out_handle)` / `drop(handle)`
    - `start/stop/subscribe_state/process_data/ping`

> 注意：V1 阶段可以先把 “模型转换 / metadata / schema” 留在现有 `ng_*_metadata_json_ptr` 导出中不动；
> vtable 先覆盖运行时行为（start/stop/data-plane/state）。

#### Phase X.2 双栈运行（迁移期，host 同时支持旧 ABI 与 vtable ABI）

- host loader 探测顺序：
  - 先探测 `ng_*_get_vtable_v1()`（新符号）
  - 不存在则回退到 `create_*_factory()`（旧符号）
- SDK 宏新增导出：
  - `ng_driver_get_vtable_v1()` / `ng_plugin_get_vtable_v1()`
  - 返回 `static Ng*VTableV1` 指针（只读）
- 迁移策略：
  - 先在 SDK 内实现 “vtable 适配层” 调用现有 Rust 代码（Supervised* + RuntimeAware*）
  - 等全量插件迁移后，再逐步删除旧 ABI 分支

#### Phase X.3 清理与删除（强制）

- 删除旧 ABI 导出入口（当且仅当全仓迁移完成）：
  - 删除 `create_*_factory()` 旧语义分支
  - 删除 `DriverFactory/PluginFactory` 对外扩展入口（仅保留 SDK 内部必要的 compat，最终可删）
- 验收标准：
  - host 侧不再依赖 trait-object FFI
  - 所有 driver/plugin 仅通过 vtable ABI 被加载与调用
  - 发生 panic/错误时不会跨边界展开（error 以 `NgStatus` 返回）

---

## 11. 最终验收（必须全部满足）

- **一致性**
  - 全部连接型 southward/northward 都走统一 `Connecting/Initializing/Connected/Reconnecting/Failed/Disconnected`
  - connect/init/run 三阶段失败可清晰诊断（FailureReport）
- **性能**
  - 热读句柄无锁（ArcSwap）
  - supervision 内核静态分发，无 `dyn Connector/Session`
  - 退避期间无任务膨胀
- **可运维**
  - 指标覆盖成功/失败/退避/预算耗尽
  - 日志按 channel/app 过滤正确，第三方库日志也能归属

---

## 12. 附：实现者指南（把协议逻辑放哪里）

- **connect()**：只做“建立连接/认证/握手”，不要做订阅/总召（放 init）
- **init()**：必须完成“Ready 的定义”（OPCUA 订阅、IEC104 总召、MQTT 订阅）
- **run()**：驱动 event loop，负责维持连接直到断开；退出原因用 `RunOutcome` 表达
