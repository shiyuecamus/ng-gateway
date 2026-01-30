## NG Gateway `mapped_json`（JMESPath）最佳实践：语义完整的高性能设计与 Phase 推进计划

> 本文目标：在**不破坏现有配置兼容性**的前提下，把 `mapped_json` 从“可用”推进到“生产级高吞吐 + 易排障 + 语义稳定”的状态，并给出可执行的 Phase 计划与验收标准。  
> 适用范围：所有 Northward 插件（Kafka/Pulsar/未来 MQTT/HTTP/WebSocket 等）复用的 SDK `mapped_json` 能力（`ng-gateway-sdk`）。

---

## 1. 背景与现状审计（代码与文档契约对齐）

### 1.1 当前实现位置（以 SDK 为中心）

- **映射引擎**：`ng-gateway-sdk/src/northward/mapping.rs`
  - `MappedJsonSpec` / `MappedRule`
  - `CompiledMappedJson::compile()`：编译 JMESPath 并预解析 `out_path` segments
  - `CompiledMappedJson::apply()`：执行映射并写入输出 JSON
  - `CompiledMappedJson::apply_lossy()`：容错执行（eval 失败写 `null`，写入冲突忽略）
  - `build_mapping_input()`：构造 canonical input view（跨插件稳定输入）
- **Uplink 接入**：`ng-gateway-sdk/src/northward/payload.rs`
  - `UplinkPayloadConfig::MappedJson { config: BTreeMap<String,String> }`
  - `encode_uplink_payload*_mapped_json*()`：目前每条消息都会把 map 变成 rules 并 `compile()`
- **Downlink 接入**：`ng-gateway-sdk/src/northward/downlink.rs`
  - `DownlinkPayloadConfig::MappedJson { config, filter }`
  - `decode_mapped()`：filter 通过后，目前每条消息都会 `compile()`

### 1.2 文档与代码的“契约承诺”

UI 文档明确对用户承诺：

- `mapped_json` 是产品级声明式映射，强调 **“compile once, apply many”**  
  - `ng-gateway-ui/docs/src/northward/payload/mapped-json.md`
- 输入视图（canonical input）是跨插件稳定的：
  - `{ schema_version, event_kind, ts_ms, app:{...}, device:{...}, data:{...} }`

SDK `mapping.rs` 的模块注释也承诺：

- “Compile expressions once, apply many times”
- “Pre-parse output paths to minimize per-message overhead”

### 1.3 现状与契约的偏差（必须修复）

当前 Uplink/Downlink 在热路径上存在**决定性性能问题**：

- **每条消息都会执行**：
  - 从 `BTreeMap<String,String>` 生成 `Vec<MappedRule>`（涉及字符串 clone）
  - `CompiledMappedJson::compile()`（JMESPath compile + segments 解析 + heap 分配）

这与“compile once, apply many”的承诺冲突，并且会在高吞吐场景造成：

- CPU 显著浪费（JMESPath compile 远比 apply 昂贵）
- 更高尾延迟（p99/p999 抖动）
- 配置错误无法早失败（直到运行时收到第一条消息才暴露 compile 错误）

---

## 2. 设计目标与非目标

### 2.1 设计目标（必须满足）

- **语义正确（Correctness）**
  - Uplink/Downlink 的 mapping 结果在同一配置下必须稳定可预测
  - Downlink 的 mapping 必须倾向于“严格”（避免错误映射生成错误控制指令）
- **高吞吐（Performance）**
  - 热路径仅做：构造 input（必要时）+ JMESPath search + JSON 写入
  - compile 必须移出每条消息的处理链路
- **可观测/可排障（Observability & Debuggability）**
  - compile/eval/out_path 冲突等错误需要包含足够上下文（至少包含 rule 的 out_path）
  - 支持指标与采样日志（不泄漏敏感 payload）
- **兼容性（Compatibility）**
  - 不破坏现有配置形状（`config: Map<out_path, expr>`）
  - 平滑迁移：旧配置无需变更即可获得性能收益
- **最小侵入（Minimal Changes）**
  - 优先在 SDK 提供“预编译/Prepared”接口，让插件侧做少量改动即可落地

### 2.2 非目标（当前阶段不做）

- 引入 Lua Sandbox（另有设计文档：`document/ng-lua-transform-sandbox-design.md`）
- 在 `mapped_json` 内引入更复杂 DSL（例如自定义函数、模板渲染等）
- 引入磁盘持久化队列、断网续传（不属于 mapping 范畴）

