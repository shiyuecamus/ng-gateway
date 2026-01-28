# NG Gateway：统一 Supervision（最终版）——极致性能的 Driver/Plugin 生命周期治理（含伪代码与落地计划）

> 本文是最终版设计：允许破坏式重构，不考虑最小变更/向前兼容。  
> 目标：把 southward/northward 所有“连接型组件”的生命周期治理（connect/init/run/reconnect/failed/stop）做成 **SDK 统一托管**，让 driver/plugin 作者只实现“不可统一的协议与业务逻辑”，并在热路径保持 **零虚调用、零额外分配、无锁热读句柄**。

---

## 0. 核心结论（先把争议点钉死）

### 0.1 `SupervisorLoop` 不应该由每个 driver/plugin 手动启动

如果 driver/plugin 仍需自己拼装并启动（span、watch、ArcSwap、retry、observer、spawn），抽象只统一了循环内部，没统一“接入闭环”，价值被稀释。  
**最终版要求：SDK 统一托管启动与闭环 wiring**，driver/plugin 作者只实现 traits（泛型/associated types）与业务方法。

### 0.2 `ProtocolSupervisor` 不跨 ABI 暴露：Factory 仍返回 `dyn Driver/Plugin`，但对象是 SDK wrapper

你们现有架构中，跨 ABI 动态加载边界稳定存在：

- `DriverFactory -> Box<dyn Driver>`
- `PluginFactory -> Box<dyn Plugin>`

而 “极致性能 + associated types” 的 supervision 内核必须是：

- `SupervisorLoop<P>` 对 `P` 静态分发（monomorphization）

因此最终版采用：**Factory 仍返回 `Box<dyn Driver/Plugin>`，但实际返回的是 `SupervisedDriver<T>` / `SupervisedPlugin<T>`（SDK 内置 wrapper）**，wrapper 内部持有 `T` 与 `SupervisorLoop<T::Protocol>`。

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
  - 状态广播用 `watch`，payload 小且结构化
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
  - `subscribe_connection_state`：提供连接态订阅（watch receiver）
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
  - `subscribe_connection_state`：连接态订阅
  - `process_data`：把 southward 数据发往平台（publish/uplink）
  - `events_tx`（或等价机制）：把平台下行业务事件（RPC/Command/WritePoint）发回 core
- **生命周期**：与 app 实例同生共死，内部跨多次重连产生多次 Session。

### 1.3.3 `SupervisedDriver<T>` / `SupervisedPlugin<T>`（SDK wrapper，最终版的“本体”）

- **是什么**：SDK 内置的统一托管层，实现 `Driver/Plugin`，并持有用户实现 `T` 与 supervision 内核。
- **职责（必须统一的地方都在这里）**：
  - 创建并绑定 span（`channel_id` / `app_id` 等）
  - 创建 `watch::Sender<ConnectionState>` 并对外暴露 receiver
  - 创建 `HandleCell`（ArcSwap）并保证：**Connected(Ready) 才发布，断连立刻清空**
  - 注入 `RetryPolicy/RetryController/Budget`
  - 注入 Observer（metrics/tracing）
  - 启动并托管 SupervisorLoop（spawn + join + cancel）
  - 在 data-plane 入口统一热读 handle，并统一 NotConnected 语义
- **意义**：实现者不再手写“启动 supervisor”的样板；所有行为一致且可被 CI 强制。

### 1.3.4 `SouthwardComponent` / `NorthwardComponent`（实现者写的“业务实现体”）

- **是什么**：协议/业务作者实现的具体类型（你“写的 driver/plugin”在最终版里就是它们）。
- **职责**：
  - 提供 `build_connector()`（构建连接器，捕获 cfg/依赖，但不做 I/O）
  - 实现 data-plane 方法（collect/write/execute/process_data 等），只关注业务
  - 绝不负责：状态机、退避预算、句柄发布、span/metrics wiring（全部由 wrapper 托管）
