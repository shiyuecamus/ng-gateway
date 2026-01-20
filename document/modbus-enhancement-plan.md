# Modbus 采集性能增强综合计划（ng-gateway）

> 范围声明（按你的要求做了收敛）：
>
> - **只覆盖采集热路径**：Collector 调度 + Driver 能力扩展 + Modbus 批量语义增强 + TCP/RTU 连接池可配置
> - **不包含**：控制面清空、plan cache
> - 目标：在“同一物理从站（同 slaveId）被拆成多个业务设备”的建模下，让采集从“大量碎请求 + mutex 串行排队”升级为“按物理分组聚合采集 + 最少请求数”，并保持设计可扩展到 IEC104 等协议

---

## 0. 背景与问题复盘（为什么会慢）

你当前的典型场景：

- 同一 Modbus Channel 下 50+ 业务设备（业务分组/权限/展示需要）
- 这些业务设备的 `slaveId` 实际相同（例如全是 1），属于同一物理从站/同一条协议会话
- 点位规模大（例如 0..809 holding registers）+ bits 地址稀疏

当前架构的根因：

- Collector 按 **device** 并发触发 `driver.collect_data(device, points)`，但 Modbus driver 内部对同一 `Context` 必然 **mutex 串行化**（还要 `set_slave`），并发没有吞吐收益
- planner 的合并只发生在“单业务设备的点集合”内部，业务设备边界把连续地址切碎，导致大量小请求（你日志里 `ReadHoldingRegisters(x, 20)` / `ReadDiscreteInputs(addr, 1)`）

要根治：必须把“采集最小执行单元”从 **业务设备** 提升到 **物理分组**（例如 Modbus 按 slaveId、IEC104 按 common address），但仍保留业务设备的产出语义。

---

## 1. 设计目标与硬约束

### 1.1 目标

- **正确性**：同一物理会话/分组内请求顺序与单飞语义正确（不会因并发乱序引入隐患）
- **性能**：请求数接近理论最小（holding regs 810 个、上限 125 ⇒ ~7 次量级）
- **通用性**：Collector 只做“通用分组调度”，分组语义与聚合采集细节由 driver 决定（Modbus/IEC104/…）
- **热路径质量**：对象安全、少分配、少拷贝、易观测

### 1.2 硬约束（Modbus）

- 0x03/0x04 单次读寄存器数量 **≤125**
- RTU 同一总线必须单飞（连接池强制 1）；TCP 连接池可配置（默认 1）

---

## 2. 平台级最佳实践：在 `Driver` 上新增“可选分组采集能力”

你提出的两个方向里，最佳实践是：

- **Collector 负责分组与调度**（统一的限流、取消、超时、观测、回滚策略）
- **Driver 负责分组语义与聚合采集实现**（协议/领域知识）

在“只评估极致质量（不考虑破坏性/兼容性/改动面）”的前提下，最佳实践是：

- **Collector：统一做分组与调度**
- **Driver：统一只暴露一个采集入口，但该入口天然支持 batch（items）**

也就是说：**把 `Driver::collect_data` 升级为批量签名 `collect_data(items)`**，Collector 按策略分组后按组调用。

### 2.1 关键点：分组 key 不能假设是数字

分组 key 可能是：

- 单字段：Modbus `slaveId(u8)`
- 组合：IEC104 `(CommonAddress, LinkAddress)`、OPC UA endpoint 等
- 甚至非数值：字符串标识/枚举等

因此 `Option<u64>` 太武断。最佳实践是引入一个**对象安全、低分配、可组合**的 key 类型。

### 2.2 推荐 key 类型：`CollectionGroupKey([u8; 16])`

建议在 SDK 中定义：

- `pub struct CollectionGroupKey([u8; 16]);`

设计理由：

- **对象安全**：返回值是固定大小的 Copy/Clone 值，不涉及泛型/闭包/生命周期
- **低分配**：不需要 `String/Arc<str>`
- **足够表达力**：16 字节可承载：
  - 直接打包：`slaveId`、`(a,b)`、`u128`
  - 或承载哈希输出：对任意组合/字符串做稳定哈希后写入 16 字节
- **可扩展**：可在高位塞 `kind`（协议/分组类型），避免不同协议的 key 空间相互污染

推荐提供构造函数（概念示意，便于实现与审计）：