---

## 3. 关键概念与语义定义（必须写死）

### 3.1 Canonical Input View（输入视图）语义

#### 3.1.1 形状（schema_version = 1）

```json
{
  "schema_version": 1,
  "event_kind": "telemetry",
  "ts_ms": 1734870900000,
  "app": { "id": 1, "name": "my-app", "plugin_type": "kafka" },
  "device": { "id": 1001, "name": "dev-1", "type": null },
  "data": { }
}
```

#### 3.1.2 稳定性承诺

- **稳定字段（强承诺）**：`schema_version`、`event_kind`、`ts_ms`、`app.*`、`device.*`、`data`
- **`data` 的稳定性**：`data` 直接来自 `NorthwardData` 的 serde JSON 形状
  - 对外承诺：`NorthwardData` 的字段可能演进，但会在文档中维护“真实示例输入”
  - 对内原则：尽量只追加字段，不随意重命名/删除；如需破坏性变更必须 bump `schema_version`

#### 3.1.3 重要约束

- `mapped_json` 的表达式只允许读取这个 input view，不允许访问插件内部状态或执行 I/O。
- `schema_version` 必须在未来演进中保证向后兼容策略明确：
  - 规则：**新字段追加不 bump；破坏性变更必须 bump**。

### 3.2 配置形状与“顺序语义”澄清

当前配置为 JSON Map（对象）：

```json
{
  "out.path": "expr",
  "out2.path": "expr2"
}
```

语义约束：

- **不承诺用户输入的 key 顺序**（JSON object 无序；即使 Rust 使用 `BTreeMap` 也只是按 key 排序）
- 但映射行为必须保证在同一份 `config` 下**输出稳定**：
  - 规则建议：禁止/避免对同一路径或重叠路径重复写入（如 `a` 与 `a.b`）
  - 若发生冲突：由 `OutPathConflictPolicy` 决定行为（见下）

> 设计建议（Phase 2/3 才考虑）：可引入新配置形状 `rules: Vec<MappedRule>`（保序），同时保留 map 兼容解析，以满足“有顺序需求的高级用户”。

### 3.3 Output Path 写入与冲突策略

#### 3.3.1 输出路径（out_path）

- 采用 `.` 分隔的段路径，例如：
  - `payload.data.point_id`
  - `meta.type`

不支持的形态（当前不做）：

- 数组下标写入：`a[0].b`（避免复杂性与歧义）
- 转义点号：`a\.b`（可在未来扩展）

#### 3.3.2 冲突策略（OutPathConflictPolicy）

冲突示例：

- 先写 `a = 1`（a 是 number）
- 再写 `a.b = 2`（要求 a 是 object）

策略定义：

- **Overwrite（默认宽松）**
  - 行为：当某段路径需要 object 但当前是非 object 时，直接覆盖为 object 再继续写入
  - 适用：uplink 数据面、允许“尽量产出一些 JSON”而不是失败
  - 风险：可能掩盖配置错误；输出可能与用户预期不一致
- **Error（严格）**
  - 行为：发生类型冲突时直接报错（返回 `MappingError::OutPath`）
  - 适用：downlink 控制面（WritePoint/Command/RpcResponse），避免错误映射产生错误指令

强制最佳实践（本设计建议）：

- **Downlink 默认使用 `Error`**
- **Uplink 默认使用 `Overwrite`**，但应允许用户/插件在未来切换为 `Error`

### 3.4 执行语义：strict vs lossy

当前引擎提供两种执行语义：

- `apply()`：严格模式（eval/out_path 冲突即失败）
- `apply_lossy()`：容错模式（eval 失败写 `null`，写入冲突忽略）

最佳实践定义：

- **Uplink（数据面）**
  - 默认：`apply()`（让错误可见，便于早发现）
  - 可选：`apply_lossy()`（强需求：宁可产出部分字段也不要中断）
  - 若启用 lossy：必须配套“失败计数 + 抽样日志”，避免 silent failure
- **Downlink（控制面）**
  - 默认且强烈建议：只允许 `apply()`（严格）
  - 禁止默认 lossy：因为 `null`/缺字段可能通过反序列化为默认值或导致业务误判

---

## 4. 性能瓶颈分析（为什么必须做 compile 生命周期改造）

### 4.1 热路径成本拆解

以当前实现为基线，每条消息的 `mapped_json` 处理链路包含：

