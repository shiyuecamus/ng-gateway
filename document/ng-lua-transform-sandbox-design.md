## ng-lua-transform：Lua 脚本 Transform（沙箱 + 生产级风险控制）在现有代码结构下的落地设计（SDK 抽象）

> 目标：在 **`ng-gateway-sdk`** 层提供一套可复用的 Lua Transform 能力，供 **Kafka/Pulsar/未来 MQTT 等北向插件**统一使用。  
> 约束：遵循当前代码架构（`process_data()` 尽量 CPU-only + 有界队列背压，I/O 下沉到任务），遵循现有配置/metadata/UI schema 组织方式（`payload.mode + UnionCase`）。

---

## 1. 现状对齐（基于当前仓库代码）

### 1.1 现有北向插件的关键结构

- **Kafka/Pulsar 插件**在 `process_data()` 内做：
  - `build_context()`（SDK）
  - `render_template()`（SDK，Handlebars + cache，模板语法 `{{var}}`）
  - `encode_uplink_payload()`（SDK，`envelope_json|kv|timeseries_rows|mapped_json`）
  - 然后 `try_send()` 到插件内部 **有界 outbound 队列**，由 publisher task 负责 I/O
- **Downlink**：Kafka/Pulsar supervisor 侧通过 SDK `decode_event()` 解析消息，并依据：
  - `AckPolicy`（OnSuccess/Always/Never）
  - `FailurePolicy`（Drop/Error）
    来决定 commit/ack 行为

### 1.2 SDK 里已经抽象出的“可复用骨架”

- **Topic/Key 模板**：`ng-gateway-sdk/src/northward/template.rs`（Handlebars + `default` helper）
- **mapped_json**：`ng-gateway-sdk/src/northward/mapping.rs`（JMESPath 编译 + 热路径 apply + 统一输入视图 `build_mapping_input()`）
- **payload 编码**：`ng-gateway-sdk/src/northward/payload.rs`
- **downlink 解码与路由**：`ng-gateway-sdk/src/northward/downlink.rs`

> 结论：Lua Transform 应该以“新增 payload.mode=lua”的方式进入 SDK，并复用现有 metadata 组织（`payload.mode + UnionCase`），做到**最小侵入**与**跨插件复用**。

---

## 2. 设计目标与非目标

### 2.1 设计目标（必须满足）

- **生产级安全**：
  - 可靠中断：必须支持 **指令步数限制** + **deadline 超时**（不能只依赖 `tokio::time::timeout`）
  - 环境隔离：每个脚本独立 `_ENV`（不污染全局）
  - 能力最小化：不暴露 `os/io/package/debug/require/dofile/loadfile/load/collectgarbage` 等
  - 输入/输出净化：只允许 JSON 兼容类型，并做结构/大小上限
- **高吞吐友好**：
  - 默认在热路径上尽量少分配、可缓存编译结果
  - 提供可选的执行隔离（worker thread + 有界队列），避免慢脚本拖垮 `process_data()` 串行链路
- **可观测/可排错**：
  - 指标：成功/失败/超时/指令超限/输入输出超限等计数与耗时
  - 日志：错误摘要可见但不泄漏敏感 payload
- **统一抽象**：
  - Kafka/Pulsar/MQTT 等插件使用同一套类型与执行器
  - UI schema 能统一渲染（脚本编辑器、限制项、开关）

### 2.2 非目标（本设计暂不做）

- 远程动态下发脚本 / 热更新分发系统（后续可扩展）
- 从插件 DLL 动态加载前端组件（UI widget 仅由 UI 仓库内置）
- LuaJIT 优化（先以 `mlua + lua54` 达到一致性与可移植性）

---

## 3. SDK 层抽象：模块、类型与边界

### 3.1 模块规划（建议）

在 `ng-gateway-sdk` 增加：

- `ng-gateway-sdk/src/northward/lua/mod.rs`
  - `LuaScriptConfig` / `LuaLimits` / `LuaSandboxConfig`
  - `LuaTransformDirection` / `LuaTransformInput` / `LuaTransformOutput`
  - `LuaTransformEngine`（编译/缓存/执行）
  - `LuaTransformExecutor`（可选：线程隔离执行）
  - `LuaTransformError`（错误分类 + 可观测字段）