- `CollectionGroupKey::from_u64(kind: u32, v: u64)`
- `CollectionGroupKey::from_pair_u64(kind: u32, a: u64, b: u64)`
- `CollectionGroupKey::from_u128(kind: u32, v: u128)`
- `CollectionGroupKey::from_bytes(kind: u32, b: [u8; 12])`（kind + payload）

> 关于“非数字 key”：driver 可以把字符串/复合结构做 **稳定哈希**（例如 xxhash/ahash 的固定种子版本，或你们自定义 hash），写入 16 字节；碰撞概率极低且可观测（见风险章节）。

### 2.3 最终推荐的 `Driver` 采集接口（单一入口，天然支持分组）

在 `ng-gateway-sdk/src/southward/mod.rs` 的 `trait Driver`：

1) **分组 key（可选）**

- `fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey>`
- 返回 `Some(key)`：表示该 device 属于某个“物理采集分组”（例如 Modbus=slaveId；IEC104=common address 等）
- 返回 `None`：表示不做物理分组（Collector 会把它当成“单元素分组”）

2) **采集入口（唯一入口）**

- `async fn collect_data(&self, items: &[(Arc<dyn RuntimeDevice>, Arc<[Arc<dyn RuntimePoint>]>)] ) -> DriverResult<Vec<NorthwardData>>`

接口语义约定（极致质量必须写清楚）：

- **items 必须属于同一个分组调用上下文**
  - 当 `collection_group_key` 返回 `Some(k)` 时：Collector 只会把同一个 `k` 的 items 放在一次调用里
  - 当 `collection_group_key` 返回 `None` 时：Collector 保证 `items.len()==1`
- **items 顺序必须稳定**：Collector 以 `device_id` 升序构造 items（避免热路径的随机性与抖动）
- **输出语义**：driver 仍需输出按业务 device 组织的 `Vec<NorthwardData>`（Telemetry/Attributes）
- **错误语义**：一次调用失败是否允许“部分成功”必须由 driver 明确实现（建议：可返回部分成功 + 通过 metadata 标注失败原因；或直接 fail-fast。此处需要在实现阶段固化为一致规则）
- **超时语义**：Collector 对一次 `collect_data(items)` 应施加“组级超时”（而不是 per-device 超时），否则会破坏分组聚合的意义

> 结论：你提出的“每个 channel tick 先拿到所有设备 → 按策略分组 → 统一调用 `collect_data(items)`”就是这条路线；这是最干净的接口形态。

---

## 3. Collector 的通用调度（不懂协议，只懂 key）

### 3.1 每个 channel tick 的统一流程

对某个 channel：

1. 取 `device_ids`
2. 对每个 `device_id` 拉取：
   - `runtime_device`
   - `points`
   - `driver_handle`
3. 对每个 device：
   - `key = driver.collection_group_key(&*runtime_device)`
   - （强约束）分组必须在 **同一个 `driver_handle` 实例** 内完成；Collector 必须先按 `driver_handle` 分桶，再在桶内按 `key` 分组
4. 分桶：
   - `key = Some(k)` 的：按 `k` 分组，得到 `Vec<(device, points)>`
   - `key = None` 的：每个 device 自成一组（保证 `items.len()==1`）
5. 对每个分组调用（统一入口）：
   - `driver.collect_data(&items_in_group)`

### 3.2 并发与单飞约束

- **分组内**：由 driver 自己决定是否并发；对 Modbus/RTU 强制单飞，对 TCP 可做连接池并行（见第 5 章）
- **分组间**：Collector 通过 `Semaphore` 限制并发（沿用现有 `max_concurrent_collections`），避免热点 channel 抢占全局资源

> 备注：你如果希望“同一 channel 内最多并发 N 个 group”，可后续加一个 channel 级 limit；本计划不引入额外配置，复用现有 global semaphore 能先跑通。

---

## 4. Modbus 具体增强设计

### 4.1 分组 key（按 slaveId）

对 Modbus driver：

- `collection_group_key(device) = Some(CollectionGroupKey::from_u64(KIND_MODBUS_SLAVE, slave_id as u64))`

说明：

- 一个 driver 实例天然绑定一个 channel/连接配置，因此 key 不需要再包含 endpoint；若未来一个 driver 管多 endpoint，再把 endpoint hash 混入 key 即可（仍保持 16B）。

### 4.2 `collect_data(items)` 的输出语义（保留业务设备）

