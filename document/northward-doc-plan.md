# NG Gateway 北向插件文档（产品级）规划稿

> 目标：输出一套“语义正确、可交付、产品级”的北向（Northward）文档信息架构与页面设计，让用户能**按文档完成使用、配置、验证、排障与扩展**。
>
> 本规划稿基于当前代码实现进行约束（Kafka/Pulsar/ThingsBoard/OPC UA Server），对未实现/不支持的能力将明确标注，并统一指向路线图：`/guide/other/roadmap`（侧边栏配置：`ng-gateway-ui/docs/.vitepress/config/zh.mts` 第 204 行）。

---

## 0. 读者与范围

### 0.1 目标读者

- **集成工程师**：把网关数据上送到 Kafka/Pulsar/ThingsBoard 或对外暴露 OPC UA Server。
- **运维/现场工程师**：关心弱网、队列、重试、吞吐、告警、排障。
- **二次开发者**：要扩展插件、扩展 payload、做声明式映射或未来的 Lua Transform。

### 0.2 文档覆盖范围（本版本）

- **已实现并需要写成产品级**：
  - Kafka（uplink + downlink）
  - Pulsar（uplink + downlink）
  - ThingsBoard（uplink + RPC/attributes 等下行链路 + Provision + 凭据持久化）
  - OPC UA Server（把网关点位暴露为 OPC UA 节点 + 写回）
  - 通用：`RetryPolicy`、`QueuePolicy`、模板渲染（Handlebars）、payload（EnvelopeJson/Kv/TimeseriesRows/MappedJson）、downlink（EnvelopeJson/MappedJson + AckPolicy/FailurePolicy）
- **侧边栏已预留但当前 repo 未看到对应北向插件 crate 的条目**（需要在文档中明确“当前版本暂不支持/尚未落地”）：
  - MQTT / WebSocket / HTTP（`ng-gateway-ui/docs/.vitepress/config/zh.mts` 的北向菜单里存在，但 `ng-gateway-northward/` 下未发现相应 crate）

### 0.3 非目标（当前版本不承诺）

::: warning 当前版本限制（需要在文档中显式写清楚）
- **断网续传（磁盘 WAL / 持久化队列）**：core 目前只提供**内存 buffer**（`QueuePolicy.buffer_enabled`），无磁盘落盘与回放。
- **满队列精细化降级**：目前仅支持 `dropPolicy=Discard/Block`（core），以及 OPC UA Server 的 `DiscardOldest/DiscardNewest/BlockWithTimeout`（插件内更新队列）。未实现“时间采样、按 latest 合并、按窗口聚合、分级丢弃”等更复杂策略。
- **ThingsBoard Protobuf**：配置项存在但实现为 `TODO`，当前会返回错误“Protobuf format not yet implemented”。
:::

---

## 1. 现状能力清单（按实现落地）

> 本节用于决定“文档该怎么写”，不是最终给用户看的文档正文；落地时会把关键信息拆进各页面。

### 1.1 Core（AppActor）层面的通用语义

- **每个 Northward App 独立 Actor**：独立数据队列（Gateway→Plugin）与事件通道（Plugin→Gateway），互相隔离。
- **QueuePolicy（内存队列 + 可选内存 buffer）**：
  - `capacity`：Gateway→Plugin 主队列容量（bounded mpsc）。
  - `dropPolicy`：
    - `Discard`：队列满丢弃当前条。
    - `Block`：阻塞等待 `blockDuration`，超时则丢弃。
  - `bufferEnabled/bufferCapacity/bufferExpireMs`：当插件未连接时，数据进入**内存 FIFO buffer**；连接恢复时尝试 flush 到主队列（flush 时如果主队列满，会把剩余数据重新放回 buffer）。
  - **无磁盘 WAL**：buffer 只在内存，重启/掉电会丢失。
- **RetryPolicy**：用于插件 supervisor 的连接重试/退避（Kafka/Pulsar/ThingsBoard 均使用 gateway-level backoff 构建器）。

### 1.2 通用 payload 与模板（SDK）

- **Topic/Key 模板引擎**：Handlebars `{{var}}`，非 strict（缺失键→空字符串），内置 `{{default x "fallback"}}` helper。
- **UplinkPayloadConfig**：
  - `envelope_json`（默认）
  - `kv { includeMeta }`
  - `timeseries_rows { includeMeta }`
  - `mapped_json { config }`：JMESPath 表达式映射（compile once, apply many）。
- **MappedJson 输入视图（稳定 shape）**：`{schema_version,event_kind,ts_ms,app:{...},device:{...},data:{...}}`
  - 这决定了文档里要给用户一份**可复制的字段表 + 示例输入**。

### 1.3 Kafka 插件（northward/kafka）

