# Modbus 驱动：同时兼容 TCP / RTU 的并行采集「最佳实践」方案（ng-gateway）

> 目标：在 **不破坏 RTU 总线约束** 的前提下，为 Modbus TCP 提供可控的并行能力（多连接/连接池），提高吞吐与周期完成率，同时保持可观测性、可回滚性与工程可维护性。

---

## 0. TL;DR（可直接拿去落地的结论）

- **RTU**：同一串口/总线 **必须单飞**（inflight=1）。所谓“并行”只能通过 **多串口/多总线** 实现。
- **TCP**：推荐采用 **多连接 + 每连接单飞（worker 模型）**。连接数 `pool_size=2~4` 通常就能显著提升吞吐；过大反而可能触发设备限流/断连。
- **批量参数**：
  - `maxBatch`：对 0x03/0x04 寄存器读建议 **≤125**（项目 UI 也做了约束）。实践推荐 `120` 起步。
  - `maxGap`：你们当前算法下，连续地址要合并需要 `maxGap>=1`；推荐默认 **1**，避免把“洞”读进来导致整批异常。
- **写入**：强烈建议对同一设备（slave/unit）**串行化写操作**，读可以并行、写要守序。
- **回滚策略**：所有并行能力都应由配置开关控制；把 `pool_size` 设回 `1` 即可恢复现有串行语义。

---

## 1. 现状剖析（基于当前代码的真实行为）

### 1.1 读批次合并规则（maxGap / maxBatch 的真实含义）

当前 planner 的合并逻辑是“按 function code 分组、按 address 排序、在 `max_gap`/`max_batch` 约束下合并为范围读”：

- **`maxBatch`**：限制单个范围读的 **地址跨度**（`batch_end - batch_start + 1`），并最终作为 `read_* (start, qty)` 的 `qty`。
- **`maxGap`**：限制 “下一点起始地址” 与 “当前批末尾地址” 的距离：`next_start - batch_end <= max_gap`。
  - 重要：对 `quantity=1` 的连续地址点（0,1,2...），由于 `batch_end` 是包含末尾地址，所以连续时差值为 1，因此要合并连续地址 **必须** `maxGap>=1`。

### 1.2 Session / 执行模型

当前 Modbus 驱动使用 `SessionEntry` 保存一个 `ArcSwapOption<Mutex<Context>>`，Supervisor 负责维持 **单连接** 并在错误/超时后触发重连。

采集阶段（`collect_with_slave`）对 planner 产出的 batches 采用 `for batch in batches { ... await }` **串行**执行。每个 batch 在执行时会：

- 先 `ctx.lock().await` 获取 Context 的独占访问
- `guard.set_slave(slave)` 设置当前从站（unit id）
- 调用 `read_*` 执行请求

这意味着：

- 即使是 Modbus TCP，当前也是“单连接单飞 + 批次串行”的吞吐上限。
- `set_slave` 使得同一 `Context` **不能并发服务多个 slave**（否则会互相覆盖 unit id）；因此并行的基本单位必须是“多个 Context（多连接）”。

---

## 2. 兼容 TCP / RTU 的不变量与硬约束（必须写死）

### 2.1 RTU（串口/总线）约束

- **同一串口是物理共享介质**：半双工、需要静默间隔、以及从站响应窗口等约束。
- 在同一串口上并发发送请求没有意义且极易造成帧冲突/超时/异常风暴。
- 因此：**RTU 的 inflight 必须为 1**。吞吐的提升来自更好的 batching、合理超时与更稳定的重试/退避，以及“多串口扩展”。

### 2.2 TCP 设备侧约束

- 不同厂商设备对并发连接/并发请求支持差异极大：
  - 有些只允许 1 条连接；多连会被踢下线
  - 有些允许多连，但内部仍串行处理
  - 有些支持真正并行（更适合 `pool_size>1`）
- 因此：并行必须**可配置**、并且默认保守、具备退避与熔断。

### 2.3 Modbus 协议常见硬限制（尤其 0x03/0x04）

对 0x03/0x04（寄存器读）：

- 常见最大 `quantity` 为 **125 registers**。
- 将 `maxBatch` 设为 500/1000 的风险是：设备返回异常码（Illegal Data Value）、不响应导致超时、或库直接拒绝。