你明确要求业务设备继续保留，因此最佳实践是：

- **输入**：同一 slaveId 下的 `[(deviceA, pointsA), (deviceB, pointsB), ...]`
- **聚合采集**：driver 在内部合并点位，做最少 Modbus 请求
- **输出**：仍然输出按业务 `device_id` 组织的 `Vec<NorthwardData>`（Telemetry/Attributes）

这样 Collector 无需做“点值拆分”，也避免了“粗糙拆分层”的热路径额外开销与复杂度。

### 4.3 批量语义增强：4 个参数，分别用于 Registers 与 Bits

现状问题：你们 channel 只有 `maxGap/maxBatch`，且 UI 把 `maxBatch` 上限绑死到 125；这会把 bits 的批量能力也一并锁死（不合理）。

建议将 Modbus channel config 拆成：

- **Registers（0x03/0x04）**
  - `maxBatchRegisters`：范围读最大寄存器数量（**≤125**）
  - `maxGapRegisters`：合并间隙阈值
- **Bits（0x01/0x02）**
  - `maxBatchBits`：范围读最大 bit 数量（建议 UI 上限 2000）
  - `maxGapBits`：合并间隙阈值

planner 规则（保持简单、可解释）：

- 对 registers：按 address 排序；当 `gap <= maxGapRegisters && span <= maxBatchRegisters` 时合并
- 对 bits：按 address 排序；当 `gap <= maxGapBits && span <= maxBatchBits` 时合并

> 重要提示：在你们 planner 实现里，要合并连续地址点，`maxGap*` 必须 **≥1**（因为 `next_start - batch_end` 对连续点为 1）。因此建议默认：
> - `maxGapRegisters = 1`
> - `maxGapBits = 1` 起步，再按稀疏程度调整为 8/16/32

### 4.4 Modbus 默认值（最佳实践起步）

结合你的现场约束（regs 上限 125、bits 稀疏）：

- `maxBatchRegisters = 120`
- `maxGapRegisters = 1`
- `maxBatchBits = 512`（若设备足够强可到 1024/2000）
- `maxGapBits = 16`（稀疏读洞可控，减少 1bit/次退化）

### 4.5 `metadata.rs` 的 UI/约束调整

在 `ng-gateway-southward/modbus/src/metadata.rs`：

- 新增/替换字段：
  - `maxBatchRegisters`（default 120, max 125）
  - `maxGapRegisters`（default 1, max 2000）
  - `maxBatchBits`（default 512, max 2000）
  - `maxGapBits`（default 16, max 2000）
  
> 本计划不讨论兼容迁移；实现时可直接替换旧字段（`maxGap/maxBatch`）为上述 4 个字段。

---

## 5. TCP 多连接 / RTU 单飞：做成“可配置且默认 1”

你要求“默认给 1 就好，但可配置”。最佳实践：

### 5.1 配置项

在 Modbus channel config 增加：

- `tcpPoolSize`（default 1, min 1, 建议 max 8）

### 5.2 行为定义

- TCP：`tcpPoolSize = N` ⇒ driver 内维护 N 条独立连接（或 N 个 Context worker），每条连接单飞
- RTU：无论用户配置多少，强制 `tcpPoolSize = 1`（或单独 rtuPoolSize 但 clamp=1），避免误配

> 注意：连接池并不是解决“同 slaveId 业务设备拆分”的必要条件；它是对 TCP 设备吞吐上限的可选提升。即使 pool=1，只要做了分组聚合采集，请求数也会显著下降。

---

## 6. 风险与规避

### 6.1 分组 key 哈希碰撞

当 driver 用哈希把复合 key 压缩进 16B 时存在理论碰撞概率。

规避建议：

- 给 `CollectionGroupKey` 写入 `kind`（4B）+ `hash128`（12B 或 16B）  
- 在 debug/trace 级日志里输出“分组 key -> 代表性字段”（例如 Modbus slaveId），碰撞时可快速定位
- 对关键协议（Modbus/IEC104）优先用“可逆打包”（如 u64/u128）而不是哈希

### 6.2 driver 输出 `NorthwardData` 的一致性

由于分组采集会把多个业务设备合并读，driver 必须确保：

- 仍按业务 device_id 正确组装 Telemetry/Attributes
- 不跨设备污染 point_key、timestamp、metadata

建议在实现阶段加两类测试：