并在现有模块扩展：

- `ng-gateway-sdk/src/northward/payload.rs`
  - `UplinkPayloadConfig` 新增 `Lua { ... }`
  - `encode_uplink_payload()` 支持 `lua` 分支
- `ng-gateway-sdk/src/northward/downlink.rs`
  - `DownlinkPayloadConfig` 新增 `Lua { ... }`
  - `decode_event()` 支持 `lua` 分支
- `ng-gateway-sdk/src/northward/codec.rs`
  - `EncodeError` / `DecodeError` 增补 Lua 相关错误（或做 `From<LuaTransformError>` 转换）

> 重要：Kafka/Pulsar 插件的 `config.rs` 当前 **直接 re-export SDK 的 payload/downlink 类型**，因此 SDK 增量后，插件 config 不需要重建结构，只需要更新 `metadata.rs` 让 UI 能配置到新字段。

---

## 4. Lua 输入/输出契约（强约束，跨插件稳定）

### 4.1 基本原则

- Lua 脚本的输入/输出必须与 JSON 同构（`null/bool/number/string/array/object`）
- Rust 侧输入来源必须是 **`serde_json::Value`**，严禁用 `lua.load(json_string)` 解析 JSON（会变成执行 Lua 代码）
- 输出必须经过净化（禁止 function/userdata/thread/lightuserdata / metatable 等）

### 4.2 TransformInput（统一形状）

建议固定为：

- `direction`: `"uplink" | "downlink"`
- `meta`（稳定 envelope 元信息）：
  - `schema_version`（固定 1，后续可演进）
  - `app`: `{ id, name, plugin_type }`
  - `device`: `{ id, name, type? }`（uplink 必有；downlink 可选）
  - `event_kind`: 对齐 `EnvelopeKind`（uplink：来自 `NorthwardData::envelope_kind()`；downlink：来自配置槽位）
  - `ts_ms`
- `message`（本次消息上下文）：
  - `topic`（渲染后的 topic；downlink 为实际接收 topic）
  - `key`（渲染后的 key；downlink 为消息 key/partition_key）
  - `properties`（k/v，Kafka headers / Pulsar properties）
  - `content_type`（可选，未来 MQTT/HTTP 复用）
- `data`：
  - uplink：`NorthwardData` 的 canonical JSON（建议直接 `serde_json::to_value(data)`）
  - downlink：`{ raw: { kind="bytes_base64", value="..." }, json?: <parsed json if parse ok> }`

### 4.3 TransformOutput（统一形状）

建议固定为：

- `status`: `"ok" | "drop" | "error"`
- `error?`: `{ message: string, classification: "permanent" | "transient" }`
- **uplink 输出**（仅当 direction=uplink）：
  - `publish?`:
    - `payload`: `{ kind: "utf8" | "bytes_base64", value: string }`
    - `content_type?`: string
    - `headers_delta?`: map<string,string>
    - `topic_override?` / `key_override?`（默认禁用；见 7.3 扩展）
- **downlink 输出**（仅当 direction=downlink）：
  - `emit_event?`:
    - `data`: object（目标结构的 JSON：WritePoint / Command / ServerRpcResponse）
    - `ignore?`: bool（可选：脚本显式忽略该消息）

> 说明：由于当前 downlink 路由是“按槽位固定目标类型”，因此脚本输出的 `emit_event.data` 会被 Rust 侧按槽位反序列化为固定类型，避免脚本随意构造不受控事件。

---

## 5. 沙箱设计（生产级护栏）

### 5.1 Lua runtime 选择

- 推荐 `mlua`，启用 `lua54`（一致性优先）
- 是否启用 `send`/多线程特性需谨慎评估（Lua 状态共享的并发安全）

### 5.2 环境隔离（每脚本独立 `_ENV`）

- **不要**在 `lua.globals()` 上删库（会影响其它脚本/实例）
- 对每个脚本创建独立 `env`，通过 `Chunk::set_environment(env)` 加载
- `env` 只注入白名单：
  - base：`assert/error/type/tostring/tonumber/pairs/ipairs/next`
  - `math/string/table`（移除 `string.dump`）
