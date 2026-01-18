# 北向插件文档结构与产品级落地计划（v3.0）

本文档用于规划并落地 `ng-gateway-ui/docs/src/northward` 的**产品级北向插件文档**：让用户不需要看源码，也能清楚知道：

- **如何使用北向插件**：怎么创建/配置 App、怎么订阅设备、怎么上线与回滚
- **怎么验证**：如何确认“连接成功/数据上云/下行可用/不会丢得离谱”
- **需要注意什么**：弱网、队列背压、平台限流、TLS、幂等、顺序、容量预算
- **遇到问题怎么排查**：从症状到根因的路径 + 具体命令/指标/日志字段

> 范式参考：`docs/src/southward/*` 的写法（配置模型 + 必读注意点 + 排障），并与现有北向 `overview.md` 的术语/语义保持一致。

---

## 0. 文档范围与硬边界（避免“写了但做不到”）

### 0.1 本轮文档要覆盖的“用户旅程”

1. **选型**：我该选 Kafka / Pulsar / ThingsBoard / MQTT / OPC UA Server？
2. **安装启用**：插件是否存在？版本匹配吗？依赖（Broker/平台）可达吗？
3. **配置**：App 配置、AppSubscription（订阅设备/路由）、RetryPolicy、QueuePolicy、TLS
4. **验证**：连接状态、数据是否到达、是否有丢弃/积压、下行命令是否可闭环
5. **上线与运维**：容量预算、告警、升级/回滚、弱网与平台限流的处置
6. **排障**：连接不上、鉴权失败、发布失败、队列满、延迟变大、CPU/内存异常

### 0.2 明确暂不支持（或仅 Roadmap）的能力

文档中必须以 **`::: warning` 明示**，并在路线图中可追踪：

- **磁盘断网续传（Disk WAL + 回放）**：当前 `overview.md` 已标注 Roadmap；需要补齐“产品级最佳实践（现阶段怎么做）+ 路线图条目（未来怎么做）”
- **更精细的队列满降级策略**（按数据类型/优先级/时效性自动降级：丢弃、采样、按 last 合并等）：需要“现状说明 + 最佳实践 + Roadmap”
- **HTTP/WebSocket 北向连接器**：`zh.mts` 侧边栏已预留，但若实现/文档未完成，需要给出“暂不支持页 + 路线图”

---

## 1. 读者画像（决定文档怎么写）

- **集成工程师（主要读者）**：关心“怎么配 + 怎么验证 + 出问题怎么办”
- **运维/现场工程师**：关心“告警指标 + 排障路径 + 回滚与风险”
- **开发者**：关心“插件边界、失败语义、队列与背压协作、配置 schema”

写作原则：

- **默认读者是集成工程师**：每页必须能做到“复制配置 + 执行验证”
- **高级概念放到“概念/最佳实践”**：不要把新手挡在第一屏
- **所有关键能力都要给“现状 vs Roadmap”**：避免误用与售前误导

---

## 2. 北向术语与语义（统一心智模型）

文档中统一使用以下概念，并在第一次出现时链接到 `northward/overview.md`：

- **Plugin（插件）**：Kafka/Pulsar/ThingsBoard/MQTT/OPC UA Server
- **App（北向应用实例）**：一个 Plugin 的一个运行实例（可多实例：多租户/多 topic/隔离策略）
- **AppSubscription（应用订阅）**：App 订阅哪些设备/点位；并用 `priority` 表达资源紧张时优先级
- **RetryPolicy**：指数退避重试（连接重试/发送失败重试）
- **QueuePolicy**：队列容量、队列满策略、断链缓冲（目前以内存为主，磁盘为 Roadmap）

---

## 3. 目标信息架构（IA）：目录树 + 导航对齐

### 3.1 最终目录树（建议落地）