- 同一 slaveId 下多 device 的点位覆盖无冲突
- 同一点位在多个设备误配时的行为（应报错或 deterministic）

---

## 7. 落地计划（仅设计与步骤，不立刻动代码）

### Phase 1：SDK 接口扩展（最佳实践最终形态）

- 增加 `CollectionGroupKey([u8;16])`
- 在 `Driver` trait：
  - 增加 `collection_group_key(...) -> Option<CollectionGroupKey>`
  - 将 `collect_data` 升级为批量签名：`collect_data(items: &[(device, points)])`

### Phase 2：Collector 通用分组调度

- 在 channel tick 内：
  - 拉取 devices/points
  - 按 `collection_group_key` 分桶
  - 每个分组统一调用 `collect_data(items_in_group)`

### Phase 3：Modbus driver 实现分组聚合采集 + 参数语义增强

- 实现 `collection_group_key`（按 slaveId）
- 实现 `collect_data(items)`（在 items.len()>1 时做聚合采集）：
  - 合并 points，按 4 参数生成 batches
  - 读请求执行（单连接默认；TCP pool 可配置）
  - 输出按业务 device 组织的 `NorthwardData`
- 在 `metadata.rs` 增加新参数与最佳实践默认值

### Phase 4：TCP/RTU 连接池可配置

- TCP：`tcpPoolSize` 默认 1，可调大
- RTU：强制 1（clamp）

---

## 8. 验收标准（你可以用来验收落地质量）

- **请求数**：holding registers 0..809 场景，读请求数从几十次降到 ~7 次量级（maxBatchRegisters=120/125）
- **采集耗时**：同等网络/设备 RTT 下显著下降（目标 < 1s 量级，具体依设备 RTT）
- **业务设备语义**：北向/实时 UI 仍按业务 device_id 展示与上报
- **配置可控**：`tcpPoolSize` 默认为 1，RTU 不会误并行


---

## 9. Implementation Spec（可直接开工的工程规格）

> 本章是“必须/应该/可以”的规范条款，落地时按此实现；若要偏离，必须在评审里明确写出偏离原因与风险。

### 9.1 SDK：新增类型与 trait 变更（精确定义）

#### 9.1.1 `CollectionGroupKey`

**位置**：`ng-gateway-sdk/src/southward/mod.rs`（或拆到 `sdk::southward::grouping` 子模块也可）

**定义要求（MUST）**：

- `CollectionGroupKey` 必须是**固定大小**、**可复制**、**可哈希**、**可比较**的值类型
- 必须能作为 `HashMap` key 使用
- 必须支持稳定的 debug 输出（用于日志/排障）

推荐定义（示意）：

```rust
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct CollectionGroupKey(pub [u8; 16]);

impl CollectionGroupKey {
    /// 建议：前 4B 为 kind，后 12B 为 payload（或直接 16B payload 也可，但 kind 会更可观测）
    pub fn from_u64(kind: u32, v: u64) -> Self { /* ... */ }
    pub fn from_pair_u64(kind: u32, a: u64, b: u64) -> Self { /* ... */ }
    pub fn from_u128(kind: u32, v: u128) -> Self { /* ... */ }
    pub fn from_hash128(kind: u32, h: [u8; 16]) -> Self { /* ... */ }
}
```

**哈希 key 规则（MUST）**：

- 如果 driver 选择用哈希压缩复合 key，必须使用**稳定种子**（不可用随机种子哈希），避免同配置重启后分组键变化导致观测困难。
- 对 Modbus/IEC104 这类关键协议，优先用**可逆打包**而不是哈希（MUST-SHOULD）。

#### 9.1.2 `Driver` trait：采集入口升级为批量签名

**位置**：`ng-gateway-sdk/src/southward/mod.rs`

**变更要求（MUST）**：

- 将采集接口升级为唯一入口：

```rust
async fn collect_data(
    &self,
    items: &[(Arc<dyn RuntimeDevice>, Arc<[Arc<dyn RuntimePoint>]>)]
) -> DriverResult<Vec<NorthwardData>>;
```

- 新增分组 key（建议提供默认实现返回 `None`，但本计划不要求兼容；是否 default 取决于你希望的改造强度）：

```rust
fn collection_group_key(&self, device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> { None }
```

**输入不变量（MUST）**：

