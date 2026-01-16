# 北向插件文档重构与编写计划 (v2.0 - 深度增强版)

本文档规划 `ng-gateway-ui/docs/src/northward` 下的文档结构与内容。
**核心目标**：不仅教会用户“怎么配”，更要传递“为什么这么配”的**最佳实践**，覆盖高可用、高并发、数据一致性及平台侧的高级交互场景。

## 1. 目录结构规划

```text
src/northward/
├── overview.md                 # [新增] 北向核心架构：缓冲、可靠性、并发模型
├── mqtt/                       # [重构] MQTT v5 标准插件
│   ├── index.md                # 基础配置与使用
│   └── best-practices.md       # [进阶] 共享订阅、TLS双向认证、Retain策略
├── kafka/                      # [新增] Kafka 插件
│   ├── index.md                # 基础配置与认证
│   └── producer-tuning.md      # [进阶] 分区策略、压缩、幂等性与高吞吐调优
├── pulsar/                     # [新增] Pulsar 插件
│   └── index.md
├── thingsboard/                # [新增] ThingsBoard 专用插件
│   ├── index.md                # 基础连接与 Gateway API
│   └── advanced.md             # [进阶] 自动设备注册、属性同步、RPC 闭环
└── opcua-server/               # [新增] OPC UA Server 插件
    ├── index.md                # 基础服务配置
    └── modeling.md             # [进阶] 地址空间映射规则与安全策略
```

---

## 2. 通用北向概念 (`overview.md`)

本章建立用户对网关北向数据链路的**系统级认知**。

-   **数据流向 (Pipeline)**：采集 -> 标准化 (CloudEvent) -> 缓冲队列 (Buffer) -> 批处理 (Batch) -> 发送 (Dispatch)。
-   **断网续传 (Disk-backed Buffer)**：
    -   **机制**：内存队列满后自动溢出到磁盘 (SQLite/RocksDB)，网络恢复后自动回放。
    -   **配置建议**：如何评估磁盘空间与 `max_buffer_size`，防止 IO 饿死。
-   **QoS 降级策略**：
    -   当队列积压严重时，网关如何丢弃低优先级数据（如高频 Telemetry）以保全高优先级数据（如 Alarm/Event）。
-   **数据格式标准**：
    -   JSON (调试友好) vs Protobuf (带宽节省 60%+) 的选型指南。

---

## 3. Kafka 插件深度规划 (`kafka/`)

### 3.1 基础配置 (`index.md`)
-   **连接与认证**：
    -   **Bootstrap Servers**：多节点填写规范。
    -   **SASL 认证矩阵**：PLAIN, SCRAM-SHA-256/512, GSSAPI (Kerberos), OAUTHBEARER 的配置示例。
    -   **SSL/TLS**：Truststore/Keystore 配置（如需要）。

### 3.2 进阶调优与最佳实践 (`producer-tuning.md`)
-   **分区策略 (Partitioning Strategy) —— 核心点**：
    -   **Default (Round-robin)**：高吞吐，但无法保证同一设备的有序性。
    -   **By Key (Device ID)**：**最佳实践**。确保同一设备的数据永远进入同一 Partition，保证时序数据库消费时的严格保序。
    -   **Custom**：按 Tenant 分区（多租户隔离场景）。
-   **可靠性语义 (Reliability)**：
    -   **`acks` 设置**：`1` (Leader) vs `all` (ISR)。推荐 `all` 配合 `min.insync.replicas`。
    -   **幂等性 (Idempotence)**：开启 `enable.idempotence=true` 防止网络抖动导致的数据重复。
-   **吞吐量调优 (Throughput)**：
    -   **Batching**：`batch.size` (如 16KB-64KB) 与 `linger.ms` (如 5-10ms) 的平衡。
    -   **Compression**：推荐 `zstd` 或 `lz4`（CPU 低开销，高压缩比）。
-   **Log Compaction**：
    -   针对 "Device State" 类 Topic，建议开启 Compaction，网关侧需配合发送 Key。

---

## 4. ThingsBoard 插件深度规划 (`thingsboard/`)

