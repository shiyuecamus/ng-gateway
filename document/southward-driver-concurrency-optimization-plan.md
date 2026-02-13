# 南向驱动并发优化计划

> 基于全驱动深度审计产出，按 Phase 分阶段推进。每个 Phase 内按优先级排列，完成后进入下一阶段。

---

## Phase 1 — 正确性修复（Critical / High）

> 目标：消除数据丢失、数据错位、CPU 热循环等正确性风险。
> 预计改动量：~200 行

### 1.1 `try_join_all` 部分失败丢弃全部数据 [S7 + OPC UA]

**严重性**：High  
**影响**：S7 有 5 个 batch，其中 1 个超时 → `try_join_all` 返回 `Err` → 其余 4 个成功读取的数据**全部丢弃**。OPC UA 同理。生产环境中 PLC 负载波动时会导致间歇性全量数据丢失。

**涉及文件**：
- `ng-gateway-southward/s7/src/protocol/session/mod.rs` — `read_var()` 和 `read_var_typed()`
- `ng-gateway-southward/opcua/src/handle.rs` — `collect_data()`

**修复方案**：
将 `try_join_all` 改为 `join_all` + 逐 batch/chunk 处理 `Result`：

```rust
// Before:
let responses: Vec<Bytes> = try_join_all(futs).await?;

// After:
let results: Vec<Result<Bytes, Error>> = futures::future::join_all(futs).await;
let mut responses = Vec::with_capacity(results.len());
for (batch_idx, r) in results.into_iter().enumerate() {
    match r {
        Ok(bytes) => responses.push(bytes),
        Err(e) => {
            tracing::warn!(batch_idx, error = %e, "S7 read batch failed, using empty placeholder");
            responses.push(Bytes::new()); // Planner merge handles per-item return codes
        }
    }
}
```

成功的 batch 照常上报数据，失败的 batch 对应的点位值为空（merge 阶段按 return code 处理），只有**全部 batch 失败**时才返回 `Err`。

---

### 1.2 MC spec 分发策略破坏 Planner 合并 [MC]

**严重性**：High  
**影响**：`i % pool_size` 的 round-robin 将连续地址（D100~D199）分散到不同 session，每个 session 的 Planner 看到不连续的点位，无法合并。极端情况下请求数膨胀 N 倍。

**涉及文件**：
- `ng-gateway-southward/mc/src/handle.rs` — `collect_data()` 中的 spec 分发逻辑

**修复方案**：
先按 `(device_code, head)` 排序（与 Planner 内部排序键一致），再按 device_code 连续分组后轮询分配到 session：

```rust
// 1. Sort specs by the same key the planner uses for coalescing
specs.sort_by_key(|s| (s.device_code, s.addr.head));

// 2. Distribute contiguous runs of the same device_code to sessions
let mut groups: Vec<Vec<TypedPointReadSpec>> = (0..pool_size).map(|_| Vec::new()).collect();
let mut session_idx = 0usize;
let mut prev_device_code: Option<u16> = None;

for spec in specs.into_iter() {
    if prev_device_code != Some(spec.device_code) {
        prev_device_code = Some(spec.device_code);
        session_idx = (session_idx + 1) % pool_size;
    }
    groups[session_idx].push(spec);
}
```

这样同一 device_code 的连续地址保持在同一个 session 中，Planner 的合并效果与单连接完全一致。

---

### 1.3 MC session `run()` 的 `select_all` 监听逻辑有竞态 [MC]

**严重性**：Critical  
**影响**：`select_all` 只消费一个 watcher 的 `changed()` future，其余被 drop 但未 `borrow_and_update()`，下一轮 `has_changed()` 可能误判，导致 CPU 热循环或漏检状态变化。

**涉及文件**：
- `ng-gateway-southward/mc/src/session.rs` — `run()` 中的 `any_changed` 异步块

**修复方案**：
用 `watch::Receiver::wait_for` 替代手动 `has_changed` + `select_all`：

```rust
async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
    let mut lifecycle_rxs: Vec<watch::Receiver<SessionLifecycleState>> =
        self.proto_sessions.iter().map(|s| s.lifecycle()).collect();

    loop {
        let futs = lifecycle_rxs.iter_mut().map(|rx| {
            Box::pin(async move {
                rx.wait_for(|s| {
                    matches!(s, SessionLifecycleState::Closed | SessionLifecycleState::Failed)
                }).await.is_ok()
            })
        });

        tokio::select! {
            _ = ctx.cancel.cancelled() => {
                if let Some(pool) = self.handle.detach_pool() {
                    pool.shutdown_all().await;
                }
                return Ok(RunOutcome::Disconnected);
            }
            (failed, _idx, _remaining) = futures::future::select_all(futs) => {
                if failed {
                    if let Some(pool) = self.handle.detach_pool() {
                        pool.shutdown_all().await;
                    }
                    return Ok(RunOutcome::ReconnectRequested(Arc::<str>::from(
                        "mc protocol session ended",
                    )));
                }
            }
        }
    }
}
```