- **Uplink**：topic/key 支持模板；headers 会携带 RenderContext kv；producer 支持 idempotence、acks、compression、batch/linger/timeouts、max.in.flight 等。
- **Downlink**：
  - 精确 topic 订阅（不支持模板/通配/regex）。
  - group.id = `ng-gateway-plugin-{app_id}`，`enable.auto.commit=false`，`auto.offset.reset=latest`。
  - payload 支持 `EnvelopeJson` 或 `MappedJson`（带 filter：json pointer / property / key）。
  - `AckPolicy` + `FailurePolicy` 决定 commit 行为。

### 1.4 Pulsar 插件（northward/pulsar）

- **Uplink**：topic/key 模板；properties = RenderContext kv；partition_key = key；支持可选 batching（默认 false）+ compression（默认 LZ4）。
- **Downlink**：
  - subscription name = `ng-gateway-plugin-{app_id}`，SubType=Shared。
  - topics 为 route table 的精确 topic 列表（无 wildcard）。
  - 按 `AckPolicy/FailurePolicy` 做 ack / nack。

### 1.5 ThingsBoard 插件（northward/thingsboard）

- **连接模式**：None / UsernamePassword / Token / X509Certificate / Provision。
- **Provision**：
  - `/provision/request` & `/provision/response`，支持总超时、最大重试次数、重试间隔。
  - 凭据持久化：extension manager `provision_credentials`。
- **Uplink（JSON）**：
  - Telemetry：`v1/gateway/telemetry`，payload 形状按 ThingsBoard Gateway API。
  - Attributes：`v1/gateway/attributes`。
  - Connect/Disconnect：`v1/gateway/connect` / `v1/gateway/disconnect`。
- **Downlink/交互**：订阅 attributes/RPC/gateway rpc 等 topic，通过 router 分发到 handlers，并转为 `NorthwardEvent`。
- **Protobuf**：配置枚举存在，但当前实现会返回错误（需要文档明确告知）。

### 1.6 OPC UA Server 插件（northward/opcua-server）

- **对外暴露 OPC UA Server**（与 southward 的 OPC UA driver 不同：这是 northward 插件）。
- **节点结构**：
  - AddressSpace 根：`Objects/NG-Gateway/{channel}/{device}/{point}`
  - NodeId（String）：`ns=1;s={channel}.{device}.{point_key}`（对组件进行 sanitize：仅保留 `[A-Za-z0-9._-]`，其余替换为 `-`）
  - Variable 的 `DataType` 与 `AccessLevel` 由点位 meta 映射。
- **更新队列**：插件内部 update queue（batch 级）支持 `DiscardOldest/DiscardNewest/BlockWithTimeout`，默认容量 10_000、默认丢弃 oldest（偏实时新鲜度）。
- **写回**：
  - OPC UA Write → `NorthwardEvent::WritePoint`（带 timeout_ms）
  - 等待 `WritePointResponse` 再回写 status；错误映射到 OPC UA `StatusCode`。
- **安全**：
  - endpoints：`no_security` 与 `basic256sha256_sign_encrypt`（默认 endpoint 目前是 `no_security`）
  - `trusted_client_certs` 支持 PEM 或 base64 DER，会 materialize 到 `./pki/plugin/{plugin_id}/trusted/`

---

## 2. 文档信息架构（IA）与页面树（建议落地到 VitePress）

### 2.1 北向文档的“产品级导航”建议

**目标**：用户按“从 0 到 1”的路径阅读；同时保留“查字典式参考”页面；每个插件页都能独立闭环（配置→验证→注意事项→排障）。

建议把北向拆成三层：

1) **总览/快速开始/通用语义（跨插件）**  
2) **协议与数据格式（跨插件）**：Envelope/Kv/TimeseriesRows/MappedJson/模板变量/Downlink 语义  
3) **插件分册（按平台）**：Kafka / Pulsar / ThingsBoard / OPC UA Server

### 2.2 建议的 docs 文件结构（`ng-gateway-ui/docs/src/northward/`）

> 说明：当前仅有 `northward/overview.md`。以下是完整建议树；你确认后我再把这些页面逐个创建并填充。