- **并发语义**：
  - 由 wrapper 决定是否并发调用 data-plane（例如通过 in-flight semaphore 控制）。
  - 实现者必须假设 data-plane 可并发（除非 wrapper 明确串行），内部用协议层串行化/连接池等保证正确性。

### 1.3.5 `DriverFactory` / `PluginFactory`（跨 ABI 的构造入口）

- **是什么**：动态加载边界的“构造器”，负责从 init context 创建实例。
- **最终版职责**：
  - `create_driver/create_plugin`：构造实现者类型 `T`，并立即用 SDK wrapper 包装：返回 `Box<dyn Driver/Plugin>`
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

- **是什么**：在一个任务里实现统一状态机：Connecting → Initializing → Connected → (Reconnecting)* → Failed/Disconnected。
- **职责**：
  - 协作取消（connect/init/run/backoff 都可取消）
  - 退避/预算（RetryController）
  - 发布状态（watch::Sender<ConnectionState>）
  - 发布/清理句柄（HandleCell）
  - 调用 observer（指标/日志/事件）
- **强约束**：
  - 所有 spawn 必须继承 span（`.instrument(span)`）

### 1.3.10 `Observer`（观测扩展点）

- **是什么**：把 metrics/log/event 从主逻辑里剥离出来的可插拔接口。
- **职责**：
  - 监听 state change / failure / backoff
  - 以低开销方式记录指标与结构化日志

### 1.3.11 `ConnectionState` vs `SouthwardConnectionState/NorthwardConnectionState`

- **最终版建议**：统一到一个 `ConnectionState`（含 `Initializing`），southward/northward 的旧 enum 作为视图/桥接层存在或被替换。
- **为什么要统一**：core 的 monitor/web/metrics 需要同一语义来展示与告警；否则每次新增阶段都要改两套体系。

---

## 2. 统一状态模型（强语义，统一展示）

> 现状：southward 有 `SouthwardConnectionState`，northward 有 `NorthwardConnectionState`。最终版建议统一为 SDK 单一 `ConnectionState`，并在旧 enum 上做薄桥接（或直接替换）。

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
pub enum FailureKind { Retryable, Fatal }

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
    pub since: std::time::Instant,
    pub backoff: Option<std::time::Duration>,
    pub last_failure: Option<Arc<FailureReport>>,
    pub budget: RetryBudgetSnapshot,
}
```

> 关键：`Initializing` 是必须状态，否则“连接成功但订阅失败/总召失败”的阶段无法一致治理与展示。

---

## 3. 统一 supervision 内核：Connector + Session（把“后置动作”纳入 Session Init）

最终版的抽象不再是 “connect_once + run_event_loop” 两个裸函数，而是 **连接返回一个 Session 对象**：

- Session 内部包含 event loop 资源
- Session 暴露 `handle()`（用于发布与 data-plane）
- Session 提供 `init()`（总召/订阅/后置握手动作）
- Session 提供 `run()`（驱动连接生命周期直到断开）

这样可以把 init 纳入统一管控，同时避免“connect_once 返回 (Handle, EventLoop) 但 init 需要两者协作”的 awkward API。

### 3.1 失败分类（必须由实现者显式提供）

Supervisor 不做“字符串猜测”。实现者提供分类，分别针对 Connect/Init/Run：

```rust
pub enum FailureClass { Retryable, Fatal, Stop }
pub enum StageHint { Connect, Init, Run }
```

### 3.2 Trait 设计（泛型 + associated types，零成本）

```rust
use tokio_util::sync::CancellationToken;
use tracing::Span;

pub struct Ctx<'a> {
    pub cancel: CancellationToken,
    pub span: Span,                 // 已包含 channel_id/app_id 等字段
    pub attempt: u64,
    pub now: std::time::Instant,
    pub observer: &'a dyn Observer, // 默认 Noop
}

