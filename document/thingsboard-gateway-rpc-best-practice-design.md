## NG Gateway：ThingsBoard Gateway Service RPC（设备重命名/删除/凭据清理/心跳/设备列表/重启/重启系统）最佳实践设计与 Phase 推进计划

> 本文目标：在 **不破坏现有 northward/southward 架构** 的前提下，为 ThingsBoard（TB）补齐其对网关下发的 **Gateway Service RPC** 能力，做到：
>
> - **语义完整**：覆盖 TB 常见 gateway-level RPC（rename/delete/remove_provisioned_credentials/ping/devices/restart/reboot）
> - **性能极致**：不引入热路径额外锁竞争/分配；控制面做到常数级开销、可背压、可限流
> - **可演进**：TB 特有语义尽量收敛在 TB 插件内部，不污染 core 的通用命令空间
> - **可观测 + 可治理**：可开关、可灰度、可回滚、可审计

---

## 0. 背景与问题陈述

### 0.1 TB 会给网关下发哪些 RPC（你提供的事实输入）

TB 在“删除网关子设备 / 子设备重命名 / 删除已 provision 的凭据”等场景，会下发 gateway-level RPC 到网关（典型为 `v1/devices/me/rpc/request/+`）。

- **Device renaming RPC（设备重命名）**

```json
{
  "method": "gateway_device_renamed",
  "params": {"Old device name": "New device name"}
}
```

- **Device removal RPC（设备移除）**

```json
{
  "method": "gateway_device_deleted",
  "params": "Removed device name"
}
```

- **Device remove provisioned credentials RPC（删除已 provision 的 credentials）**
  - `params` 为空（或无该字段）
  - 上层 TB 通知网关删除已 provision 的相关 credentials

你同时关心是否需要实现这些方法：

- `device_renamed` / `device_deleted`（可能是旧版本/不同文档名，或与 `gateway_device_*` 等价）
- `remove_provisioned_credentials`
- `gateway_ping`
- `gateway_devices`
- `gateway_restart`
- `gateway_reboot`

### 0.2 为什么“必须实现”，否则会发生什么（语义层面）

TB 的 Gateway API 强依赖 **device name** 作为子设备标识（MQTT Gateway API 里 device name 是主键语义）。如果平台侧发生 rename/delete，而网关侧不跟随更新：

- **rename**：网关继续用旧 name 上报，TB 会“重新创建/复活”一个同名设备或造成数据分裂（同一物理设备数据散落到新旧两个 TB device）
- **delete**：网关继续上报会“重新创建”被删除的 device（平台侧认为删了但又回来）
- **remove_provisioned_credentials**：若网关继续使用旧凭据，可能造成认证失败/不可控重连风暴；若凭据泄露，平台要求立即撤销时无法生效

结论：对 TB 而言，这些 RPC 属于 **控制面一致性** 的关键闭环，不实现会造成“平台状态与网关状态不可收敛”。

---

## 1. 现状审计（以代码为事实基础）

### 1.1 当前 TB 插件的控制面入口已经存在，但没有“方法语义”

TB 插件已订阅 gateway/device RPC topic，并将 RPC 解析为统一的 `NorthwardEvent::CommandReceived`：

- `v1/devices/me/rpc/request/+`（网关级 RPC）在 `ng-gateway-northward/thingsboard/src/handlers.rs::handle_device_rpc_request` 中解析为：
  - `Command.target_type = TargetType::Gateway`
  - `Command.key = rpc_request.method`
  - `Command.params = rpc_request.params`
- `v1/gateway/rpc`（子设备 RPC）在 `handle_gateway_rpc` 中解析为：
  - `Command.target_type = TargetType::SubDevice`
  - `Command.device_name = rpc_request.device`

但当前实现 **没有** 对 `gateway_device_renamed / gateway_device_deleted / ...` 做任何语义化处理。

### 1.2 core 对 gateway-level command 的执行机制是“全局 registry”，不适合承载 TB 特有语义