```text
docs/src/northward/
  overview.md                      # 已存在：需要补齐与本规划一致的链接与限制说明
  quick-start.md                   # 从 0 到 1：创建 App/订阅/验证链路
  concepts.md                      # Plugin/App/AppSubscription、uplink/downlink、数据面/控制面
  policies/
    retry-policy.md                # RetryPolicy 语义、默认值、最佳实践
    queue-policy.md                # QueuePolicy/内存 buffer 现状与限制（无磁盘）
  templates/
    handlebars.md                  # 模板语法（Handlebars + default helper），变量表
    variables.md                   # RenderContext 变量、时间分区变量 yyyy/MM/dd/HH
  payload/
    overview.md                    # 四种上行 payload 选型指南
    envelope-json.md               # 协议包络（现有 overview.md 已引用：需要补文件）
    kv.md                          # Kv 形状与 includeMeta
    timeseries-rows.md             # TimeseriesRows 形状与 includeMeta
    mapped-json.md                 # JMESPath 映射：输入视图/规则/陷阱/性能
    mapped-json-jmespath.md        # 常用 JMESPath cheat sheet（面向用户）
  downlink/
    overview.md                    # 下行总览：事件种类、topic 精确订阅限制、ack/failure
    envelope-json.md               # 下行 EnvelopeJson：WritePoint/Command/RpcResponse
    mapped-json.md                 # 下行 mapped_json：filter 的使用方式（JsonPointer/Key/Property）
  best-practices/
    architecture.md                # 多 App 隔离、容量预算、关键数据与遥测拆分
    performance.md                 # 吞吐调优：批处理、压缩、topic/partition 策略
    reliability.md                 # 弱网策略：Retry/Queue/buffer 现状与 roadmap
  troubleshooting/
    overview.md                    # 排障索引（按症状）
    common-errors.md               # 常见错误码/日志关键字/定位路径
    verify-checklist.md            # “怎么验证”清单：连通性/主题/消息格式/消费确认
  kafka/
    index.md                       # Kafka 插件总览与快速配置
    connection-security.md         # PLAINTEXT/SSL/SASL、TLS/SASL 参数与踩坑
    uplink.md                      # uplink topics/keys/headers/payload
    partitions.md                  # 分区策略（key）、有序性、幂等与吞吐
    downlink.md                    # downlink topic 精确订阅、group、Ack/Failure、offset 行为
    examples.md                    # 典型场景配置示例（含 JSON）
    troubleshooting.md             # Kafka 专属排障（认证、ACL、超时、QueueFull、消费组）
    assets/
      placeholder.txt              # 截图占位目录（你后续替换）
  pulsar/
    index.md
    connection-auth.md
    uplink.md
    partitions.md                  # partition_key 与 topic 规划
    downlink.md                    # Shared subscription + ack/nack + filter
    examples.md
    troubleshooting.md
    assets/placeholder.txt
  thingsboard/
    index.md
    connection-modes.md            # None/Token/UserPass/X509/Provision
    provision.md                   # Provision 深入：profile/key/secret、凭据存储、重试
    uplink-format.md               # TB Gateway Telemetry/Attributes/Connect/Disconnect payload 形状
    rpc-and-attributes.md          # RPC/Attributes 下行：topic、payload、事件映射
    protobuf-status.md             # 明确说明：Protobuf 当前不支持（指向 roadmap）
    examples.md
    troubleshooting.md
    assets/placeholder.txt
  opcua-server/
    index.md
    node-mapping.md                # NodeId/层级/命名 sanitize 规则（必须稳定）
    security.md                    # endpoints、trusted_client_certs、PKI 目录、证书导入
    writeback.md                   # Write → WritePoint → Response → StatusCode 映射
    performance.md                 # update queue 与 drop policy 调优
    troubleshooting.md
    assets/placeholder.txt
  not-supported/
    mqtt.md                        # 若保留侧边栏入口：明确当前版本不支持
    websocket.md
    http.md
```

::: tip 侧边栏调整建议（最小化）
- 如果你希望“用户不要点进不存在的页面”，建议把当前侧边栏里未实现的 MQTT/WebSocket/HTTP 先移到 `not-supported/` 并明确写“Roadmap”，或临时从侧边栏隐藏。
- 为保持与 `southward/*/index.md` 一致，建议北向插件也使用 `index.md` 形式，并把侧边栏 link 统一改成带 `/` 的路径（例如 `/northward/kafka/`）。
:::

---

## 3. 每类页面的“产品级写作模板”（落地时统一套用）

> 这一部分是“写作规范 + 内容 checklist”，确保每页都能让用户独立闭环。

### 3.1 插件首页（`northward/<plugin>/index.md`）模板

- **这是什么**：插件定位、适用场景、你会获得什么能力（uplink/downlink/写回等）。
- **你需要准备什么**：依赖组件（Broker、证书、账号、topic 规划等）。
- **最快跑通（10 分钟）**：
  - 创建 App
  - 配置最小字段（只填必填）
  - 建立 AppSubscription
  - 验证：如何看到消息（示例命令）
  - 常见失败点（3~5 个）
- **配置模型总览**：给一份“字段表 + 默认值 + 推荐值”。
- **限制/不支持**：放在醒目的 `::: warning` 中（例如：topic 精确订阅、protobuf 不支持、无磁盘续传等）。