- `items.len() == 0`：Collector 不得调用（MUST NOT）。Driver 侧可以 `debug_assert!` 或直接返回 `Ok(vec![])`。
- 若 `collection_group_key` 返回 `None`：Collector 必须保证 `items.len() == 1`。
- 若 `collection_group_key` 返回 `Some(k)`：Collector 必须保证本次调用的 `items` 都属于同一个 `k`。

**输出不变量（MUST）**：

- Driver 必须输出按业务 device 组织的 `NorthwardData`（不允许把多个业务设备硬合并成一个虚拟设备上报）。

### 9.2 Collector：分组、并发、超时与 tick 语义（必须条款）

#### 9.2.1 分组算法（MUST）

在一个 channel tick 内：

1. 收集本 tick 要采集的设备集合 `device_ids`
2. 对每个设备获取：
   - `driver_handle: Arc<dyn Driver>`
   - `runtime_device: Arc<dyn RuntimeDevice>`
   - `points: Arc<[Arc<dyn RuntimePoint>]>`
3. **先按 `driver_handle` 分桶**（MUST）：
   - key = `Arc::as_ptr(&driver_handle)`（指针身份）或其他“同实例稳定”方式
4. 在每个 driver bucket 内，再按 `collection_group_key(device)` 分桶：
   - `Some(k)`：进入物理分组 k
   - `None`：每个 device 自成一组（`items.len()==1`）
5. 对每个分组构造 `items`：
   - `items` 必须按 `device_id` 升序排序（MUST）
   - `points` 为空的设备必须在 Collector 侧提前过滤掉（MUST），避免 driver 做无意义工作

#### 9.2.2 tick 行为（MUST）

为避免 period < 耗时导致“追赶 tick”长期满载：

- channel collection loop 必须设置 `tokio::time::MissedTickBehavior::Skip`（MUST）
- 如果上一次 tick 还在执行，新 tick 必须直接跳过或被合并（MUST；推荐 Skip）

#### 9.2.3 超时语义（MUST）

- 现有 `collection_timeout_ms`（或同等配置）从“per-device timeout”升级为 **per-group timeout**（MUST）
- Collector 对每次 `driver.collect_data(items_in_group)` 施加超时：
  - 超时返回 `Err(timeout)`，该 group 计为失败

#### 9.2.4 并发语义（SHOULD）

- Collector 允许多个 group 并行，但必须受全局 `Semaphore(max_concurrent_collections)` 限制（MUST）
- 同一 driver 实例内的多个 group 是否并行：
  - **推荐**：允许并行，由 driver 内部约束（TCP pool 可并行、RTU clamp=1）（SHOULD）
  - 若实现更简单，也可在 driver bucket 内串行执行（MAY），但会牺牲多 slave 并行能力

### 9.3 错误与“部分成功”语义（必须定死，否则会变成不可测试）

本计划建议采用 **“部分成功允许”** 作为最佳实践（因为现场网络/设备偶发失败很常见，fail-fast 会导致整轮数据全空）。

**规范（MUST）**：

- Driver 在一次 `collect_data(items)` 内可以返回部分业务设备的数据（Ok + subset）。
- 发生以下情况必须返回 `Err`（fail the whole group）：
  - 连接不可用/需要重连（transport error、协议栈断开）
  - group 级超时
  - 配置/建模错误（例如 items 内 slaveId 不一致、点位类型不支持）

**缺失点位的表现（MUST）**：

- 对于“某些 batch 失败导致部分点位无法读取”的情况：
  - 这些点位在本次输出中应被**省略**（不输出旧值/默认值）
  - driver 必须在自身 metrics/日志里记录失败 batch（见 9.6 可观测性）

### 9.4 Modbus Driver：`collect_data(items)` 的详细实现规格

#### 9.4.1 输入校验（MUST）

- `items.len()==1`：允许走快速路径（现有逻辑）
- `items.len()>1`：
  - 必须校验所有 `RuntimeDevice` 都是 ModbusDevice（否则 `ConfigurationError`）
  - 必须校验所有 device 的 `slaveId` 一致（否则 `ConfigurationError`）

#### 9.4.2 点位合并与去重（MUST）

将所有 `points` 合并成一个逻辑列表：

- 只保留 AccessMode 为 `Read`/`ReadWrite` 的点位（MUST）
- 对重复 point_id 的点位（多个业务设备引用同一点）：
  - **推荐**：视为建模错误并报 `ConfigurationError`（MUST-SHOULD）
  - 或者 deterministic 选择第一条（MAY，但要在文档里写清楚风险）