core 在 `ng-gateway-core/src/gateway.rs` 中统一处理 `NorthwardEvent::CommandReceived`：

- `TargetType::SubDevice`：路由到 southward 执行动作（与 TB 无关）
- `TargetType::Gateway`：调用 `handle_gateway_commands(cmd)`，并通过 `gateway_command_registry()` 查找 handler

当前 `ng-gateway-core/src/commands.rs` 的 gateway registry 只有一个示例 `get_gateway_status` handler（且是全局命名空间），并且 handler 接口拿不到 `app_id`/`plugin_type`：

- **问题 1：无法区分“哪个 northward app 发来的 gateway command”**
- **问题 2：TB 特有命令如果直接放进 core registry，会污染通用命令空间**
- **问题 3：rename/delete 等需要影响 TB 插件的“出站 device name 绑定”，这属于插件私有状态；core registry 不持有该状态**

最佳实践结论：**TB 特有 Gateway Service RPC 应该优先在 TB 插件内部完成处理与响应**；只有确实属于“网关进程级别”的操作（restart/reboot）才考虑落在 core，但也应通过“平台无关的内部命令”对接。

---

## 2. 设计目标与非目标

### 2.1 设计目标（必须同时满足）

- **语义完整**
  - 覆盖 TB Gateway Service RPC 常见方法：rename/delete/remove_provisioned_credentials/ping/devices/restart/reboot
  - 允许多种 `params` 形状（TB 版本差异/文档差异/大小写差异），做到“尽量兼容、严格校验、可观测拒绝”
- **极致性能（不牺牲热路径）**
  - telemetry/attributes 热路径不引入额外序列化开销、不引入重锁
  - device name 重写必须是 O(1) 查表，且 clone 成本最小（建议 `Arc<str>`）
- **幂等 + 顺序鲁棒**
  - 同一 RPC 重复投递/重试不导致状态抖动
  - rename/delete 的乱序到达能够收敛（至少做到 last-writer-wins + tombstone）
- **安全**
  - restart/reboot 默认禁用；需显式配置允许
  - 对外部输入（RPC payload）做大小限制、类型校验、方法 allowlist
- **可观测**
  - 每个方法：计数、失败计数、延迟、拒绝原因
  - 关键状态变化：alias 更新、tombstone 命中、凭据删除、重启触发
- **可演进**
  - TB 特有状态（alias/tombstone/凭据）收敛在 TB 插件；未来可对接其他平台实现自己的适配

### 2.2 非目标（本期不强制）

- 不要求实现 TB 的全部 gateway management API（如 remote config 全量、firmware/software OTA 流程）
- 不要求改变 core 的 `Command`/`NorthwardEvent` 通用协议（仅在必要时提出“下一阶段演进建议”）

---

## 3. 关键设计决策：把 TB Service RPC 放到哪里处理？

### 3.1 方案对比

#### 方案 A：把 `gateway_device_*` 等方法直接注册到 core 的 `gateway_command_registry`

- **优点**
  - 改动路径短：TB 插件无需改动，继续将 RPC 作为 `CommandReceived` 上抛
- **致命问题**
  - core handler 无法感知 `app_id`/`plugin_type`，无法区分是否来自 TB 插件
  - rename/delete 需要影响 TB 插件的出站 device name（alias），core 无法持有插件私有状态
  - 命名空间污染：TB method 是平台私有协议，不应进入 core 通用命令集合

结论：**不推荐**。

#### 方案 B：TB 插件在接收 RPC 时“就地语义化处理并直接回复 TB”

- **优点**
  - 平台私有语义收敛在插件内部（最佳实践）
  - alias/tombstone/凭据等状态天然属于插件生命周期，能持久化在插件自己的 `ExtensionStore`
  - core 保持平台无关
- **挑战**
  - 当前 `MessageRouter` handler 仅有 `(topic, payload)`，没有直接拿到 `AsyncClient` 用于 publish response