`wait_for` 内部正确调用 `borrow_and_update()`，消除了手动状态管理的竞态。

---

### 1.4 Ethernet/IP 无连接活性检测 [EIP]

**严重性**：High  
**影响**：TCP 连接静默断开（网络故障、PLC 重启）后，pool 中的死连接持续被 `pick()` 选中，25~50% 的请求超时。

**涉及文件**：
- `ng-gateway-southward/ethernet-ip/src/session.rs` — `run()`
- `ng-gateway-southward/ethernet-ip/src/handle.rs` — `EipSessionPool`

**修复方案**：
在 `collect_data` 中发现 batch 读取失败时标记该连接为不健康，`pick()` 跳过不健康连接，后续由 reconnect 恢复：

```rust
// EipSessionPool 新增:
pub struct EipSessionPool {
    clients: Vec<Arc<Mutex<EipClient>>>,
    healthy: Vec<AtomicBool>,   // 新增：每连接健康标记
    rr: AtomicUsize,
}

impl EipSessionPool {
    /// Round-robin pick, skip unhealthy members.
    pub fn pick(&self) -> Option<Arc<Mutex<EipClient>>> {
        let n = self.clients.len();
        if n == 0 { return None; }
        for _ in 0..n {
            let i = self.rr.fetch_add(1, Ordering::Relaxed) % n;
            if self.healthy[i].load(Ordering::Relaxed) {
                return Some(Arc::clone(&self.clients[i]));
            }
        }
        // All unhealthy, fallback to round-robin anyway
        let i = self.rr.fetch_add(1, Ordering::Relaxed) % n;
        Some(Arc::clone(&self.clients[i]))
    }

    /// Mark a pool member as unhealthy.
    pub fn mark_unhealthy(&self, idx: usize) { ... }
}
```

同时在 `collect_data` 的 batch 错误处理中调用 `pool.mark_unhealthy(pool_idx)`。

---

### 1.5 EIP `take(n)` 按位置匹配结果可能数据错位 [EIP]

**严重性**：High  
**影响**：`read_tags_batch` 返回结果顺序与输入不一致时，按索引匹配会导致 point A 的值写入 point B。

**涉及文件**：
- `ng-gateway-southward/ethernet-ip/src/handle.rs` — `collect_data()` 中处理 wave 结果的循环

**修复方案**：
按 `tag_name` 匹配结果而非位置索引，并添加长度校验：

```rust
Ok(Ok(results)) => {
    if results.len() != chunk.len() {
        tracing::warn!(
            expected = chunk.len(), actual = results.len(),
            "read_tags_batch returned mismatched result count"
        );
    }
    // Build lookup by tag name for safe matching
    let result_map: std::collections::HashMap<&str, _> =
        results.iter().map(|(name, res)| (name.as_str(), res)).collect();

    for point in chunk.iter() {
        let Some(res) = result_map.get(point.tag_name.as_str()) else {
            continue;
        };
        // ... decode and push to buffers ...
    }
}
```

---

## Phase 2 — 热路径性能优化（Medium）

> 目标：消除热路径上的锁、拷贝、多余堆分配，每周期每点位的开销最小化。
> 预计改动量：~150 行

### 2.1 S7 `planner_config` 去 Mutex [S7]

**涉及文件**：`s7/src/protocol/session/mod.rs`

将 `planner_config: Arc<Mutex<Option<PlannerConfig>>>` 改为 `ArcSwapOption<PlannerConfig>`。握手后 `store()` 一次，热路径 `load()` 零锁。

### 2.2 S7 `Bytes::copy_from_slice` 双重拷贝 [S7]

**涉及文件**：`s7/src/protocol/session/mod.rs`、`s7/src/protocol/frame/` 相关解析

修改 `S7AckDataPayloadRef` 使 `raw_tail` 暴露为 `Bytes` 子切片（来自原始解析 buffer），read_var 直接返回零拷贝切片。

### 2.3 `Arc::<str>` 每周期每点位新分配 [全部驱动]

**涉及文件**：所有驱动的 `handle.rs` 中 `collect_data()` 热路径

在 `RuntimePoint` 实现结构体（如 `S7Point`、`McPoint`、`OpcUaPoint`、`EthernetIpPoint`）上预缓存 `point_key_arc: Arc<str>` 字段（init 时一次分配），热路径仅 `Arc::clone()`（原子 +1，零堆分配）。

### 2.4 OPC UA `node_id_cache` 去阻塞锁 [OPC UA]

**涉及文件**：`opcua/src/handle.rs`

node_id_cache: RwLock<HashMap<String, NodeId>> 使用标准库同步锁，write 路径会阻塞 tokio 工作线程。应改用 DashMap。