- 禁止注入：
  - `os/io/package/debug`
  - `require/dofile/loadfile/load/collectgarbage`
  - `print`（可用 `ng.log` 替代）
- `setmetatable/getmetatable` 默认禁用（需要时可由配置开关放行）

### 5.3 可靠中断（必须）

必须实现 **hook**：

- `max_instructions`：每 N 条指令触发一次 hook，累计超限立即报错中断
- `timeout_ms`：hook 内检查 `Instant::now() > deadline`，超时立即报错中断

> 关键点：`tokio::time::timeout` **无法打断** Lua 死循环，只会让 await 返回；CPU 仍会跑。必须用 hook。

### 5.4 资源上限（DoS 防护）

强制执行：

- `max_input_bytes`：输入 JSON（或 raw bytes）超过即拒绝
- `max_output_bytes`：输出 payload 超过即拒绝
- `max_string_bytes`：Lua string 导出时限制
- `max_table_items`：table 导出时限制
- `max_depth`：递归深度限制（输入转 Lua / 输出转 JSON）

可选（依赖 runtime 支持）：

- memory limit / stack limit（如 Lua runtime 或 mlua API 支持）

### 5.5 Host API（只开放最小集合）

注入只读 `ng` 表（建议）：

- `ng.log(level, msg)`（level 限定：debug/info/warn/error）
- `ng.now_ms() -> integer`
- `ng.sha256(str) -> hex`（可选，若已有 SDK hash 工具可复用）
- `ng.render_template(template, ctx)`（可选，谨慎：避免让脚本二次引入复杂模板渲染成本）

禁止：

- 任意网络请求
- 文件读写
- 系统命令

---

## 6. 执行模型与性能设计（与现有插件架构对齐）

### 6.1 两种执行模式（建议都支持）

#### A) Inline（最简单，默认可用）

- 在 `encode_uplink_payload()` / `decode_event()` 内直接执行 Lua
- 优点：实现简单，改动小
- 缺点：`process_data()` 串行链路上会额外消耗 CPU（虽然有 timeout/指令限制，但吞吐会受影响）

#### B) Executor（推荐生产默认，隔离慢脚本）

在 SDK 提供 `LuaTransformExecutor`：

- 内部是 **有界队列** + **固定数量 worker thread**
- 每个 worker thread 持有独立 Lua VM（避免跨线程共享状态）
- job 模型：`job = { script_id, input_value, reply_oneshot }`
- backpressure：
  - `try_submit()`：队列满则返回错误，让调用者按策略 drop/error（与现有 outbound 队列一致）
  - （可选）`submit().await`：允许等待，但应避免在 hot path 使用

> 这与现有 Kafka/Pulsar 的 I/O publisher task 模式一致：把“可能慢/不稳定”的部分从 `process_data()` 分离出去，并通过 bounded queue + structured concurrency 控制资源。

### 6.2 编译缓存策略

建议 SDK `LuaTransformEngine` 支持：

- `start()` 时预编译（如果脚本是 inline/path 且可读取）
- 运行时按 `script_id = sha256(script_source + entrypoint + sandbox_flags)` 做缓存 key
- 缓存内容：`CompiledChunk + entrypoint_function_ref + env`
- 缓存大小上限（LRU / 计数上限），避免脚本数量增长导致内存膨胀

### 6.3 输出与 topic/key/header 的可控性

强烈建议默认策略：

- **默认禁止**脚本覆盖 `topic/key`（避免把路由逻辑交给脚本导致不可控）
- **默认禁止**脚本直接设置任意 headers/properties（只允许 `headers_delta`，且 key 白名单可配置）
- 默认只允许脚本决定：
  - 输出 payload（utf8 / bytes_base64）
  - `drop`（丢弃，不发送 / 不转发）
  - `error`（失败，交由 failure policy 处理）

可扩展策略（Phase 3）：

- `allow_topic_override: bool`（默认 false）
- `allow_key_override: bool`（默认 false）
- `allowed_header_keys: Vec<String>`（默认仅允许 `content-type` 或者完全禁止）

---