```text
ng-gateway-ui/docs/src/northward/
├── overview.md                         # 已有：北向架构/通用策略/现状与 Roadmap（补充链接与落地指引）
├── quickstart.md                       # 新增：10 分钟跑通（创建 App + 订阅 + 验证）
├── configuration.md                    # 新增：通用配置模型（App/AppSubscription/RetryPolicy/QueuePolicy）
├── verification.md                     # 新增：验证清单（连通性/上行/下行/指标/日志）
├── troubleshooting.md                  # 新增：北向专项排障（连接/鉴权/限流/队列满/延迟）
├── best-practices/
│   ├── capacity-planning.md            # 新增：容量预算（队列容量/带宽/CPU/磁盘（Roadmap））
│   ├── offline-and-weak-network.md     # 新增：弱网最佳实践（现阶段做法 + 断网续传 Roadmap）
│   ├── queue-full-strategies.md        # 新增：队列满策略（现状 + 规划：丢弃/采样/last 合并等）
│   ├── security-and-secrets.md         # 新增：TLS/证书/凭证管理（引用 ops/tls.md）
│   └── multi-app-isolation.md          # 新增：多 App 隔离（关键数据/遥测拆分、不同策略）
├── kafka/
│   ├── index.md                        # 新增：Kafka 插件使用（配置/验证/排障）
│   ├── auth.md                         # 新增：SASL/SSL 认证矩阵（可选拆分）
│   └── tuning.md                       # 新增：吞吐/顺序/幂等/压缩/批处理（生产建议）
├── pulsar/
│   ├── index.md                        # 新增：Pulsar 插件使用（配置/验证/排障）
│   └── tuning.md                       # 新增：producer batching、compression、topic 策略
├── thingsboard/
│   ├── index.md                        # 新增：ThingsBoard 插件使用（连接/映射/验证）
│   ├── mapping.md                      # 新增：Telemetry/Attributes 映射与命名约定
│   └── rpc.md                          # 新增：下行 RPC 闭环（与网关 Action/WritePoint 对齐）
├── opcua/
│   ├── index.md                        # 新增：OPC UA Server 使用（地址空间/连通性/验证）
│   └── security.md                     # 新增：安全策略/证书/UaExpert 验证
├── mqtt/
│   ├── index.md                        # 新增：MQTT 插件使用（Topic/认证/TLS/验证）
│   └── best-practices.md               # 新增：Topic 规划、Retain/LWT、QoS 选择、限流
├── http.md                             # 新增（占位）：若当前不支持则写清楚 + 路线图
└── websocket.md                        # 新增（占位）：若当前不支持则写清楚 + 路线图
```

### 3.2 与侧边栏导航的对齐（`docs/.vitepress/config/zh.mts`）

当前 `zh.mts` 已有北向条目：Kafka / Pulsar / Thingsboard / OPC UA / MQTT / WebSocket / HTTP（见侧边栏 `北向` 分组）。

本计划建议：

- **保持现有导航不动**（先把页面补齐），避免用户链接失效
- 对 **HTTP/WebSocket**：
  - 若实现/文档暂未完成：落地 `northward/http.md` 与 `northward/websocket.md` 为“暂不支持说明页”
  - 同时更新路线图（见第 7 章）

---

## 4. 每一页要写什么（必须可用、可验证、可排障）

> 这部分是落地写作的“验收标准”。每一页完成后，读者应该能在不求助的情况下走完关键路径。

### 4.1 `northward/quickstart.md`（10 分钟跑通）

内容结构建议：

- **前置条件**：网关已安装；至少 1 个 Southward Channel + Device + Point 能产生数据
- **步骤 1：创建北向 App**
  - UI 操作步骤（截图占位）
  - 最小可用配置示例（JSON）
- **步骤 2：创建 AppSubscription**
  - 选择设备/过滤规则/priority（截图占位）
- **步骤 3：验证**
  - App 状态 = Connected（截图占位）
  - 平台侧看到数据（Kafka/Pulsar/MQTT/ThingsBoard 示例各给 1 个最短验证命令）
  - 关键指标/日志确认“没有持续背压”

截图占位（示例）：

- `![创建北向 App（占位）](./assets/quickstart/app-create.png)`
- `![配置 AppSubscription（占位）](./assets/quickstart/subscription-create.png)`
- `![平台侧消费验证（占位）](./assets/quickstart/consume-verify.png)`

### 4.2 `northward/configuration.md`（通用配置模型）

必须包含：

- **App / AppSubscription 的字段解释**（与 `overview.md` 保持一致）
- **RetryPolicy** 参数表 + 推荐默认值解释（含“什么时候不要无限重试”）
- **QueuePolicy** 参数表 + 推荐默认值解释
- **字段命名规则**：对外 JSON camelCase vs 内部 snake_case（`overview.md` 已提及）