### 3.2 通用策略（`RetryPolicy` / `QueuePolicy`）页面模板

- **语义先行**：用 2~3 段话解释“为什么需要它”与“选错会怎样”。
- **字段表**：字段名、类型、默认值、建议范围、错误后果。
- **最佳实践**：
  - 关键数据 vs 高频遥测拆分 App
  - `Discard` vs `Block` 选择
  - buffer 只适用于短时抖动（并明确“不是断网续传”）
- **运维指标**：推荐监控的 counters/latency/queue depth（结合现有 ops 文档链接）。

### 3.3 payload 协议页面模板（Envelope/Kv/TimeseriesRows/MappedJson）

- **何时选它**（决策表）
- **JSON 形状（示例）**：最少给 2 个例子：Telemetry & Attributes（或控制面）。
- **字段语义**：schema_version、event_kind、ts_ms、device/app 元信息等。
- **兼容性**：哪些字段稳定承诺、哪些可能扩展。
- **性能注意事项**：字段名重复、对象大小、批量写入建议。
- **常见坑**：时间戳单位、字符串/二进制编码、meta 开关对体积影响。

### 3.4 downlink 页面模板（EnvelopeJson / MappedJson）

- **事件类型**：WritePoint / CommandReceived / RpcResponseReceived（分别解释“它对应什么场景”）。
- **topic 限制**：必须精确匹配（强约束）。
- **ack/failure 语义**：AckPolicy=OnSuccess/Always/Never；FailurePolicy=Drop/Error（commit/ack/nack 行为）。
- **安全建议**：topic 不承载敏感信息；鉴权/ACL；避免把 request_id 编进 topic。
- **排障**：过滤器不匹配、payload decode 错、事件通道关闭等。

---

## 4. 截图规划（占位符清单）

> 你后续会自行截图替换；落地文档时我会在对应位置放 `![...](...)` 占位。

### 4.1 通用 UI（北向 App）

- **创建北向 App**：App 列表页 + 新建弹窗/页面  
  `<!-- TODO screenshot: northward-app-create -->`
- **配置 RetryPolicy/QueuePolicy**：策略配置区域  
  `<!-- TODO screenshot: northward-policy-config -->`
- **AppSubscription 订阅设备**：选择设备/优先级/保存  
  `<!-- TODO screenshot: northward-app-subscription -->`
- **连接状态与错误信息**：Connected/Failed/last_error 展示  
  `<!-- TODO screenshot: northward-connection-state -->`

### 4.2 Kafka / Pulsar / ThingsBoard / OPC UA Server 专属

- **Kafka**：topic/key/payload 配置区；消费验证截图  
  `<!-- TODO screenshot: kafka-config -->` / `<!-- TODO screenshot: kafka-consume-verify -->`
- **Pulsar**：service_url/auth/batching；消费验证截图  
  `<!-- TODO screenshot: pulsar-config -->` / `<!-- TODO screenshot: pulsar-consume-verify -->`
- **ThingsBoard**：Provision 配置、TB 后台 device profile/key/secret 配置、收到 telemetry 的平台界面  
  `<!-- TODO screenshot: tb-provision-config -->` / `<!-- TODO screenshot: tb-ui-telemetry -->`
- **OPC UA Server**：UAExpert 浏览树、读值、写值与返回状态  
  `<!-- TODO screenshot: opcua-browse -->` / `<!-- TODO screenshot: opcua-write -->`

---

## 5. Roadmap 对齐（需要在文档中明确链接）

### 5.1 本规划中明确“当前不支持/未来计划”的点

- **磁盘 WAL / 断网续传 / 回放**（产品级可靠性）
- **满队列精细化策略**：时间采样、latest 合并、按数据类型优先级丢弃、按窗口聚合等
- **二进制 payload（Protobuf/Avro）**：包括 Schema 管理、灰度双写等
- **Lua Transform Sandbox（MappedJson 的下一步）**：参见 `document/ng-lua-transform-sandbox-design.md`

### 5.2 文档内的统一指向

- 路线图页面（站内）：`/guide/other/roadmap`
- Lua Transform 设计（repo 文档）：`document/ng-lua-transform-sandbox-design.md`

---

## 6. 落地顺序（你确认后我将按此推进）

1) 先补齐北向的“通用闭环”页面：`quick-start`、`queue-policy`、`retry-policy`、`templates/variables`、`payload/*`、`downlink/*`、`troubleshooting/*`  
2) 再逐个落地插件分册：Kafka → Pulsar → ThingsBoard → OPC UA Server  
3) 最后处理“侧边栏存在但未实现的插件”页面：写清楚 not-supported 并指向 roadmap（或按你的偏好从侧边栏隐藏）