## 7. 配置模型（SDK 类型扩展 + 插件复用）

### 7.1 SDK：Lua 配置类型（建议）

- `LuaScriptConfig`
  - `inline: Option<String>`
  - `path: Option<String>`
  - `entrypoint: Option<String>`（默认 `"transform"`）
- `LuaLimits`（默认值必须偏保守，确保不会拖垮网关）
  - `timeout_ms: u64`（建议默认 20~50ms）
  - `max_instructions: u64`（建议默认 200_000）
  - `max_input_bytes: u64`（建议默认 256KiB）
  - `max_output_bytes: u64`（建议默认 1MiB）
  - `max_string_bytes: u64`（建议默认 256KiB）
  - `max_table_items: u64`（建议默认 10_000）
  - `max_depth: u32`（建议默认 32）
- `LuaSandboxConfig`
  - `allow_setmetatable: bool`（默认 false）
  - `allow_getmetatable: bool`（默认 false）
  - `allow_template_render: bool`（默认 false）
  - `allow_sha256: bool`（默认 true）
  - `log_level_cap: "info" | "warn" | "error"`（默认 "warn"）
- `LuaFailurePolicy`
  - `drop | error`
  - `classify_transient_as_error: bool`（默认 true，用于区分 transient/permanent 的处理语义）

> 注：`LuaFailurePolicy` 建议只出现在 payload 配置里（更贴近“映射失败时的行为”），而不是复用 downlink 的 `FailurePolicy`，因为 Lua 有 `transient/permanent` 分类，语义更细。

### 7.2 SDK：payload/downlink 枚举扩展（建议最小化）

在 `ng-gateway-sdk::northward::payload::UplinkPayloadConfig` 增加：

- `Lua { script: LuaScriptConfig, #[serde(default)] limits: LuaLimits, #[serde(default)] sandbox: LuaSandboxConfig, #[serde(default)] failure_policy: LuaFailurePolicy }`

在 `ng-gateway-sdk::northward::downlink::DownlinkPayloadConfig` 增加：

- `Lua { script: LuaScriptConfig, #[serde(default)] limits: LuaLimits, #[serde(default)] sandbox: LuaSandboxConfig, #[serde(default)] filter: MappedDownlinkFilterConfig }`

> 说明：downlink 的 filter 可以直接复用现有 `MappedDownlinkFilterConfig`（JsonPointer/Property/Key/None），这样 Kafka/Pulsar 多路由共用一个 topic 时，Lua 也能避免“误处理其它事件”导致的噪声错误。

### 7.3 script.path 的定位策略（必须统一）

需要在 SDK 层定义一个明确策略，否则会出现“不同插件/不同部署环境路径不一致”的运维坑：

- **推荐策略**：`script.path` 为相对路径时，以“应用(app_id)的 northward 插件工作目录”为基准解析
  - 例如：`$NG_DATA_DIR/apps/{app_id}/northward/{plugin_type}/scripts/...`
- **备选策略**：相对 `gateway.toml` 所在目录
- **必须支持**：`inline` 与 `path` 二选一；同时存在时优先 `inline`

（见文末“待确认问题 Q1”）

---

## 8. SDK 执行器 API 形状（建议）

下面给出建议的 API 轮廓（示意，非最终签名；示例代码仅用于表达接口语义）：

```rust
/// Build and run a production-grade Lua transform with strict sandboxing.
///
/// English notes:
/// - Never use `lua.load(json_string)` for JSON input.
/// - Always enable instruction hook + deadline checks for reliable interruption.
pub struct LuaTransformEngine { /* ... */ }

impl LuaTransformEngine {
    /// Compile script and prepare isolated environment.
    pub fn compile(script: &LuaScriptConfig, sandbox: &LuaSandboxConfig) -> Result<Self, LuaTransformError> {
        // ...
        Ok(Self { /* ... */ })
    }

    /// Execute transform for one message. Enforces limits and sanitizes output.
    pub fn execute(&self, input: &serde_json::Value, limits: &LuaLimits) -> Result<serde_json::Value, LuaTransformError> {
        // ...
        Ok(serde_json::Value::Null)
    }
}

/// Optional executor that isolates Lua VMs in dedicated worker threads.
pub struct LuaTransformExecutor { /* bounded queue + workers */ }

impl LuaTransformExecutor {
    /// Try submit a job without awaiting (backpressure-friendly).
    pub fn try_submit(&self, job: LuaJob) -> Result<tokio::sync::oneshot::Receiver<LuaJobResult>, LuaTransformError> {
        // ...
        unimplemented!()
    }
}
```