- 构造 input view：`build_mapping_input()`（序列化 `NorthwardData` 并拼 meta）
- 将 config map 转换为 rules vec（字符串 clone + vec 分配）
- 逐条规则编译 JMESPath（昂贵）并解析 out_path segments（中等）
- apply：对每条规则 `Expression::search()` + `serde_json::to_value()` + set_out_path

其中 **compile** 是显著高于 apply 的昂贵步骤，且完全可以按配置复用。

### 4.2 设计原则：把“昂贵且可复用”的工作移出热路径

必须满足：

- **compile 必须只在配置变更时发生**
- 热路径只做：
  - apply（搜索 + 写入）
  - 以及必要的 input view 构造（无法避免，但可优化）

---

## 5. 总体架构设计：Prepared/Precompiled Mapping 生命周期

### 5.1 核心思想

引入“预编译（Prepared/Precompiled）”概念，把 `config -> CompiledMappedJson` 的过程放在：

- App/Plugin 启动或配置加载阶段
- 或 UI 保存配置后的校验阶段（如果有专门校验 pipeline）

而不是每条消息执行阶段。

### 5.2 推荐的生命周期分层（不绑定具体插件实现）

#### 5.2.1 Uplink（Gateway -> Broker）

数据流（建议）：

```text
config load / update
  └─ compile mapped_json once
      └─ store in plugin/app state

process_data() hot path
  └─ build_mapping_input()
  └─ compiled.apply(&input)
  └─ serialize to bytes
  └─ try_send to outbound queue (I/O in publisher task)
```

#### 5.2.2 Downlink（Broker -> Gateway）

数据流（建议）：

```text
config load / update
  └─ build_route_table()
      └─ for each route: compile mapped_json once (strict policy)

consumer hot path
  └─ parse json bytes
  └─ filter match?
      └─ compiled.apply(&input)
      └─ deserialize to typed event (WritePoint/Command/RpcResponse)
```

### 5.3 SDK API 形状（建议）

目标：插件侧做最小改动即可“compile once, apply many”，并且不引入新的重依赖。

建议在 `ng-gateway-sdk` 增加少量辅助 API/类型（示意，非最终签名）：

```rust
/// A prepared (precompiled) mapped_json instance ready for hot-path apply.
///
/// English notes:
/// - This struct must be cheap to clone (use `Arc` internally).
/// - All expensive work (JMESPath compile, out_path parse) happens in `prepare_*`.
pub struct PreparedMappedJson {
    pub compiled: CompiledMappedJson,
}

/// Prepare uplink mapping with tolerant overwrite by default.
pub fn prepare_uplink_mapped_json(cfg: &MappedJsonConfig) -> Result<PreparedMappedJson, MappingError>;

/// Prepare downlink mapping with strict error-on-conflict.
pub fn prepare_downlink_mapped_json(cfg: &MappedJsonConfig) -> Result<PreparedMappedJson, MappingError>;
```

说明：

- `PreparedMappedJson` 是对 `CompiledMappedJson` 的薄封装，目的是把“默认策略”固化到 API
- 插件也可以直接调用 `CompiledMappedJson::compile_with_policy(...)`，但容易在不同插件里产生不同默认值

### 5.4 兼容性策略（必须）

- `UplinkPayloadConfig::MappedJson { config }` 不变
- `DownlinkPayloadConfig::MappedJson { config, filter }` 不变
- 新增的 prepared/compiled 结构只存在于运行时内存，不进入配置序列化

---

## 6. 语义与安全：为何 Downlink 必须默认严格

### 6.1 控制面错误的代价远高于数据面

Downlink 映射的结果会进入：

- `WritePoint`（直接写设备点位）
- `Command`（触发命令）
- `RpcResponse`（影响上层控制闭环）

因此必须遵循：

- **错误要尽早失败**
- **冲突要明确报错**
- **不能 silent overwrite / silent null**

### 6.2 推荐默认策略总结

- Downlink：
  - `OutPathConflictPolicy::Error`
  - 只允许 strict `apply()`
  - filter 未命中返回 `Ok(None)`（已实现，保持）
- Uplink：
  - `OutPathConflictPolicy::Overwrite`（默认）
  - 默认 strict `apply()`，可按需提供 lossy（且要观测）

---

## 7. 错误语义与可观测性（必须补齐上下文）

### 7.1 当前错误类型不足

`MappingError::Eval` / `Compile` 目前仅包含 `expr`（和 error string），排障时无法快速定位：