必须用 `::: tip` 强调：

::: tip 推荐做法：拆分 App 做隔离
把“关键数据（告警/事件/控制面）”和“高频遥测”拆到不同 App，分别配置不同的 `queuePolicy` 与 `retryPolicy`，避免慢消费者拖垮全局。
:::

### 4.3 `northward/verification.md`（怎么验证：连得上、发得出、收得到）

必须覆盖三层验证：

- **网关侧**：App 状态、指标（队列深度/丢弃数/重试次数）、日志（鉴权失败/限流/超时）
- **网络侧**：DNS/路由/端口/TLS
- **平台侧**：最短消费/查看方式

必须给出一个“验证清单”：

- [ ] App 显示 Connected（或至少能自动重连）
- [ ] 平台侧能看到新数据（不是历史缓存）
- [ ] 队列深度在可控范围波动，不持续上涨
- [ ] drops 为 0（或在预期范围内）且有解释

::: warning 验证不要只看“能连上”
只验证连接成功是不够的。必须同时验证：在业务峰值流量下，队列不会长期接近满、不会持续丢弃、延迟不会不可控增长。
:::

### 4.4 `northward/troubleshooting.md`（北向专项排障）

这里要承接 `docs/src/ops/troubleshooting.md`，但聚焦北向“可操作”的细节。

建议用 Mermaid 做 3 条排障树：

1. **连接失败**：网络不可达 → TLS → 鉴权 → 平台限流/ACL
2. **连接正常但没数据**：订阅错误 → 路由/过滤 → payload/schema → 平台消费组/offset
3. **延迟变大/丢数据**：队列满 → 插件 buffer full（见第 6 章）→ 降级策略 → 容量预算/拆分 App

必须包含一个“常见错误码/日志”表格（按插件类型分别列）：

- 鉴权失败（401/403/SASL auth failed）
- TLS handshake failed / certificate verify failed
- timeout / connection reset / broken pipe
- rate limited / quota exceeded
- queue full / backpressure applied（对应 `ng-gateway-sdk/src/northward/mod.rs` 的行为）

### 4.5 `northward/best-practices/offline-and-weak-network.md`（弱网/断链最佳实践）

必须明确两段内容：

- **现阶段（已支持）**：内存队列 + 重试/退避的最佳实践（怎么配、怎么验证、怎么告警）
- **Roadmap（计划支持）**：磁盘 WAL + 回放的产品规划（为什么需要、用户能获得什么、会暴露哪些配置）

::: warning 当前版本的断网续传是 Roadmap
当前版本的可靠性主要依赖 **内存队列（QueuePolicy）+ 重试（RetryPolicy）**。磁盘 WAL 断网续传/回放属于 Roadmap，请不要将其作为强承诺能力依赖。
:::

### 4.6 `northward/best-practices/queue-full-strategies.md`（队列满策略：现状 + 规划）

这页是你特别点名的关键页：要同时写清楚 **当前行为** 与 **产品级策略规划**。

必须引用现状（面向用户语言，不需要贴源码）：

- 北向插件内部存在 **bounded queue**；当队列满时会返回背压错误（`Plugin buffer full - backpressure applied`）。
- 这意味着“上游继续推数据 ≠ 系统会无限缓存”；系统会显式暴露拥塞信号。

产品级策略规划（文档里先说清楚，后续实现再落地）：

- **策略 A：Discard（丢弃）**（默认建议用于高频遥测）
  - 丢弃“最不值钱”的数据（通常是过期遥测）
- **策略 B：Sample（采样）**
  - 当队列深度超过阈值时，按比例降低上报频率（例如 1/2、1/4）
- **策略 C：Coalesce by last（按 last 合并）**
  - 以 `(device_id, point_key, type)` 为 key，只保留最新值（适合“状态类遥测”，不适合“事件类”）
- **策略 D：Priority preserve（按优先级保留）**
  - 报警/事件/控制面响应优先保留；遥测优先降级

必须讲清楚每种策略的“代价/边界”：

- 采样会影响统计准确性
- last 合并会丢掉变化过程（只能保留最终态）
- 优先级需要“数据类型可判定”与指标可观测

---

## 5. 插件分册：每个插件页面必须包含的固定章节

为了“语义正确 + 可运营”，每个插件 `*/index.md` 都按相同结构写（用户不需要重新学习）：