#[async_trait::async_trait]
pub trait Session: Send + 'static {
    type Handle: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    /// 句柄用于 data-plane，必须可被 Arc 包装并热读。
    fn handle(&self) -> &Self::Handle;

    /// Session Init：总召/订阅/恢复状态/预热等。成功后才允许进入 Connected(Ready) 并发布 handle。
    async fn init(&mut self, ctx: Ctx<'_>) -> Result<(), Self::Error>;

    /// Run：驱动 event loop，直到断开/取消/失败，返回结构化结果（或 error + 分类）。
    async fn run(self, ctx: Ctx<'_>) -> Result<RunOutcome, Self::Error>;
}

#[derive(Clone, Debug)]
pub enum RunOutcome {
    GracefulStop,          // cancel/stop
    DisconnectedRetryable, // 正常断开但应重连（rolling restart、EOF 等）
    RetryableFailure,      // run 中可重试失败
    FatalFailure,          // run 中不可恢复失败
}

#[async_trait::async_trait]
pub trait Connector: Send + Sync + 'static {
    type Session: Session;

    /// Connect：建立底层连接/认证/握手，返回 Session（尚未 init）。
    async fn connect(&self, ctx: Ctx<'_>) -> Result<Self::Session, <Self::Session as Session>::Error>;

    /// 明确分类：Connect/Init/Run 的错误语义可能不同。
    fn classify_error(&self, stage: StageHint, err: &<Self::Session as Session>::Error) -> FailureClass;

    /// 错误摘要（UI/告警友好，避免大对象 clone）。
    fn error_summary(&self, err: &<Self::Session as Session>::Error) -> Arc<str> {
        Arc::<str>::from(err.to_string())
    }
}
```

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
    state_tx: watch::Sender<ConnectionState>,
    handle_cell: HandleCell<<C::Session as Session>::Handle>,
    observer: Box<dyn Observer>,
}

impl<C: Connector> SupervisorLoop<C> {
  pub async fn run(mut self) -> anyhow::Result<()> {
    let mut attempt = 0u64;
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
          let class = self.connector.classify_error(StageHint::Connect, &e);
          let report = self.mk_report(FailurePhase::Connect, class, &e);
          if let Some(next) = self.on_failure(class, attempt, report.clone()).await? { continue; }
          return Err(anyhow::anyhow!("supervisor failed (connect)"));
        }
      };

      // --- INIT ---
      self.publish(Phase::Initializing, attempt, None, None);
      if let Err(e) = sess.init(self.ctx(attempt)).instrument(self.span.clone()).await {
        let class = self.connector.classify_error(StageHint::Init, &e);
        let report = self.mk_report(FailurePhase::Init, class, &e);
        self.handle_cell.store(None);
        if let Some(next) = self.on_failure(class, attempt, report.clone()).await? { continue; }
        return Err(anyhow::anyhow!("supervisor failed (init)"));
      }

      // 只有 init 成功后才发布 handle 并进入 Connected(Ready)
      let h = Arc::new(sess.handle().to_owned_handle()); // 实际实现：Handle 通常放在 Session 内，可直接 Arc::new(clone/Arc)
      self.handle_cell.store(Some(h));
      self.retry.on_success(std::time::Instant::now());
      self.publish(Phase::Connected, attempt, None, None);

      // --- RUN ---
      let outcome = match sess.run(self.ctx(attempt)).instrument(self.span.clone()).await {
        Ok(o) => o,
        Err(e) => {
          let class = self.connector.classify_error(StageHint::Run, &e);
          match class {
            FailureClass::Stop => RunOutcome::GracefulStop,
            FailureClass::Fatal => RunOutcome::FatalFailure,
            FailureClass::Retryable => RunOutcome::RetryableFailure,
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
          if let Some(next) = self.on_failure(FailureClass::Retryable, attempt, report).await? { continue; }
          return Err(anyhow::anyhow!("supervisor failed (budget exhausted)"));
        }
      }
    }
  }
}
```

> 注：上面出现 `to_owned_handle()` 只是表达“句柄需可被 Arc 化并供热读”。最终实现可要求 `Handle: Clone` 或直接让 Session 内部持有 `Arc<Handle>` 并返回引用到 `Arc`（更优）。

### 4.3 强制 Span 传播

- SupervisorLoop 内部所有 spawn 必须由 SDK 提供的 `spawn_in_span()` 完成，禁止 driver/plugin 自己 spawn supervisor。
- `connect/init/run` 的 future 也统一 `.instrument(span)`，确保第三方库日志具备 id 字段（channel_id/app_id）。

---

## 5. 终极接入：SDK Wrapper 托管一切（Driver/Plugin 作者只写 traits）

这一节回答你最关心的：**DriverFactory/PluginFactory 应该如何设计**，才能让接入零样板且保持极致性能。

### 5.1 Southward：`SupervisedDriver<T>`（SDK wrapper 实现 `dyn Driver`）

#### 5.1.1 驱动作者要实现的最小接口（最终版）

驱动作者不再直接实现 `ng-gateway-sdk::Driver`（object-safe），而是实现：

- `SouthwardComponent`：提供 `Connector`（协议生命周期）+ data-plane（collect/write/execute/health）方法

```rust
pub trait SouthwardComponent: Send + Sync + 'static {
  type Connector: Connector;

  // identity（用于 span/metrics labels）
  fn channel_id(&self) -> i32;
  fn driver_kind(&self) -> &'static str;

  // build connector（捕获 cfg/依赖，不做 I/O）
  fn build_connector(&self) -> Self::Connector;

  // data-plane：SDK 负责提供 handle（已是 Arc<Handle>），你只做业务
  async fn collect(&self, handle: Arc<<<Self::Connector as Connector>::Session as Session>::Handle>, items: &[CollectItem])
    -> DriverResult<Vec<NorthwardData>>;

  async fn write_point(&self, handle: Arc<<<Self::Connector as Connector>::Session as Session>::Handle>, device: Arc<dyn RuntimeDevice>, point: Arc<dyn RuntimePoint>, value: &NGValue, timeout_ms: Option<u64>)
    -> DriverResult<WriteResult>;

  async fn execute_action(&self, handle: Arc<<<Self::Connector as Connector>::Session as Session>::Handle>, device: Arc<dyn RuntimeDevice>, action: Arc<dyn RuntimeAction>, parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>)
    -> DriverResult<ExecuteResult>;

  async fn health_check(&self) -> DriverResult<DriverHealth>;
}
```

#### 5.1.2 SDK wrapper 的职责（完全统一）

`SupervisedDriver<T>` 实现 `ng-gateway-sdk::Driver`，并统一：

- `start()`：创建 span、state watch、handle cell、retry policy、observer，并启动 SupervisorLoop（SDK spawn）
- `stop()`：cancel + join（或 best-effort）
- `subscribe_connection_state()`：返回统一 state（必要时桥接到 `SouthwardConnectionState`）
- `collect/write/execute`：统一热读 handle；NotConnected 统一错误；并发护栏可在 wrapper 内统一（in-flight semaphore）

> 关键：driver 作者再也不写 supervisor 启动；也不会在每个协议里重复 watch/ArcSwap/retry/span 细节。

### 5.2 Northward：`SupervisedPlugin<T>`（SDK wrapper 实现 `dyn Plugin`）

同理，plugin 作者实现：

```rust
pub trait NorthwardComponent: Send + Sync + 'static {
  type Connector: Connector;

  fn app_id(&self) -> i32;
  fn plugin_kind(&self) -> &'static str;
  fn build_connector(&self) -> Self::Connector;

  // data-plane：对外发送（publish）等
  async fn process_data(&self, handle: Arc<<<Self::Connector as Connector>::Session as Session>::Handle>, data: Arc<NorthwardData>)
    -> NorthwardResult<()>;

  // 业务事件：例如 platform->gateway 的 command/rpc/writepoint
  fn events_tx(&self) -> &tokio::sync::mpsc::Sender<NorthwardEvent>;

  async fn health_check(&self) -> NorthwardResult<Duration>;
}
```

SDK wrapper `SupervisedPlugin<T>` 统一托管 supervisor，`Plugin::start/stop/subscribe_connection_state/process_data` 都标准化。

### 5.3 Session Init 如何覆盖 OPCUA/IEC104/MQTT 的“连接后动作”

最终版对实现者的要求非常清晰：

- 把“总召/订阅”等动作写在 `Session::init()`
- 只有 init 成功后，Supervisor 才：
  - 发布 handle（ArcSwap）
  - 将 state 置为 Connected（Ready）

示例映射：

- **OPCUA client**
  - `connect()`：建立 session + 创建底层 eventloop
  - `init()`：create subscription + create monitored items +（可选）browse/read 预热
  - `run()`：驱动 publish loop / reconnect detection
- **IEC104**
  - `connect()`：TCP connect + start data transfer handshake
  - `init()`：发送总召 GI / 对时 / 初始化遥测映射
  - `run()`：驱动接收/处理 APCI/ASDU，直到断开
- **MQTT**
  - `connect()`：CONNECT/CONNACK 完成，得到 client
  - `init()`：SUBSCRIBE topics（必须在 Ready 前完成，否则上层误判 connected）
  - `run()`：poll event loop，处理消息与 keepalive

### 5.4 Server-mode（northward OPCUA Server）也按同一模型实现

- `connect()`：bind/listen/start server
- `init()`：加载 address space / 注册 handlers / 恢复持久化状态（如有）
- `run()`：accept/serve loop

`Connected` 的语义解释为 **Serving/Running**（不是“连上远端”）。

---

## 6. Factory 设计（最终版）：Factory 只负责“构建组件实现”，SDK 宏负责 wrap

### 6.1 新原则

- Factory 保持 object-safe，用于跨 ABI 动态加载
- Factory 不暴露 `Connector/Session`（不跨 ABI 暴露 associated types）
- Factory 返回的 `dyn Driver/Plugin` 由 SDK 自动 wrap 成 supervised wrapper

### 6.2 SouthwardFactory（最终版）

建议把原 `DriverFactory` 拆成两层概念（最终版推荐）：

- `DriverFactory`：跨 ABI，负责创建 “driver component 实现” 与 runtime model 转换
- `SupervisedDriver::from_component(...)`：SDK 内统一 wrap

伪代码：

```rust
pub trait DriverFactory: DowncastSync + Send + Sync {
  fn create_component(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn SouthwardComponentDyn>>;
  fn convert_runtime_channel(&self, channel: ChannelModel) -> DriverResult<Arc<dyn RuntimeChannel>>;
  fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>>;
  fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>>;
  fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>>;
}
```

这里出现 `SouthwardComponentDyn` 是一个 **object-safe shim**，它内部持有真正的 `T: SouthwardComponent`，但对外仍可由 wrapper 使用并保持静态分发？  
最终版不建议走这个方向（会引入虚调用），更优方案是：

- **Factory 直接返回 `Box<dyn Driver>`，但 Driver 的具体类型就是 `SupervisedDriver<T>`**  
- `T` 是具体类型，存在于 driver crate 内；Factory 的构造函数里写死 `T`，因此不会丢失单态化

也就是说：保持你们现有模式（`create_driver -> Box<dyn Driver>`），但通过宏统一 wrap：

```rust
fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>> {
  let impl_ = MyDriverImpl::new(ctx)?;
  Ok(Box::new(SupervisedDriver::new(impl_)))
}
```

为了彻底消灭样板，SDK 提供宏：

- `ng_supervised_driver_factory!(factory = MyFactory, driver_impl = MyDriverImpl, ...)`

宏负责：

- 生成 RuntimeAwareFactory（你们已经有）
- 在 `create_driver()` 内自动 `SupervisedDriver::new(MyDriverImpl::new(ctx)?)`

### 6.3 NorthwardFactory（最终版）

同理：

```rust
fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>> {
  let impl_ = MyPluginImpl::new(ctx)?;
  Ok(Box::new(SupervisedPlugin::new(impl_)))
}
```

并提供宏：

- `ng_supervised_plugin_factory!(factory = MyFactory, plugin_impl = MyPluginImpl, ...)`

---

## 7. Retry/Budget（统一、可配置、默认最优）

最终版要求：SDK 提供两套默认策略（可覆盖）：

- `RetryPolicy::default_for_southward()`
- `RetryPolicy::default_for_northward()`

原因：

- southward 往往是现场设备/工业网，短抖动多，适合更快探测与较长预算
- northward 往往是云端平台/消息系统，抖动与限流语义不同，适合更保守 backoff

预算模型建议：Token Bucket（可平滑恢复），并强制 jitter（避免集群同步重连）。

---

## 8. Observer/指标/日志（统一命名与低开销）

### 8.1 Observer（统一扩展点）

SupervisorLoop 内只发事件，不直接写 prometheus；由 Observer 负责：

- `on_state_change(ConnectionState)`
- `on_failure(FailureReport)`
- `on_backoff(Duration, budget)`

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

---

## 10. 落地计划（破坏式重构版，按“先建骨架再迁移”）

### Phase 1：SDK 内核落地（不迁移协议）

- 新增 `ng-gateway-sdk::supervision` 模块：
  - `state.rs`：`ConnectionState/Phase/FailureReport`
  - `connector.rs`：`Connector/Session/Ctx/RunOutcome`
  - `retry.rs`：`RetryPolicy/RetryController/Budget`
  - `handle.rs`：`HandleCell`
  - `loop.rs`：`SupervisorLoop<C>`
  - `observer.rs`：Observer + 默认实现（metrics/tracing）
  - `wrapper_southward.rs`：`SupervisedDriver<T>`
  - `wrapper_northward.rs`：`SupervisedPlugin<T>`
  - `macros.rs`：`ng_supervised_driver_factory!` / `ng_supervised_plugin_factory!`

**验收标准**

- 单测：retry determinism（jitter=none）、budget 耗尽边界、状态机迁移合法性
- fake connector/session 组件测试：脚本驱动 connect/init/run 的各种结果，验证 state/backoff/failed

### Phase 2：Southward 迁移（全量，删掉各协议 supervisor.rs）

顺序建议：OPCUA → IEC104 → S7 → MC → 其余（modbus/dnp3/…）

每个协议迁移必须做：

- 把“连接成功后动作”移入 `Session::init()`（订阅/总召/对时等）
- data-plane 逻辑使用 wrapper 注入的 `Arc<Handle>`，热读无锁

**验收标准**

- core 的 `ChannelMonitor` 看到的 state 可一致更新（Connecting/Initializing/Connected/...）
- “全局 INFO + 通道 DEBUG”时，协议与第三方库日志可按 channel 输出
- budget 耗尽进入 Failed 且不再重连

### Phase 3：Northward 迁移（全量，删掉各插件 supervisor.rs）

顺序建议：Kafka/Pulsar/MQTT/Thingsboard（任选一个先做模板）→ 全量推广

迁移要点：

- MQTT subscribe 放入 `Session::init()`，成功后才 Ready
- consumer loop / downlink loop 在 `Session::run()` 内统一管理，禁止无界 spawn

**验收标准**

- AppActor 订阅 state 能一致更新
- per-app 日志过滤成立（全局 INFO + app DEBUG）

### Phase 4：统一 Web/API/metrics 展示

- southward/northward 的连接状态统一映射到同一 `ConnectionState`（或可兼容旧结构但展示一致）
- metrics 统一以 `supervisor_*` 命名汇总

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