- 是哪条 rule？
- out_path 是什么？
- 是 compile 阶段还是 eval 阶段？

### 7.2 建议的错误上下文（不破坏外部 API）

建议把 `out_path` 也纳入错误结构（示意）：

```rust
pub enum MappingError {
    InvalidRule(String),
    Compile { out_path: String, expr: String, error: String },
    Eval { out_path: String, expr: String, error: String },
    OutPath { path: String, error: String },
}
```

最佳实践：

- 错误信息必须可直接用于日志与 UI 展示（短、可读）
- 不能泄漏 payload 内容

### 7.3 指标与日志（建议最小集合）

指标维度（labels）必须控制基数（cardinality）。建议 labels：

- `plugin_type`
- `direction`（uplink/downlink）
- `event_kind`
- `mode`（mapped_json）

不建议默认使用 `app_id` / `device_id` 作为 label（基数过高）；如确有需要，用 sampling 日志替代。

建议指标：

- counters
  - `ng_mapped_json_apply_total`
  - `ng_mapped_json_apply_ok_total`
  - `ng_mapped_json_apply_error_total`
  - `ng_mapped_json_compile_total`
  - `ng_mapped_json_compile_error_total`
- histogram
  - `ng_mapped_json_apply_duration_ms`
  - `ng_mapped_json_compile_duration_ms`

日志建议（采样）：

- compile error：必须记录 `plugin_type + direction + event_kind + out_path + expr`（expr 可截断）
- apply error：记录同上 + error_kind（eval/out_path）
- 对于高频 apply error：必须采样（例如每 N 秒最多 1 条），避免打爆日志

---

## 8. `build_mapping_input()` 的性能优化建议（次优先级，但建议做）

### 8.1 现状问题

当前 `build_mapping_input()` 构造过程中会产生多次 `String` 分配：

- device_name clone / app_name to_string / plugin_type to_string 等
- 再通过 `serde_json::to_value()` 进入 `Value`，会再次持有字符串

这不是决定性瓶颈，但在极高吞吐下仍然可观。

### 8.2 优化方向（不破坏语义）

- 使用借用字段（`&str`）或 `Arc<str>` 直接序列化，减少中间 `String`
- 对于 `NorthwardData::Telemetry/Attributes` 这种高频类型，尽量避免构造临时字符串
- 明确 `ts_ms` 的计算策略（现在 DeviceConnected/Disconnected 使用 `Utc::now()`，其他使用 data 内 timestamp；保持即可）

---

## 9. 兼容性与迁移策略（必须写清）

### 9.1 配置兼容

- 配置 JSON 不变
- UI schema 不变（除非未来引入 ordered rules 形态）

### 9.2 行为兼容（可能的“行为变化点”）

当你把 Downlink 默认切到严格策略后，可能出现：

- 以前 silent overwrite 的冲突现在会报错（这是期望的安全提升）

因此：

- Phase 1 中建议只对 Downlink 的 `MappedJson` 使用 strict policy（如果你担心兼容，可提供一个开关，默认 strict，允许回退）

---

## 10. 测试与压测计划（必须能量化收益）

### 10.1 单元测试（SDK）

覆盖点：

- compile 成功/失败（空 out_path、空 expr、非法 jmespath）
- apply eval error（表达式在输入上 search 失败）
- out_path 冲突：Overwrite vs Error 两种策略行为
- apply_lossy：eval error 写 null；冲突忽略写入

### 10.2 集成测试（插件侧或 SDK tests）

现有参考：`ng-gateway-northward/pulsar/tests/mapped_json_test.rs`

需要新增/增强：

- “compile once” 验证（可通过注入计数或基准测试证明 compile 不在热路径）
- Downlink filter + mapped_json：filter 不匹配必须 `Ok(None)` 且不计为 error

### 10.3 基准测试（criterion 或自研 micro-bench）

建议两个场景：

- **Before**：每条消息 compile + apply
- **After**：启动时 compile 一次 + 每条消息 apply

指标：

- 单核吞吐（ops/s）
- p50/p99 延迟（ns 或 us）
- 分配次数/字节（可用 jemalloc/heaptrack/pprof 辅助）

验收标准建议：

- 在典型规则数（例如 10/30/100 rules）下，After 相比 Before 至少提升一个数量级的性能（通常可以做到）
- 尾延迟显著下降（p99 至少下降 50% 以上，视规则复杂度而定）

---