项目 UI 元数据也体现了这一约束（`maxBatch <= 125`）。

---

## 3. 最佳实践架构：统一的 “SessionPool + Worker 单飞” 模型（推荐）

> 这是同时兼容 TCP/RTU、并且工程上最稳的方案：**每条连接（或 RTU attach 后的 Context）一个 worker 串行执行**，靠 “多个 worker” 实现 TCP 并行。

### 3.1 核心理念

- **并行的单位 = Context 实例**（TCP: 不同 socket；RTU: 同一串口只能 1 个 Context）
- **每个 Context 内只允许单飞**（避免 pipeline/transaction 复杂度）
- **通过调度层控制并发度与顺序约束**

### 3.2 组件拆分（建议模块）

#### A) `SessionWorker`

每个 worker 负责：

- 独占一个 `Context` 的生命周期（连接、健康、断开、重连）
- 串行处理从队列来的操作（read/write）
- 维护该 worker 的指标：请求数、成功数、失败数、平均 RTT、队列深度

队列建议：

- `tokio::sync::mpsc::Receiver<ModbusOp>`：每个 worker 一个 receiver
- backpressure：队列容量要有界，避免高压场景无限堆积

#### B) `SessionPool`

统一抽象：

- `pool_size`：TCP 下 N（2~4），RTU 下强制 1
- 负责 worker 的创建与运行
- 提供一个 `dispatch(op) -> Future<Result<T>>` 的接口（内部选择某个 worker）

选择策略（从简单到进阶）：

- Round-robin（实现简单，足够好）
- Least-queue / Least-inflight（更均衡，但需要额外共享状态）
- Consistent-hash by slave_id（便于写入守序与缓存 locality）

#### C) `Scheduler`（调度层）

调度层接收 planner 输出的 batches，并决定：

- **读请求**：允许并行，受 `max_inflight`（或 `pool_size`）限制
- **写请求**：对同一 slave 必须串行（见 3.4）

推荐实现方式：

- 读：将 batches 映射为 ops，使用 `FuturesUnordered` 或 `join_all` + `Semaphore` 控制并发
- 写：增加 per-slave 的互斥（或固定路由到同一 worker）

### 3.3 为什么 worker 模型比 “共享 Context + Mutex 并行” 更好

共享 `Arc<Mutex<Context>>` 的并行方案表面可行，但会退化为：

- 所有并发最终在 `ctx.lock()` 处串行排队（并发是假象）
- `set_slave` 会造成并发不安全（必须靠更粗的锁把它串行化）
- 错误隔离差：一次卡住/超时会拖慢整个队列

worker 模型的优势：

- 每连接单飞，天然正确
- 多连接带来真实并行（TCP）
- 更好的 backpressure（每 worker 队列有界）
- 连接级错误隔离（坏一个不拖全部）

---

## 3.4 写入的最佳实践：按 slave 串行化（强约束）

### 规则

- **同一 slave 的写必须顺序执行**（尤其有“先写配置再读回验证”的场景）。
- 不同 slave 之间的写在 TCP 下可以并行（取决于设备是否同一物理机/同一 unit id）。

### 两种实现方式（推荐 A）

#### A) “按 slave 固定路由到某个 worker”（推荐）

- `worker_index = hash(slave_id) % pool_size`
- 该 slave 的读/写都走固定 worker

优点：实现简单、顺序天然成立、缓存/连接 locality 好。  
缺点：当某个 slave 特别重时可能不均衡（可通过更好的 hash/映射或单独配置解决）。

#### B) “写入独占锁（per-slave mutex）”

- 写入前 `lock(slave_id)`，写完释放
- 读不加锁、可并行

优点：更均衡、读不受影响。  
缺点：需要维护锁表（HashMap）、并处理生命周期与清理。

---

## 4. 配置设计（同时兼容 TCP/RTU）

建议把并行相关配置显式化（示例字段名，按你们现有 config 风格 camelCase）：

### 4.1 通用（TCP/RTU 都有意义）

- `maxGap: u16`
- `maxBatch: u16`（寄存器读建议 ≤125）
- `maxInflight: u16`：调度层最大并发请求数（建议默认等于 pool_size；RTU 下强制 1）
- `queueCapacity: u16`：每 worker 队列容量（默认 64 或 128）

### 4.2 TCP 专用