> 落地时建议把 `LuaTransformEngine` 设计为 **无 async**（纯 CPU），executor 才负责把它与 tokio 结合。这样更容易测试与复用。

---

## 9. Kafka/Pulsar 插件侧集成方式（最小化改动）

### 9.1 Uplink（Gateway -> MQ）

#### 集成点

- 目前 Kafka/Pulsar 都调用 `encode_uplink_payload(&mapping.payload, ...)`
- SDK 扩展后：
  - 非 lua：保持现状
  - lua：走 `UplinkPayloadConfig::Lua` 分支

#### 推荐执行方式（生产默认）

为避免慢脚本拖垮 `process_data()` 串行链路，建议插件内部增加一个 **transform queue + transform worker**（与 outbound publisher 的结构同构）：

- `process_data()`：
  - 只做 `build_context + render_template + 构造 TransformInput`
  - `try_send()` 到 `transform_tx`（有界，满则返回 PublishFailed/QueueFull）
- `transform_task`（可由 SDK 提供通用封装，或插件内轻量实现）：
  - 从 `transform_rx` 取任务
  - 执行：
    - lua：调用 `LuaTransformExecutor` 或 inline engine（取决于配置）
    - 其它模式：可继续复用 `encode_uplink_payload()`（也可统一在 transform_task 做）
  - 得到 payload 后再 `try_send()` 到现有 `outbound_tx`（I/O task 不变）

这样可以做到：

- Lua 变慢只会占满 `transform` 队列，而不会卡住 `process_data()` 处理后续消息
- 通过 `transform` 队列容量实现“脚本侧背压”，与现有 outbound 队列背压一致

> 如果你更想保持极简：Phase 1 可以先做 inline；Phase 2 再引入 transform_task（见第 12 节路线）。

### 9.2 Downlink（MQ -> Gateway）

#### 集成点

- Kafka/Pulsar supervisor 侧已统一调用 SDK `decode_event(route, meta, bytes)`
- SDK 增加 `DownlinkPayloadConfig::Lua` 后，插件侧基本不用改 supervisor 逻辑

#### ack/failure 语义

- Lua decode 成功 -> `Ok(Some(ev))` -> forwarded=true -> ack/commit（OnSuccess）
- Lua filter 未命中或脚本显式 ignore -> `Ok(None)` -> ignored -> ok=true -> ack/commit（OnSuccess）
- Lua 执行/净化/反序列化失败 -> `Err(DecodeError::...)`
  - `FailurePolicy::Drop` -> commit（丢弃）
  - `FailurePolicy::Error` -> 不 commit（用于调试/需要重放的场景）

---

## 10. UI schema / metadata 扩展（现有结构增量演进）

### 10.1 现状：UiProps 尚无 widget

当前 `ng-gateway-sdk::UiProps` 只有 placeholder/help/prefix/suffix/col_span/read_only/disabled，**没有 widget** 字段。  
如果希望 UI 侧对 Lua script 提供代码编辑器体验（语法高亮/行号/大小提示），建议在 SDK 侧增量扩展：

- `UiProps` 增加：
  - `widget: Option<UiWidget>`
- `UiWidget`：
  - `name: String`（例如 `code_editor` / `northward.code_editor`）
  - `props: serde_json::Value`（例如 `{ "language": "lua", "height": 240 }`）

兼容策略：

- UI 不认识 widget 时必须回退到默认渲染（按 `UiDataType`）
- props 采取“前端识别自己认识的 key，其余忽略”的方式解耦

### 10.2 Kafka/Pulsar metadata.rs 的最小改动点

两者的 `metadata.rs` 都是：

- `payload.mode` enum items
- `payload` union cases（按 `mode` 映射 children）