结论：**推荐**，但需要一个“轻量控制面上下文”让 handler 能发响应。

#### 方案 C：TB 插件只做“method 翻译”，把平台私有方法翻译成“平台无关内部命令”再上抛 core

- **适用范围**
  - `gateway_restart/gateway_reboot` 这类确实属于“网关进程/系统级动作”的命令
- **对 rename/delete 的限制**
  - rename/delete 更像“平台侧 device registry 的一致性事件”，并不等于网关本地设备的 rename/delete（本地设备是否删除由你的产品决定）

结论：**混合模式最佳**：

- rename/delete/remove_provisioned_credentials/ping/devices：**TB 插件内处理**
- restart/reboot：**翻译为平台无关内部命令**（由 core 执行或交给宿主/编排系统）

---

## 4. 目标语义与统一抽象（强约束契约）

### 4.1 统一方法集合与兼容别名

建议把 TB 的 Gateway Service RPC 统一建模为：

- **Device registry consistency（设备注册表一致性）**
  - `gateway_device_renamed`（alias：`device_renamed`）
  - `gateway_device_deleted`（alias：`device_deleted`）
- **Credentials management（凭据管理）**
  - `remove_provisioned_credentials`
- **Health & inventory（健康检查与清单）**
  - `gateway_ping`
  - `gateway_devices`
- **Host lifecycle（宿主生命周期）**
  - `gateway_restart`
  - `gateway_reboot`

兼容策略（最佳实践）：

- method 名大小写严格匹配，但允许为 TB 的历史/文档差异提供 **有限别名**（如 `device_deleted` → `gateway_device_deleted`）
- 未识别 method：返回 error（避免 TB 反复重试造成日志风暴），并记录指标 `rpc_unknown_method_total`

### 4.2 参数解析：必须“宽输入、严校验、可观测拒绝”

TB 文档/版本差异可能导致 params 形状变化。建议对每个方法给出兼容解析集合：

- `gateway_device_renamed` params：
  - **形状 1（你提供的事实输入）**：`{"Old device name": "New device name"}`（object，单 key）
  - 兼容形状 2：`{"old":"A","new":"B"}` / `{"oldName":"A","newName":"B"}`
  - 校验：old/new 非空、长度上限、禁止控制字符（至少要可打印）
- `gateway_device_deleted` params：
  - **形状 1（你提供的事实输入）**：`"Removed device name"`（string）
  - 兼容形状 2：`{"device":"name"}` / `{"name":"name"}`
- `remove_provisioned_credentials` params：
  - 允许 `null`/缺失/空 object；任何非空 params 记录 warn 但不失败（可观测）
- `gateway_ping` params：
  - 忽略 params（兼容），返回 `pong` 结构化响应
- `gateway_devices` params：
  - 可选支持 `{"include_status":true,"limit":...}`（未来扩展）；不认识字段忽略
- `gateway_restart` / `gateway_reboot` params：
  - 允许 `{"delay_ms":...,"reason":"..."}`；但必须配置开启才执行

### 4.3 响应语义（强推荐的稳定 JSON）

TB 对响应 body 的严格性依赖具体版本/前端展示，但最佳实践是返回稳定 JSON：

```json
{
  "success": true,
  "method": "gateway_ping",
  "ts_ms": 1710000000000,
  "result": { ... }
}
```

失败时：

```json
{
  "success": false,
  "method": "gateway_restart",
  "ts_ms": 1710000000000,
  "error": {
    "code": "forbidden",
    "message": "gateway_reboot is disabled by configuration"
  }
}
```

这样 TB UI/日志/审计都更容易消费，同时保持跨版本稳定。

---

## 5. 核心架构设计（高性能 + 语义完整）

### 5.1 新增 TB 插件私有“控制面上下文”：`ThingsBoardControlPlane`

在 `ng-gateway-northward/thingsboard` 内新增一个轻量组件（概念设计）：