1. **适用场景**：什么时候用它、替代方案是什么
2. **前置条件**：平台准备项（Topic/Token/证书/ACL/Consumer）
3. **最小配置示例**：一份可复制的 App.config + retryPolicy + queuePolicy
4. **订阅与路由**：AppSubscription 怎么配（至少 1 个示例）
5. **验证**：平台侧最短验证方式（命令 + 截图占位）
6. **常见问题**：鉴权/TLS/限流/顺序/幂等/消费组/offset（按插件特性）
7. **最佳实践**：吞吐调优、Topic/Partition 规划、隔离与容量预算
8. **安全建议**：TLS、凭证管理（引用 `ops/tls.md`）

每个插件至少要有 1 张“平台侧验证截图占位”。

---

## 6. 与代码行为对齐：队列满（`ng-gateway-sdk/src/northward/mod.rs`）需要怎么对用户讲

### 6.1 现状需要在文档中明确的事实

- 插件侧对 `process_data` 采用 **非阻塞入队**；当队列满时返回背压错误（`Plugin buffer full - backpressure applied`）。
- 这不是“Bug”，是**系统自保护**：避免无限堆积导致 OOM。

### 6.2 文档需要回答的三个问题（用户最关心）

1. **队列为什么会满**：平台慢/断网/限流/配置不合理/单 App 承载了太多数据
2. **满了会发生什么**：是否丢数据？是否阻塞？是否重试？如何观测？
3. **我该怎么处理**：拆分 App、调整 QueuePolicy/RetryPolicy、启用采样/last 合并（Roadmap）、扩容平台/带宽

::: tip 文档写法建议
把“队列满”当成一个**可操作的系统状态**来讲：给阈值、给指标、给动作（立刻止血 + 长期治理）。
:::

---

## 7. 路线图（需要同步更新到 `docs/src/guide/introduction/roadmap.md`）

当前路线图文件还是空的（`TODO`）。本轮文档规划需要至少补齐以下条目，并在北向 `overview.md`/最佳实践页中反向链接：

- **北向磁盘断网续传（Disk WAL + 回放 + 限速）**
  - 需求：弱网/断链/掉电不丢关键数据
  - 关键点：落盘格式、目录、磁盘配额、回放速率上限、与实时链路隔离
- **北向队列满智能降级**
  - 丢弃/采样/last 合并/按优先级保留
  - 必须配套指标：丢弃数、合并命中数、采样倍率、队列深度分位数
- **HTTP / WebSocket 北向连接器**
  - 若当前未生产可用：先给占位文档 + Roadmap 状态说明

---

## 8. 截图清单（统一占位，后续你替换即可）

建议统一放到：`ng-gateway-ui/docs/src/northward/assets/`（按页面子目录组织）。

- `assets/quickstart/app-create.png`：创建北向 App
- `assets/quickstart/subscription-create.png`：配置 AppSubscription
- `assets/quickstart/consume-verify.png`：平台侧看到数据
- `assets/verification/app-status.png`：App 状态/连接状态
- `assets/verification/metrics-queue.png`：队列深度/丢弃数指标面板
- `assets/kafka/kcat-consume.png`：kcat 消费验证
- `assets/mqtt/mosquitto-sub.png`：mosquitto_sub 验证
- `assets/thingsboard/telemetry.png`：ThingsBoard 最新遥测
- `assets/opcua/uaexpert.png`：UaExpert 节点树/变量值

---

## 9. 落地步骤与验收标准（写完就是“可交付”）

### 9.1 落地顺序（建议）

1. `northward/quickstart.md`
2. `northward/configuration.md` + `northward/verification.md`
3. `northward/troubleshooting.md`
4. `northward/best-practices/*`（弱网/队列满/容量预算/隔离/安全）
5. 各插件分册：Kafka → MQTT → ThingsBoard → Pulsar → OPC UA
6. `northward/http.md` / `northward/websocket.md`（占位或落地）
7. 更新 `guide/introduction/roadmap.md`（同步 Roadmap）

### 9.2 验收标准（每页通用）

- 有 **最小配置示例**（可复制）
- 有 **验证步骤**（可执行）
- 有 **常见问题与排查**（可操作）
- 对 Roadmap 能力有 **明确 warning**（不误导）