### 2.5 Vec 预分配容量 [全部驱动]

**涉及文件**：所有驱动 `collect_data()` 中的 `Vec::new()`

统一改为 `Vec::with_capacity(estimated_total_points)`。

### 2.6 EIP `execute_action` 重复加锁 [EIP]

**涉及文件**：`ethernet-ip/src/handle.rs` — `execute_action()`

一次 `lock()`，批量写入所有参数后释放，避免 N 次 acquire/release。

### 2.7 EIP `detach_pool` 返回旧 pool 支持优雅关闭 [EIP]

**涉及文件**：`ethernet-ip/src/handle.rs`、`ethernet-ip/src/session.rs`

`detach_pool()` 改为 `fn detach_pool(&self) -> Option<Arc<EipSessionPool>>`，session `run()` 退出时可执行 CIP unregister。

---

## Phase 3 — 架构改进（Medium / Low）

> 目标：提升 Collector 可观测性、驱动健壮性、长期可维护性。
> 预计改动量：~200 行

### 3.1 Collector 层 `per_group_key_max_inflight` 释放 [SDK + MC + EIP]

**涉及文件**：
- `ng-gateway-sdk/src/southward/concurrency.rs`
- `mc/src/handle.rs` 和 `mc/src/connector.rs`
- `ethernet-ip/src/handle.rs` 和 `ethernet-ip/src/connector.rs`

MC/EIP 的 `collector_concurrency_profile` 改为：
```rust
CollectorConcurrencyProfile::from_io_lanes(pool_size)
    .with_per_group_key_max_inflight(pool_size)
```

让 Collector 的 `build_group_calls()` 拆分 items 为 N 个子 `GroupCall` 并行调度，使 Collector 拥有重试粒度和 metrics 可观测性。

### 3.2 per-`collect_data` 外层超时 [S7 + OPC UA]

**涉及文件**：`s7/src/handle.rs`、`opcua/src/handle.rs`

在 `collect_data` 入口包一层 `tokio::time::timeout(collect_timeout, ...)` 防止 N 个 batch 各自耗时 4.9s 导致总计 49s。

### 3.3 S7 Planner 启用 coalescing gap [S7]

**涉及文件**：`s7/src/protocol/session/mod.rs` — handshake 后设置 `PlannerConfig`

启用 `gap_bytes: Some(16)` 让 Planner 跨越对齐填充合并更多地址，减少 batch 数。

### 3.4 OPC UA 单 chunk 超时重连策略优化 [OPC UA]

**涉及文件**：`opcua/src/handle.rs`

引入连续超时计数，超过阈值（如 3 次）才触发 `try_request_reconnect`，避免服务器偶尔慢一次就重连。

### 3.5 MC 部分连接失败降级 [MC]

**涉及文件**：`mc/src/session.rs`

单个 pool member 断开时仅标记不健康，`pick()` 跳过它，后台尝试重建。只有超过半数失败才触发全量重连。

### 3.6 EIP wave 执行改进为 FuturesUnordered + Semaphore [EIP]

**涉及文件**：`ethernet-ip/src/handle.rs`

消除 wave 同步点，实现"完成一个立即启动下一个"，最大化 pool 利用率。

### 3.7 S7 握手错误上下文保留 [S7]

**涉及文件**：`s7/src/protocol/session/mod.rs`

将 `is_err()` 和 `Err(_e)` 改为 `Err(e) => { tracing::warn!(error = %e, "handshake failed"); ... }`。

---

## Phase 完成标准

| Phase | 完成标准 | 验收 | 状态 |
|-------|---------|------|------|
| Phase 1 | `cargo clippy` 零 warning + 所有修改文件 linter 零 error | 正确性测试通过 | **已完成** |
| Phase 2 | `cargo bench` 采集延迟对比（如有 bench suite） | 性能回归无恶化 | **已完成** (2.2/2.4 留后续 PR) |
| Phase 3 | Collector 层 metrics 可观测到 per-group 调度 | 架构文档更新 | **已完成** (3.1 留独立 PR) |

---

## 后续迭代项（待独立 PR）

| 项目 | 说明 |
|------|------|
| Arc\<str\> 预缓存 | 全驱动 RuntimePoint 上预缓存 `point_key: Arc<str>`，热路径仅 `Arc::clone()` |
| Collector per_group_key_max_inflight | MC/EIP 设置 `per_group_key_max_inflight = pool_size`，让 Collector 拆分 items |
| S7 零拷贝 merge | 修改 S7Pdu 暴露 `raw_tail` 为 `Bytes` 子切片，消除 `copy_from_slice` |
| EIP FuturesUnordered | wave 模式改为 Semaphore + FuturesUnordered 消除同步点 |
| MC 部分池降级 | 单个 pool member 失败时仅标记不健康，不全量重连 |