- **职责**
  - 解析/执行 TB Gateway Service RPC
  - 持有 TB 插件私有状态（alias/tombstone/最近请求去重）
  - 负责向 TB 发布 RPC response（使用 `AsyncClient`）
  - 与 `ExtensionStore` 交互以持久化 alias/tombstone 与 provision creds 状态

- **关键约束（性能）**
  - 控制面“短路径”：解析 + O(1) 查表 + 轻量写入
  - 持久化采用异步批量/去抖（debounce），避免频繁写 store
  - 严禁在 MQTT event loop 的同一任务里做长耗时 I/O；必要时 spawn

### 5.2 状态模型：TB device name 绑定（alias）与删除墓碑（tombstone）

#### 5.2.1 为什么需要 alias？

TB 以 **device name** 作为子设备主键。平台 rename 后，网关必须从此刻起使用新 name 上报，否则会产生“新旧设备并存/数据分裂”。

但 ng-gateway 的内部主键更适合使用 `device_id`（数据库/运行时主键）。因此最佳实践是：

- **内部以 `device_id` 为权威主键**
- **对 TB 出站时，用 `device_id -> tb_device_name` 的映射替换出站 name**

#### 5.2.2 建议的数据结构（O(1) + 低分配）

- `DashMap<i32, TbDeviceBinding>`：`device_id -> { tb_name: Arc<str>, deleted: bool, updated_at_ms: i64 }`
- `DashMap<Arc<str>, i32>`：`tb_name -> device_id`（用于 rename/delete 通过 name 反查）
- `DashSet<i32>` 或 `DashMap<i32, Tombstone>`：记录“平台已删除，不应再上报”的设备（避免 delete 后被网关上报复活）

**注意**：出站热路径必须只做 `device_id -> tb_name` 一次查表，不做字符串拼接、不做线性搜索。

### 5.3 设备索引：利用 `NorthwardRuntimeApi::subscribe_runtime_delta()` 构建低成本 device inventory

当前 `NorthwardRuntimeApi` 只有 `list_point_meta()`，但它同时提供了 `subscribe_runtime_delta()`（`RuntimeDelta::DevicesChanged` 内含 `RuntimeDevice`，可获得 `id()` 与 `device_name()`）。

最佳实践：

- TB 插件在启动后 spawn 一个轻量任务订阅 runtime delta，维护：
  - `device_id -> (local_name, status, channel_id, type)`（用于 `gateway_devices` 响应与 rename/name 解析）
- 这样 `gateway_devices` 不需要遍历 `list_point_meta()` 去重，避免不必要分配

### 5.4 请求去重与幂等

TB RPC 可能重试（网络抖动、QoS1、平台重发）。最佳实践：

- 以 `request_id`（从 `v1/devices/me/rpc/request/<id>` 的 topic 尾部提取）为幂等键
- 保持一个固定大小的 LRU/环形缓存（例如 1024~4096）：
  - 已处理的 request_id → 直接复用上次响应（或返回 success/noop）
  - 避免“重复 rename/delete”带来的频繁持久化与日志噪声

### 5.5 具体方法语义（推荐实现）

#### 5.5.1 `gateway_ping`

- **语义**：平台探活；不应触发任何状态变更
- **响应**：`{success:true, result:{pong:true, uptime_ms, ts_ms}}`
- **性能**：常数时间；不访问 DB/Store

#### 5.5.2 `gateway_devices`

- **语义**：返回当前网关“可见的设备清单”
- **数据来源**：TB 插件维护的 `device_index`（来自 runtime delta）
- **响应**：
  - `devices`: `[{ id, name, tb_name, status, type, channel_id }]`
  - `tb_name` 通过 alias 计算；`deleted=true` 的设备可选择不返回或标记 `deleted`

#### 5.5.3 `gateway_device_renamed`（含 `device_renamed` 别名）

