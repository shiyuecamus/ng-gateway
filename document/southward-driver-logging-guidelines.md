# 南向驱动开发：日志级别与 Span 传播规范（必须遵循）

> 目标：**保证任意南向驱动**在“全局日志级别”和“按通道日志级别”组合下都能得到正确行为，且**第三方库日志（log/tracing）也能按 channel 过滤**。
>
> 适用范围：`ng-gateway-southward/*` 下所有驱动（含 supervisor / protocol session / background tasks）。

---

## 1. 核心背景（为什么必须这么做）

网关 host 侧的按通道过滤依赖 `channel_id`：

- 如果 log event 能归属到某个 `channel_id`，host 侧就能使用该 channel 的 effective level（例如全局 INFO + 通道 DEBUG 时放行 DEBUG）。
- 如果 log event **没有 `channel_id`**，host 侧只能退化为全局 level（会把你“通道 DEBUG”的期待过滤掉）。

而 `tokio::spawn` **默认不会继承当前 span**，这会导致：

- 驱动代码（或第三方库）在 spawned task 内产生的日志事件丢失 `channel_id`
- 最终表现为：**全局 INFO + 通道 DEBUG 时，通道 DEBUG 日志不输出**

---

## 2. 两条硬原则（所有规则都从这里推导）

### 2.1 原则 A：`channel_id` 必须始终可见

驱动执行路径（以及其依赖的第三方库）发出的日志，必须处在 **带 `channel_id` 的 tracing span** 内。

> 备注：tracing 的 span **不会自动继承字段**；第三方库创建的子 span 往往不重复写 `channel_id`，因此 SDK 侧需要在 span 创建时从 parent 继承 `channel_id`（不要删除该兜底逻辑）。

### 2.2 原则 B：任何 `tokio::spawn` 都会截断 span，必须显式继承

只要 spawn，就必须让 spawned future 运行在正确 span 上：

- 优先继承：`.instrument(tracing::Span::current())`
- 或显式创建 span：`info_span!(..., channel_id = <id>)` 再 `.instrument(span)`

---

## 3. 必须遵守的编码规则（Do / Don’t）

### 3.1 Don’t：在驱动热路径内部“为了 timeout/并发”再包一层 `tokio::spawn`

禁止模式（示例）：

```rust
// ❌ 禁止：这会断开 span，导致 dependency logs 丢失 channel_id
let _ = tokio::spawn(async move {
    tokio::time::timeout(d, op()).await
}).await;
```

推荐模式：

```rust
// ✅ 推荐：直接 await，保持在当前 channel_id span 内
let res = tokio::time::timeout(d, op()).await;
```

适用位置：

- `collect_data`
- `execute_action`
- `write_point`
- driver 内部的 `run_op` / `run_request` 等 hot path helper

### 3.2 Do：Supervisor / EventLoop 必须带 `channel_id` span

推荐模式（supervisor 主循环）：

```rust
use tracing::Instrument;

let span = tracing::info_span!("xxx-supervisor", channel_id = channel.id);
tokio::spawn(async move {
    // reconnect loop / state machine ...
}.instrument(span));
```

### 3.3 Do：协议 Session 层内部 spawn（IO driver）必须继承当前 span

推荐模式（session event loop 里）：

```rust
use tracing::Instrument;

tokio::spawn(async move {
    // io driver
}.instrument(tracing::Span::current()));
```

> 说明：协议层往往是第三方库/内部协议栈日志的集中输出点；这里丢 span 会导致大量日志不可按通道过滤。

### 3.4 Do：驱动内部确实需要 spawn 时，必须继承 span

推荐模式（短任务/后台任务）：

```rust
use tracing::Instrument;

tokio::spawn(async move {
    // ...
}.instrument(tracing::Span::current()));
```

如果你手上天然有 channel_id（例如 supervisor 的 channel config），建议使用显式 span（更稳定、更自解释）：

```rust
use tracing::Instrument;

let span = tracing::info_span!("xxx-task", channel_id = channel_id);
tokio::spawn(async move { /* ... */ }.instrument(span));
```

---

## 4. 新驱动落地模板（推荐直接复制）

### 4.1 start()：启动 supervisor + 后台循环

- **supervisor**：必须 `channel_id` span
- **接收循环/订阅循环**：必须 `channel_id` span（或继承当前 span）

### 4.2 run_op()：单次操作（读/写/命令）

- 不要 spawn
- 直接 `timeout(...).await`
- 所有 `tracing::warn!/debug!` 都会自动带 `channel_id`
- 第三方库日志（log/tracing）也会自动被桥接并按通道过滤

---

## 5. 自检清单（提交 PR 前必须过一遍）

### 5.1 Span/日志自检

- [ ] driver 热路径没有“为了 timeout/并发”额外 `tokio::spawn`
- [ ] 所有 supervisor/eventloop 的 `tokio::spawn` 都 `.instrument(info_span!(..., channel_id=...))`
- [ ] 协议 session 层内部 spawn 都 `.instrument(Span::current())`
- [ ] 任意 background task（metrics/subscribe/heartbeat 等）spawn 都 instrument

### 5.2 行为自测（两组必测用例）

- [ ] **全局 DEBUG + 通道 INFO**：该通道的 DEBUG 日志不应输出（正确抑制）
- [ ] **全局 INFO + 通道 DEBUG**：该通道的 DEBUG 日志应输出（含第三方库日志）

### 5.3 “第三方库日志”确认点

- [ ] 依赖库使用 `log`：确认 driver init 已启用 log→tracing bridge（SDK 已处理）
- [ ] 依赖库使用 `tracing`：确认其事件处在 `channel_id` span 内（本规范解决）

---

## 6. 常见错误与症状（快速定位）

### 6.1 症状：全局 INFO + 通道 DEBUG，看不到第三方库 DEBUG

高概率原因：

- 驱动内部 `tokio::spawn` 把 span 断开（最常见）
- supervisor/eventloop spawn 未 instrument（连接/重连/协议栈日志丢 channel_id）
- session 层内部 spawn 未 instrument（协议 IO driver 日志丢 channel_id）

定位方式（建议）：

- 在 driver 关键入口打印一次 `tracing::debug!(...)`，观察该行是否带 `channel_id`
- 找到“缺 channel_id 的日志块”前后的任务边界（通常就是 spawn 点）

---

## 7. 可选工程化建议（降低心智负担）

### 7.1 SDK helper（推荐）

在 SDK 提供统一 helper（或宏），把“spawn 必须继承 span”变成默认写法：

- `spawn_in_current_span(fut)`：内部等价 `tokio::spawn(fut.instrument(Span::current()))`
- `spawn_with_channel_span(channel_id, name, fut)`：内部创建 `info_span!(name, channel_id=...)`

目标：驱动作者 **不需要记住细节**，只要“spawn 就用 helper”。

### 7.2 CI 规则（强烈推荐）

在 CI/脚本里对 `ng-gateway-southward/**` 做约束：

- 禁止直接出现 `tokio::spawn(`（或要求所有 spawn 行必须包含 `.instrument(`）
- 违规直接失败，避免靠 code review 人肉发现

---

## 8. 结论

只要严格遵循：

- **热路径不二次 spawn**
- **所有 spawn 都 instrument 且 span 带 channel_id**

就能保证：**全局 INFO + 通道 DEBUG 时，该通道 DEBUG（包括第三方库）一定会输出**，并且不会破坏“全局 DEBUG + 通道 INFO 的抑制行为”。