## 11. Phase 推进计划（越细越好，可直接落地）

### Phase 0：基线与护栏（1-2 天）

目标：在不改动行为的前提下，补齐观测与测试护栏，避免后续优化“盲改”。

- 工作项
  - 增加/补齐 `mapping.rs` 单元测试（冲突策略、eval error、lossy 行为）
  - 选定基准测试方案（criterion 或简单 bench）
  - 明确指标命名与 labels 策略（避免高基数）
- 验收标准
  - CI 通过
  - 至少有一个可重复的性能基线数据（Before）
- 回滚策略
  - 仅新增测试/基准/指标定义，回滚无风险

### Phase 1：语义正确 + “compile once, apply many” 落地（高优先级，2-5 天）

目标：把 compile 从每条消息移出热路径，并固化 Downlink 严格策略。

- 工作项（Uplink）
  - 在插件或 core loader 侧：对 `UplinkPayloadConfig::MappedJson` 预编译并缓存
  - 热路径只调用 `compiled.apply(...)`
- 工作项（Downlink）
  - 在 `build_route_table` 或 route 构建阶段：对每个 `MappedJson` route 预编译并缓存
  - 默认使用 `OutPathConflictPolicy::Error`
  - 热路径只做 json parse + filter + apply + typed deserialize
- 验收标准
  - 功能测试通过（uplink/downlink）
  - compile 错误能在启动/加载配置阶段暴露（而非第一条消息）
  - 性能基准：After 相比 Before 明显提升（至少数倍，通常数量级）
- 风险点
  - 引入缓存后的生命周期管理（配置热更新时要重新编译并替换）
- 回滚策略
  - 保留旧路径开关（例如 feature flag 或临时配置），遇到问题可快速回退到“每条消息 compile”（不推荐长期保留）

### Phase 2：可观测性与排障增强（中优先级，2-4 天）

目标：让 mapping 失败“可定位、可统计、可限流”。

- 工作项
  - `MappingError` 增强：带 `out_path` 上下文
  - 采样日志：高频 apply error 不刷屏
  - 指标落地：compile/apply 成功/失败与耗时
- 验收标准
  - 用户能在日志里直接看到是哪个 out_path 的 expr 出错
  - 指标可在 metrics endpoint 观察到趋势
- 回滚策略
  - 错误类型变更若影响外部依赖，需提供 `Display` 兼容；否则回滚到旧错误结构

### Phase 3：输入构造与配置语义演进（低优先级，按需）

目标：进一步减少分配、补齐高级用户需求（有序 rules）。

- 可选工作项 A：`build_mapping_input()` 分配优化
  - 用借用/`Arc<str>` 结构减少中间 `String`
- 可选工作项 B：配置形态扩展（保序规则）
  - 新增 `rules: Vec<MappedRule>` 形态（保序）
  - 兼容旧 map：map -> rules（按 key 排序或明确固定规则）
- 验收标准
  - 性能继续提升（尤其在超高吞吐下）
  - 文档明确“顺序语义”与迁移方式
- 回滚策略
  - 新配置形态必须向后兼容；若出现 UI/序列化问题，可仅保留 map 形态

---

## 12. 风险清单与工程化建议

### 12.1 风险：缓存引入的“配置热更新一致性”

如果支持运行时更新配置：

- 必须确保：
  - 新配置先 compile 成功，再原子替换旧 compiled
  - compile 失败不影响旧配置继续工作（保守策略）

### 12.2 风险：Downlink 严格策略引发“以前能跑现在报错”

这是安全提升，但要让用户可理解：

- 文档中明确提示冲突案例
- UI/日志清晰提示 out_path 冲突原因

### 12.3 工程化建议：把“默认策略”收敛到 SDK

避免不同插件默认值不一致：

- 通过 SDK 提供 `prepare_uplink_mapped_json()` / `prepare_downlink_mapped_json()`（或类似）统一默认策略

---

## 13. 变更总结（本文档输出的结论）

- `mapped_json` 应保留并继续作为“产品级声明式映射”能力，但**必须修复 compile 生命周期**，否则吞吐与尾延迟会严重受损。
- **Downlink 默认必须严格**（冲突报错、禁用默认 lossy），以保障控制面安全与语义正确。
- 推荐用 Phase 方式推进：Phase 1 先把 compile 移出热路径并落地严格策略；Phase 2 再补齐错误上下文与指标；Phase 3 再做输入构造与配置语义演进。