#### 9.4.3 批次规划（MUST）

planner 必须按 function code 分组，并且对寄存器与 bits 使用不同的 batch 参数：

- `ReadHoldingRegisters / ReadInputRegisters`：
  - `maxBatch = maxBatchRegisters.clamp(1, 125)`
  - `maxGap = maxGapRegisters`
- `ReadCoils / ReadDiscreteInputs`：
  - `maxBatch = maxBatchBits.clamp(1, 2000)`（上限可配置，但 UI 建议 2000）
  - `maxGap = maxGapBits`

> `maxGap*` 连续合并要求：必须允许 ≥1，否则连续地址也无法合并（已在文档前文解释）。

#### 9.4.4 执行模型（MUST）

- RTU：连接池强制 1（clamp），同一时刻只允许一个 in-flight 请求（MUST）
- TCP：连接池大小 `tcpPoolSize` 可配置，默认 1；每条连接单飞（MUST）
- 对本 group（单一 slaveId）内的 batch：
  - 推荐在单连接内串行执行（MUST-SHOULD）
  - 若未来要并行，只能通过多连接 worker 做，并且不能在同一个 `Context` 上并发（MUST）

#### 9.4.5 解码与按业务设备组装输出（MUST）

driver 必须按业务 device_id 组织输出：

- 以 group 开始时间 `ts = Utc::now()` 作为本轮所有输出的 timestamp（MUST），保证同一轮一致性
- 将读取结果按 point.device_id 分桶，分别构造：
  - `NorthwardData::Telemetry(TelemetryData { device_id, device_name, timestamp: ts, values: Vec<PointValue>, ... })`
  - `NorthwardData::Attributes(AttributeData { ... })`
- values 必须按 `point_id` 升序（或 point_key 升序）稳定排序（SHOULD），减少下游抖动

### 9.5 Modbus 配置与 UI metadata（实现规格）

#### 9.5.1 配置字段（MUST）

Modbus channel config 必须新增（或替换）以下字段：

- `maxBatchRegisters: u16`（<=125）
- `maxGapRegisters: u16`
- `maxBatchBits: u16`（<=2000）
- `maxGapBits: u16`
- `tcpPoolSize: u16`（>=1；RTU 强制 clamp=1）

#### 9.5.2 metadata 默认值（MUST）

在 `ng-gateway-southward/modbus/src/metadata.rs`：

- `maxBatchRegisters` 默认 **120**，最大 **125**
- `maxGapRegisters` 默认 **1**
- `maxBatchBits` 默认 **512**，最大 **2000**
- `maxGapBits` 默认 **16**
- `tcpPoolSize` 默认 **1**

### 9.6 可观测性（MUST）

为保证现场可调参与可回归：

- Collector 侧（per channel tick）至少记录：
  - group 数、每 group 的 items.len、总耗时、超时/失败数
- Modbus driver 侧至少记录：
  - 总请求数、成功/失败数、p95 RTT（平均值不够，至少 p95）
  - 每轮 planner 产出的 batch 数（按 function code 分开）
  - 连接池大小与当前健康状态

### 9.7 测试计划（必须具备，否则不可称为“极致质量”）

#### 9.7.1 SDK 单测

- `CollectionGroupKey` 的：
  - equality/hash 稳定性
  - kind 隔离（不同 kind 不应混组）

#### 9.7.2 Collector 单测

- `collection_group_key=None` ⇒ `items.len()==1`
- `collection_group_key=Some(k)` ⇒ 同 k 的设备被聚合到同一次 `collect_data(items)`
- 不同 `driver_handle` 必须分开（不允许跨 driver 混组）
- items 顺序稳定（device_id 升序）

#### 9.7.3 Modbus driver 单测/集成测

- 50 个业务设备、同一 slaveId、0..809 holding registers：
  - 规划 batch 数应约等于 \(\lceil 810/maxBatchRegisters \rceil\)（以 120 为例约 7）
- bits 稀疏地址：
  - 不允许退化成“每点 1 请求”（可用上限断言：请求数 < 点数）
- TCP pool：
  - 配置为 1 时行为等价串行
  - 配置为 N 时创建 N worker/连接（可通过 mock/计数验证）
- RTU clamp：
  - 无论配置 tcpPoolSize 多大，都必须以 1 执行