因此增加 `lua` 的改动很集中：

- 在 uplink 的 `payload_mode_items` 增加 `lua`
- 在 uplink 的 `payload_union_cases` 增加 `lua` case，children 包含：
  - `script.inline`（String，建议带 widget=code_editor）
  - `script.path`（String）
  - `entrypoint`（String，默认 transform）
  - `limits.*`（Integer/Boolean）
  - `sandbox.*`（Boolean）
  - `failure_policy`（Enum：drop/error）
- 在 downlink 的 `payload_mode_items` 与 `payload_union_cases` 同理增加 `lua`
  - 另外可复用现有 filter 子 union（与 mapped_json 相同）

> 注意：目前 UI 的结构 gating 主要靠 `WhenEffect::Visible/Invisible`，而 `Union` 自身无 when；现有 Kafka/Pulsar metadata 已经把“enabled=false 时隐藏 payload 子字段”的最佳实践写好了，Lua case 只需照做即可。

---

## 11. 可观测性（指标 + 日志）

### 11.1 指标（建议最小集合）

以 `plugin_type/app_id/direction/event_kind/script_id` 为核心 label（注意 label cardinality，`script_id` 建议截断或哈希）：

- counters
  - `ng_lua_transform_total`
  - `ng_lua_transform_ok_total`
  - `ng_lua_transform_drop_total`
  - `ng_lua_transform_error_total`
  - `ng_lua_transform_timeout_total`
  - `ng_lua_transform_instruction_limit_total`
  - `ng_lua_transform_input_too_large_total`
  - `ng_lua_transform_output_too_large_total`
  - `ng_lua_transform_sanitize_error_total`
- histogram
  - `ng_lua_transform_duration_ms`

### 11.2 日志（建议）

- error 级别只记录摘要：
  - app_id / plugin_type / direction / event_kind / script_id
  - error_kind（timeout/instruction_limit/sanitize/compile/runtime）
  - message（截断）
- 不记录：
  - raw payload 全量
  - 可能包含隐私/密钥的 headers/properties

---

## 12. 推进路线（Phase 1/2/3）

### Phase 1（最小可用，优先）

- SDK：新增 `northward::lua` 模块（engine + sandbox + limits + sanitize）
- SDK：`UplinkPayloadConfig` 增加 `Lua`
- 插件：Kafka/Pulsar metadata 增加 `payload.mode=lua` 的 UI schema（仅 uplink）
- 执行模型：先支持 **Inline**（最快落地）
- 约束：禁止 topic/key 覆盖；只输出 payload；失败策略 drop/error

### Phase 2（生产推荐形态）

- SDK：提供 `LuaTransformExecutor`（bounded queue + worker threads）
- 插件：引入 `transform_task`（把 Lua 从 `process_data()` 串行链路隔离出去）
- Downlink：`DownlinkPayloadConfig` 增加 `Lua`，并接入 `decode_event()`
- filter：复用 `MappedDownlinkFilterConfig`

### Phase 3（高级能力）

- 可选允许：
  - topic/key override（默认仍禁用）
  - headers_delta 严格白名单
- 更强的资源隔离：
  - memory limit / stack limit（若 runtime 支持）
- 脚本分发与版本管理（可选，需额外系统设计）

---

## 13. 待确认问题（请你确认后再开始落地）

1. **Q1：`script.path` 的基准目录**

   - 你更希望相对哪个目录解析？
   - 推荐：`$NG_DATA_DIR/apps/{app_id}/northward/{plugin_type}/scripts/`（便于多 app、多插件隔离）

2. **Q2：Lua 默认执行模式**

   - Phase 1 是否允许先做 Inline（快），还是你希望一开始就上 Executor + transform_task（更生产）？

3. **Q3：是否允许脚本覆盖 topic/key/headers**

   - 我建议 Phase 1/2 全禁用；你是否有强需求必须允许？如果有，能否接受“白名单 + 默认关闭”？

4. **Q4：downlink Lua 输出范围**
   - 是否接受“按槽位固定目标类型”（write_point/command/rpc_response）？
   - 还是希望脚本能动态决定 emit_event.kind？（更灵活但更难控、也会影响路由与审计）