### 4.1 基础对接 (`index.md`)
-   **Gateway API 概念**：
    -   解释网关作为 **Gateway Device**，代理子设备（Sub-devices）通讯。
    -   **优势**：子设备无需独立 Token，统一管理，流量节省。
-   **连接配置**：Access Token 获取与填入。

### 4.2 高级特性 (`advanced.md`)
-   **设备自动注册 (Provisioning)**：
    -   **隐式注册**：网关上报数据时，若 TB 查无此 DeviceName，是否自动创建？
    -   **显式注册流程**：通过 `Device Provisioning Service` 接口动态申请 Credentials（适合大规模量产）。
-   **双向属性同步 (Attributes Sync)**：
    -   **Client Attributes**：网关上报设备静态信息（如 `fw_version`, `serial_number`）。
    -   **Shared Attributes**：监听平台下发的配置（如 `sampling_rate`），并自动应用到南向驱动配置中（需配合网关的动态配置能力）。
-   **RPC 远程控制 (Remote Procedure Call)**：
    -   **Server-Side RPC**：平台点击“开闸” -> 网关接收 -> 映射为 Modbus Write Coil -> 返回执行结果。
    -   **超时处理**：如何处理设备离线导致的 RPC 超时。
-   **数据映射细节**：
    -   Telemetry 上报：时间戳对齐（使用采集时间 vs 上报时间）。
    -   Connect/Disconnect 事件上报：让平台感知子设备在线状态。

---

## 5. OPC UA Server 插件深度规划 (`opcua-server/`)

### 5.1 基础与模型 (`index.md`)
-   **反向映射原理**：
    -   将网关内部数据模型（Tenant -> Device -> Point）动态映射为 OPC UA 地址空间 (Address Space)。
-   **地址空间布局 (Address Space Layout)**：
    -   `Root/Objects/NgGateway/{TenantID}/{DeviceID}/{PointID}`
    -   **Variable Node** 配置：DataType 映射（Rust 类型 -> UA Built-in Types）。
-   **服务配置**：
    -   端口、Endpoint URL、Discovery URL。

### 5.2 安全与性能 (`modeling.md`)
-   **安全策略 (Security Policies)**：
    -   None (调试用)。
    -   Basic256Sha256 / Aes128_Sha256_RsaOaep (生产推荐)。
    -   **证书管理**：生成 Server 证书、信任 Client 证书的流程。
-   **用户认证 (User Token Policies)**：
    -   Anonymous, UserName, Certificate。
-   **性能限制**：
    -   `MaxSessionCount`, `MaxNodesPerRead`, `MaxMonitoredItemsPerSubscription`。
    -   订阅模式 (Subscription) vs 轮询模式的资源消耗对比。

---

## 6. MQTT v5 插件增强 (`mqtt/`)

### 6.1 进阶场景 (`best-practices.md`)
-   **Topic 规划最佳实践**：
    -   利用 **Topic Alias** 减少带宽消耗（V5 特性）。
    -   使用 **User Properties** 传递元数据（如 `trace_id`, `sampling_source`）而不污染 Payload。
-   **负载均衡 (Shared Subscription)**：
    -   平台下行指令如果采用 `$share/group/command`，网关集群如何处理（通常网关作为设备端，较少用共享订阅，但在“网关作为数据源给多个后端消费”的场景下需提及）。
-   **保留消息 (Retain)**：
    -   用于“设备最后状态”或“配置信息”的发布，新连接的 Client 可立即获取。
-   **Will Message (遗嘱)**：
    -   配置 LWT (Last Will and Testament) 以便网关意外断电时平台能立即感知“网关离线”。

---

## 7. 文档编写执行标准

### 格式与交互
-   **配置项表格**：Key | Type | Default | Description | Recommended
-   **故障排查树**：使用 Mermaid 流程图绘制排查路径（例如：连接失败 -> 检查网络 -> 检查认证 -> 检查 ACL）。

### 截图规范 (Placeholder)
-   **ThingsBoard**: 设备详情页的“最新遥测”面板、关联子设备列表。
-   **Kafka**: 消费端 CLI 接收到的带 Header 的消息示例。
-   **OPC UA**: UaExpert 视角的节点树结构。

此 V2 版本计划确认后，将按优先级顺序开始编写。