- **语义**：平台侧对子设备重命名；网关必须从此刻起用新 name 上报，避免数据分裂
- **处理流程**
  - 解析 old/new
  - 通过 `tb_name->device_id` 或 `local_name->device_id` 解析到内部设备
  - 更新 `device_id -> tb_name = new`（last-writer-wins，记录 updated_at）
  - 如果设备之前 tombstone（deleted），默认仍保持 deleted（避免 rename 复活）
- **响应**：success + 映射结果（resolved_device_id, old, new）

#### 5.5.4 `gateway_device_deleted`（含 `device_deleted` 别名）

- **语义**：平台侧删除子设备；网关必须避免继续上报导致设备被“复活”
- **处理流程**
  - 解析 removed_name
  - 解析到 device_id（如果找不到也应 success/noop，但要记录指标 `rpc_delete_unknown_device_total`）
  - 写入 tombstone（device_id 标记 deleted）
  - 可选：触发对 TB 的 `gateway_disconnect`（如果你们希望在 TB 侧显示断开）
- **出站行为**
  - tombstone 命中的设备：TB 插件对该设备的 telemetry/attributes/connect/disconnect **全部 drop**
  - drop 必须有指标 `tb_outbound_dropped_deleted_device_total`
- **响应**：success + 删除结果

#### 5.5.5 `remove_provisioned_credentials`

- **语义**：平台要求撤销已 provision 的凭据
- **处理流程（最佳实践）**
  - 仅对 `connection.mode = provision` 生效；其他模式返回 success/noop + reason
  - 从 `ExtensionStore` 删除存储的 credentials（原子替换/删除）
  - 触发 **TB 插件会话重连**（让下一次连接走 provision 流程）
    - 推荐：发送一个“插件自重启”信号给 supervisor（或让 connector 返回一个可控错误促使重连）
- **安全**：这是安全关键路径，必须记录审计日志（app_id、原因、时间）

#### 5.5.6 `gateway_restart` / `gateway_reboot`

这两类命令危险性高，最佳实践是 **默认禁用**，并提供明确的“允许范围”与“执行责任边界”：

- **gateway_restart（推荐语义）**
  - 默认解释为“重启网关进程”（不是仅重启 TB 插件）
  - 但实际执行通常由 systemd/k8s/launchd 等编排系统完成
  - 推荐实现：core 提供平台无关的内部命令 `host.restart`，执行为：
    - 如果有 supervisor/host lifecycle 通道：触发优雅退出（让编排重启）
    - 若无：返回 `forbidden/not_supported`
- **gateway_reboot（推荐语义）**
  - 解释为“重启操作系统/设备”
  - 只在明确配置允许且宿主能力具备（权限）时执行
  - 推荐实现：core 内部命令 `host.reboot`，默认返回 forbidden

TB 插件侧只做 method → internal command 的翻译与回包，不直接在插件里执行系统调用（隔离风险）。

---

## 6. 配置与治理（必须可开关、可灰度）

建议在 `ThingsBoardPluginConfig` 的 `communication` 或新增 `control_plane` 小节引入开关（概念）：

- `control_plane.enabled`（默认 true）
- `control_plane.allowed_methods`（默认仅允许：ping/devices/rename/delete/remove_provisioned_credentials；restart/reboot 默认不在允许列表）
- `control_plane.enable_restart` / `control_plane.enable_reboot`（默认 false）
- `control_plane.deleted_device_policy`：
  - `drop`（默认）：删除后不再上报，避免复活
  - `ignore`：不处理 delete（仅用于兼容/调试）
  - `auto_recreate`：继续上报允许复活（强烈不推荐）
- `control_plane.alias_persistence`（默认 true）：alias/tombstone 是否写入 `ExtensionStore`
- `control_plane.max_alias_entries`（默认 10_000）：防止无限增长
- `control_plane.max_rpc_payload_bytes`（默认 64KB）：超过则拒绝
- `control_plane.rate_limit`（可选）：每分钟最大 RPC 数（防 DoS）

---

## 7. Phase 推进方案（可验证、可灰度、可回滚）