- `tcpPoolSize: u16`：连接池大小（默认 2；上限建议 8）
- `tcpConnectTimeoutMs / readTimeoutMs`：与现有 connection_policy 对齐即可

### 4.3 RTU 专用（并行仍为 1，但可提升稳定性）

- `rtuInterFrameDelayMs`（如需要）
- `rtuSilentIntervalChars` / `rtuSilentIntervalMs`（如需要）

> 说明：这些参数取决于底层 `tokio-modbus`/串口驱动是否暴露；如果没有，至少要在文档里明确“RTU 不并行，靠 batching + 超时/退避 + 稳定串口参数提升吞吐”。

---

## 5. 推荐默认值（以现场稳定优先）

### 5.1 你当前“0..800 holding register（约 801 个寄存器）”场景

- `maxGap = 1`
- `maxBatch = 120`

预计批次数 \(\lceil 801/120 \rceil = 7\)（最后一批更小）。

### 5.2 TCP 默认

- `tcpPoolSize = 2`
- `maxInflight = 2`（=pool_size）

> 如果设备强、网络 RTT 大、采集周期紧张，再逐步加到 `pool_size=4`；不建议一上来拉很大。

### 5.3 RTU 默认

- `maxInflight = 1`
- `pool_size = 1`（强制）
- `maxBatch = 60~120`（取决于波特率、误码率、超时设置；从 80 或 100 起步更稳）

---

## 6. 观测与调参方法（必须配套，否则并行会“看起来变快但其实更不稳”）

建议至少观测以下指标（driver 已有总请求/成功/失败/平均 RTT，可扩展为 per-worker）：

- **成功率**：成功请求 / 总请求（按 op_label：0x03/0x04/0x01/0x02 分开）
- **p50/p95 RTT**：平均值不够用，至少要 p95（否则超时边界不清）
- **队列深度**：worker queue depth（是否堆积）
- **重连频率**：reconnect/min
- **周期完成率**：每采集周期是否能在 period 内完成全量 batches

调参顺序（推荐）：

1. 固定 `maxGap=1`（先保证不读洞）
2. 调 `maxBatch`（从 120 开始；超时/失败升高则降到 100/80）
3. TCP 下调 `pool_size/maxInflight`（从 2→4；观察成功率与重连频率是否恶化）

---

## 7. 渐进式迁移步骤（最小侵入、可回滚）

> 目标：先把架构抽象铺好，默认行为不变；再逐步启用 TCP 并行。

### Step 1：抽象出 `SessionPool`，但 `pool_size=1`

- 保持现有 `SessionEntry/Supervisor` 行为不变
- Driver 侧改为通过 `pool.dispatch(op)` 执行
- 回归测试：功能等价

### Step 2：TCP 实现 `pool_size>1`（多 worker / 多 supervisor）

- 为 TCP 创建 N 个 `SessionWorker`（每个独立连接）
- 调度层对 read batches 并行 dispatch（受 `maxInflight` 限制）
- 写入仍保持串行（至少 per slave 串行）

### Step 3：完善策略与保护

- per-slave 固定路由 / per-slave mutex
- 自适应并发（失败率升高则降并发）
- 更完善的指标与告警

回滚：把 `tcpPoolSize=1` 或 `maxInflight=1` 即可退回串行。

---

## 8. 风险清单与规避（实战经验）

- **设备只允许单连接**：并行会导致频繁断线。规避：默认 `pool_size=1/2`；提供 “单连接模式” 开关；连接失败时自动降级。
- **并发导致设备响应变慢**：p95 RTT 上升并触发超时。规避：提高 `read_timeout_ms` 或降低 `maxInflight/pool_size`。
- **写并发导致状态错乱**：规避：对同 slave 严格串行（3.4）。
- **RTU 误配并行**：规避：RTU 强制 `pool_size=1`、`maxInflight=1`，即使用户配置更大也要 clamp。

---

## 9. 附：你们当前 maxGap 的一个“易踩坑”点

由于判定是 `next_start - batch_end <= maxGap`，当点是连续地址（例如 0、1、2...）时差值为 1。

- 如果你把 `maxGap` 设为 0，会导致连续点 **无法合并**，请求数暴增（吞吐明显下降）。
- 推荐默认 `maxGap=1`，既能合并连续地址，又能最大化避免“读洞”。