### Phase 0（1~2 天）：只做观测，不改行为（可选但强建议）

- 目标：在不改变系统行为的情况下，确认 TB 实际下发的 method/params 形状分布（避免“文档与现实不一致”）
- 动作：
  - 记录 method、params 的 JSON 类型、payload bytes、request_id（注意脱敏）
  - 对未知 method 打点
- 产出：
  - 指标与日志样本
  - 兼容解析策略定稿

### Phase 1（2~4 天）：实现低风险方法（ping/devices）+ RPC 框架落地

- 目标：建立 TB 插件内的 `ThingsBoardControlPlane` 基础能力（解析、响应、指标、限流）
- 范围：
  - `gateway_ping`
  - `gateway_devices`（数据先从 runtime delta 索引；若来不及，可退化为 `list_point_meta()` 去重）
- 交付验收：
  - TB 侧能看到 RPC 正常响应
  - 不影响 telemetry 热路径吞吐

### Phase 2（3~6 天）：实现一致性关键方法（rename/delete）并影响出站 name

- 目标：彻底解决“平台 rename/delete 导致数据分裂/复活”的一致性问题
- 范围：
  - `gateway_device_renamed`（含别名）
  - `gateway_device_deleted`（含别名）
  - alias/tombstone 持久化（`ExtensionStore`）
  - 出站路径改造：对 TB publish 时做 `device_id -> tb_name` 重写 + tombstone drop
- 灰度策略：
  - 引入 `control_plane.deleted_device_policy` 与 `alias_persistence` 开关
  - 默认只对 TB 插件生效，不影响其他 northward

### Phase 3（2~4 天）：实现 `remove_provisioned_credentials` 并闭环重连

- 目标：安全合规地支持平台撤销凭据
- 范围：
  - 删除 `ExtensionStore` 中存储的 provision creds
  - 触发 TB 插件重连（使其重新 provision）
  - 完整审计日志与指标
- 验收：
  - TB 下发该 RPC 后，网关能在可控时间内重新上线
  - 不出现重连风暴（有退避）

### Phase 4（视需求，2~5 天）：restart/reboot（高风险，默认关闭）

- 目标：在明确安全边界下支持远程生命周期操作
- 范围：
  - TB 插件翻译为内部命令 `host.restart/host.reboot`
  - core 落地 handler（或对接宿主/systemd/k8s）并保证优雅退出
  - 配置强约束：默认禁用，必须显式 enable
- 风险控制：
  - 可选二次确认机制（例如要求 params 中携带一次性 token，或仅允许本地管理 API 触发）

---

## 8. 后续架构演进建议（不阻塞本期，但能显著提升“正确性与可维护性”）

### 8.1 为 gateway-level command 增加“来源上下文”（app_id/plugin_type）

当前 core 的 `GatewayCommandHandler::handle(&Command)` 无法知道命令来自哪个 app。

建议（未来）：

- 在 `Command` 增加 `source_app_id` 或在 core 调用 handler 时额外传入 `app_id`
- 或引入 `per-app gateway command registry`，避免全局命名空间冲突

### 8.2 扩展 `NorthwardRuntimeApi`：提供 `list_devices()`（O(1)）而非从 point_meta 推导

这能让 `gateway_devices` 更精准、更低分配，并且对未来多平台复用更友好。

---

## 9. 结论（建议的落地路径）

- **是的，应该实现** TB 下发的 gateway-level RPC：rename/delete/remove_provisioned_credentials/ping/devices/restart/reboot。
- **最佳实践实现方式**：
  - TB 特有一致性事件（rename/delete/credentials/ping/devices）在 **TB 插件内部** 语义化处理并直接回包
  - restart/reboot 走“平台无关内部命令”，由 core/宿主负责执行，默认禁用
- **Phase 推荐**：先 ping/devices 打通框架，再做 rename/delete 的 alias+tombstone 闭环，最后做凭据清理与重启类高风险操作。

