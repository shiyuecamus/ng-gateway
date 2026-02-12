# ng-gateway 许可证系统（License）最佳实践设计与 Phase 推进计划

> 面向：通过 Docker / Docker Compose / Helm / Homebrew / RPM / DEB 安装的 ng-gateway  
> 目标：默认安装即具备“社区版（受限）”能力；可通过你颁发的许可证激活为“商业版（扩容 + 驱动/插件解锁）”；UI 提供许可证管理与激活入口  
> 约束：高性能、高吞吐、高并发；Rust + Tokio 生态；最小化变更；可离线/可在线；可密钥轮换；可观测、可审计

---

## 1. 背景与核心目标

你希望实现的不是“配置开关”，而是一个**可产品化**的商业化能力：

- **默认限制**：用户不配置许可证也能运行，但受限（通道数、设备数、应用数、点位数、动作数、可用驱动、可用插件等）。
- **可颁发许可证**：你生成并签名一份“许可证证书”（license file / token），用户导入/激活后获得商业权限。
- **UI 管理**：展示许可证详情、有效期、绑定信息、限额/解锁项、状态（有效/过期/未激活/被撤销）、操作（导入/激活/更换/导出请求文件）。
- **多安装形态一致**：Docker、Compose、Helm、brew、rpm、deb 的默认行为一致（均带默认社区许可/默认限制）。
- **安全与可维护**：客户端不包含私钥；支持密钥轮换；尽量抗篡改、可审计；支持离线激活（企业常见）。

---

## 2. 总体架构（高层）

### 2.1 组件划分

建议增加一个独立的 license 子系统（crate/模块），提供：

- **License Verifier**：验证许可证签名与字段合法性（过期、not_before、issuer、kid 等）。
- **Entitlement Engine**：将许可证声明（claims）解析成可执行的权限集（limits + feature flags + allowlists）。
- **Enforcement Points**：在运行时关键入口做限额与解锁项校验（设备创建、通道创建、点位注册、动作创建、驱动加载、插件加载等）。
- **License Store**：许可证存储/加载（文件、DB、K8s Secret 挂载等），并支持热更新（可选）。
- **License API**：后端 HTTP API 提供 UI 管理所需信息与激活入口。

> 推荐：验证与策略计算尽可能“纯函数化”，并将 enforcement 作为轻量 check，以避免在热点路径造成明显开销。

### 2.2 数据流（运行时）

1. 启动时加载 license（若无则使用内置 “Community” 默认策略）。
2. 解析 + 验签 + 生成 `Entitlements`（限额/解锁）。
3. 将 `Entitlements` 放入全局只读上下文（例如 `Arc<Entitlements>`），并在关键业务入口做快速校验：
   - 计数类限制：使用原子/有界计数器（`AtomicU64`）或集中管理器（避免锁竞争）。
   - allowlist 类限制（驱动/插件）：在加载期一次性判断并拒绝不允许的模块。
4. UI 通过 API 展示 license、当前使用量、剩余量、状态。

---

## 3. 许可证产品形态与版本策略

### 3.1 建议的版本分层

- **Community（默认内置）**：固定且较小的限额 + 仅允许部分驱动/插件。
- **Trial（试用）**：短期有效（例如 7/30 天），可放宽部分限额，便于售前评估。
- **Business（商业）**：长期有效或订阅式有效期；支持更高限额、更多驱动/插件。
- **Enterprise（企业）**：在 Business 之上增加高级功能（例如集群/HA、审计、细粒度 RBAC 等），可选“机器绑定/实例绑定”。

### 3.2 许可粒度（建议字段）

- **Limits（数值限制）**：
  - channels_max
  - devices_max
  - apps_max
  - points_max
  - actions_max
  - drivers_max（可选）
  - plugins_max（可选）
- **Allowlist（白名单）**：
  - allowed_drivers: `[String]`
  - allowed_plugins: `[String]`
- **Feature Flags（功能开关）**：
  - enable_xxx: bool（例如 enable_cluster / enable_opcua_write / enable_rule_engine 等）
- **Binding（绑定策略，可选）**：
  - instance_id（强烈建议作为主绑定点）
  - machine_fingerprint（可选，企业版启用）

> 最佳实践：优先绑定到你可控且可配置的 `instance_id`（由安装时生成并持久化），避免容器/K8s 环境硬件指纹不稳定导致误伤。

---

## 4. 安全设计（签名、密钥轮换、防篡改）

### 4.1 签名算法与密钥管理

- **算法**：Ed25519（高性能、实现成熟、密钥短、签名快）。
- **服务端**：你在“许可证颁发服务”持有私钥（建议 HSM 或最少离线保管），负责签名。
- **客户端（网关）**：只内置公钥（可内置多个公钥用于轮换），通过 `kid` 选择对应公钥验签。

> 关键点：客户端永远不应包含签名私钥；license file 只需要可验证，不需要可解密。

### 4.2 License Token 格式

建议使用“**Canonical JSON + 签名**”或“JWT-like 结构（但不强依赖 JWT 生态）”。Rust 里实现时更推荐明确结构：

- `claims_json`: 规范化 JSON（字段顺序、无多余空白、严格类型）
- `signature`: Ed25519 对 `claims_json` 的签名
- `kid`: 公钥 id
- `format_version`: 便于升级

文件可命名为 `license.ngl`（ng-license），内容为：

```json
{
  "format_version": 1,
  "kid": "2026-01",
  "claims": { ... },
  "signature": "base64..."
}
```

### 4.3 防重放/时间相关字段

建议 claims 含：

- `iat`（签发时间）
- `nbf`（not before）
- `exp`（过期）
- `issuer`
- `license_id`（唯一）
- `subject`（客户/组织/合同号）

并在客户端校验：

- `now < nbf` 拒绝
- `now >= exp` 过期
- `issuer` 必须匹配
- `format_version` 必须支持

> 时间漂移：建议允许一个小的 clock skew（例如 5 分钟），并在 UI 提示系统时间异常。

### 4.4 撤销（Revocation）

撤销有两种常见策略：

1. **在线撤销列表**：网关定期拉取 `revoked_license_ids`（需要北向网络）。
2. **离线撤销**：通过发布新的“策略包”或强制要求定期在线校验（订阅制常见）。

最佳实践建议：

- **Trial / Subscription**：要求周期性在线校验（例如每 7 天必须与许可服务握手一次）。
- **永久授权**：可不强制在线，但可选支持手工导入撤销列表。

---

## 5. 默认限制如何覆盖所有安装形态

你的需求是“用户无论怎么装，默认都有一定限制”。最佳实践是：

- **二进制内置默认 Community 策略**（最可靠，和安装方式无关）
- 并允许通过文件覆盖（license file）

这样：

- Docker/Compose/Helm：无需额外打包文件也能默认受限；需要商业版时只要挂载 license 文件或走激活 API。
- brew/rpm/deb：同理；可额外安装一个示例 license 路径提示。

> 不建议把默认限制只放在配置文件里，因为用户可能删/改配置，且不利于一致性。

---

## 6. 运行时 enforcement（限额与解锁项强制点）

### 6.1 “计数类限额”建议的强制点

必须在“创建/注册”入口处强制，而不是只在 UI 做限制：

- **通道**：创建 channel 的 API/命令入口；以及加载配置批量创建时。
- **设备**：设备注册/创建入口；协议 southward driver 上报注册也需要经过统一入口。
- **应用**：应用创建入口。
- **点位**：点位/测点注册入口；批量导入也要校验。
- **动作**：规则/动作创建入口。

实现建议：

- 用一个 `UsageMeter` 维护当前计数（来自内存 + 持久化恢复）。
- 对于“动态变化”的资源（设备上下线）：
  - 限额通常指“已注册资源数量”而非“在线数量”，更符合商业。

### 6.2 “可用驱动/插件”建议强制点

强制点在“加载/初始化”阶段，避免运行中出现半初始化状态：

- driver manager：加载驱动前先检查 `allowed_drivers`。
- plugin manager：加载插件前先检查 `allowed_plugins`。

> 最佳实践：如果某驱动/插件不被允许，应该以可读错误提示并记录审计日志，但不影响其他已允许模块启动。

### 6.3 高性能要求下的实现策略

- 将 `Entitlements` 放在 `Arc` 里，校验时仅做：
  - 少量整数比较
  - `HashSet` 查询（驱动/插件 allowlist）
- 计数的变化点（创建/删除）频率远低于数据采集热点路径，所以不会成为瓶颈。

---

## 7. 激活流程设计（在线 + 离线）

### 7.1 在线激活（推荐默认）

1. 网关生成 `ActivationRequest`（包含 instance_id、版本、部署形态、可选 machine fingerprint）。
2. UI 调用 `POST /api/license/activate` 将 request 发到你的许可服务（或网关后端代转）。
3. 许可服务返回签名后的 `license.ngl`。
4. 网关写入 License Store，热加载生效。

### 7.2 离线激活（企业常见）

1. 网关生成 `activation-request.json`（可下载）。
2. 客户发给你（邮件/工单）。
3. 你离线签发 license 文件。
4. 客户在 UI 上传 license（`POST /api/license/import`），立即生效。

---

## 8. UI/UX 设计（许可证管理菜单）

建议在 UI 增加“许可证”一级菜单：

- **概览卡片**
  - 当前版本：Community/Trial/Business/Enterprise
  - 状态：有效/过期/未安装/即将过期（<= 7 天）
  - 有效期：nbf/exp
  - 绑定信息：instance_id（可复制）
  - license_id、issuer、kid
- **限额与用量**
  - channels/devices/apps/points/actions：Max / Used / Remaining
  - drivers/plugins：Allowed 列表（可搜索）
- **操作区**
  - 导入许可证文件
  - 生成激活请求（下载）
  - 在线激活（输入激活码/许可证 key）
  - 替换许可证（需确认）
- **审计日志（可选 Phase 后置）**
  - 激活/导入/失败原因/时间/操作者

---

## 9. Rust 侧模块与接口设计（Demo 级别）

> 下面代码片段以“将来要落地到新 crate `ng-gateway-license`”为目标，字段/注释为英文，便于团队维护与 Rustdoc 生成。

### 9.1 数据模型（claims / entitlements）

```rust
/// License format version supported by the gateway.
pub const LICENSE_FORMAT_VERSION: u32 = 1;

/// High-level edition of the product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LicenseEdition {
    Community,
    Trial,
    Business,
    Enterprise,
}

/// Numeric limits enforced by the gateway.
#[derive(Debug, Clone)]
pub struct LicenseLimits {
    /// Maximum number of channels that can be created/registered.
    pub channels_max: u64,
    /// Maximum number of devices that can be created/registered.
    pub devices_max: u64,
    /// Maximum number of applications that can be created/registered.
    pub apps_max: u64,
    /// Maximum number of points (tags) that can be created/registered.
    pub points_max: u64,
    /// Maximum number of actions/rules that can be created/registered.
    pub actions_max: u64,
}

/// Feature flags used to unlock paid capabilities.
#[derive(Debug, Clone, Default)]
pub struct LicenseFeatures {
    /// Whether the rule engine is enabled.
    pub enable_rule_engine: bool,
    /// Whether clustering/HA is enabled.
    pub enable_cluster: bool,
    /// Whether write-back for OPC UA is enabled.
    pub enable_opcua_write: bool,
}

/// A normalized entitlement set derived from license claims.
#[derive(Debug, Clone)]
pub struct Entitlements {
    pub edition: LicenseEdition,
    pub limits: LicenseLimits,
    pub features: LicenseFeatures,
    /// Allowed driver identifiers (e.g. "opcua", "modbus", "iec104").
    pub allowed_drivers: std::collections::HashSet<String>,
    /// Allowed plugin identifiers.
    pub allowed_plugins: std::collections::HashSet<String>,
    /// Bound instance id (recommended binding target).
    pub bound_instance_id: Option<String>,
    /// License unique id for auditing/revocation.
    pub license_id: Option<String>,
    /// Not before (unix seconds).
    pub nbf: Option<i64>,
    /// Expiration time (unix seconds).
    pub exp: Option<i64>,
}
```

### 9.2 默认 Community 策略（内置）

```rust
/// Build the built-in Community entitlements.
///
/// This must be compiled into the binary to guarantee consistent behavior
/// across docker/helm/brew/rpm/deb installations.
pub fn default_community_entitlements() -> Entitlements {
    Entitlements {
        edition: LicenseEdition::Community,
        limits: LicenseLimits {
            channels_max: 4,
            devices_max: 64,
            apps_max: 2,
            points_max: 5_000,
            actions_max: 32,
        },
        features: LicenseFeatures {
            enable_rule_engine: true,
            enable_cluster: false,
            enable_opcua_write: false,
        },
        allowed_drivers: ["modbus", "opcua"].into_iter().map(|s| s.to_string()).collect(),
        allowed_plugins: ["builtin-metrics"].into_iter().map(|s| s.to_string()).collect(),
        bound_instance_id: None,
        license_id: None,
        nbf: None,
        exp: None,
    }
}
```

### 9.3 验签接口（Verifier）

```rust
/// A parsed license file, including claims payload and signature metadata.
pub struct SignedLicense {
    /// Format version of the license container.
    pub format_version: u32,
    /// Key id used to select the verification public key.
    pub kid: String,
    /// Canonical JSON bytes of the claims (must be verified as-is).
    pub claims_json: Vec<u8>,
    /// Signature bytes (Ed25519).
    pub signature: Vec<u8>,
}

/// Verifies signature and returns normalized entitlements.
pub trait LicenseVerifier: Send + Sync {
    /// Verify the license signature and decode entitlements.
    ///
    /// Implementations must NOT panic; always return a meaningful error.
    fn verify(&self, license: &SignedLicense, now_unix_sec: i64) -> Result<Entitlements, LicenseError>;
}
```

### 9.4 Enforcement API（热点校验要“快”）

```rust
/// A fast, read-only policy interface for checking license constraints.
pub trait LicensePolicy: Send + Sync {
    /// Returns current entitlements snapshot.
    fn entitlements(&self) -> &Entitlements;

    /// Returns true if the given driver is allowed.
    fn is_driver_allowed(&self, driver_id: &str) -> bool;

    /// Returns true if the given plugin is allowed.
    fn is_plugin_allowed(&self, plugin_id: &str) -> bool;

    /// Check whether creating one more channel would exceed the limit.
    fn can_create_channel(&self, current_channels: u64) -> bool;
}
```

> 建议：`current_channels` 等计数从你的资源管理器拿到，policy 仅做比较，不负责读写状态。

---

## 10. 后端 API 设计（UI 对接）

### 10.1 建议的 REST API

- `GET /api/license`
  - 返回当前 license 状态、edition、claims、entitlements、用量统计、校验错误（如过期）
- `POST /api/license/import`
  - 上传 license 文件（multipart 或 JSON）
- `POST /api/license/activation-request`
  - 返回 activation request（用于离线激活下载）
- `POST /api/license/activate`
  - 在线激活：提交激活码/订单号 + activation request；成功后写入 license

### 10.2 响应体字段建议

- status: `valid | invalid | expired | not_installed | not_yet_valid`
- edition
- license_id, issuer, kid, nbf, exp
- limits: max + used + remaining
- allowed_drivers/plugins
- last_error（用于 UI 提示）

---

## 11. 打包与部署对接（Docker/Compose/Helm/brew/rpm/deb）

### 11.1 统一约定的 license 文件路径

建议约定一个统一路径（按 OS/容器自动选择），例如：

- Linux/macOS：`/etc/ng-gateway/license.ngl`（或 `~/.config/ng-gateway/license.ngl`）
- Docker：容器内同样路径，通过 volume mount 覆盖
- Helm：通过 `Secret` 挂载到该路径

并提供环境变量覆盖：

- `NG_GATEWAY_LICENSE_PATH=/path/to/license.ngl`

### 11.2 Helm 建议

- `values.yaml` 支持：
  - `license.existingSecret`
  - `license.mountPath`
- Secret key：`license.ngl`

### 11.3 RPM/DEB/Homebrew 建议

- 安装时创建配置目录
- 在 README 输出：
  - 如何查看 instance_id
  - 如何导入 license
  - UI 激活路径

---

## 12. 可观测性与审计

### 12.1 Metrics（建议）

- `license_status{status="valid|expired|invalid|not_installed"} 1`
- `license_expiration_seconds`（距离 exp 的秒数）
- `license_limit_used{type="devices|channels|points|actions"}` 与 `license_limit_max{...}`
- `license_driver_allowed{driver="opcua"}`（可选）

### 12.2 日志与审计事件（建议）

关键事件：

- license_loaded / license_verified
- license_imported / license_activated
- license_verification_failed（包含原因但避免泄露敏感信息）
- license_limit_exceeded（包含资源类型与当前值）

---

## 13. Phase 推进计划（从 0 到可售卖）

### Phase 0（1-2 天）：需求冻结与关键点对齐

- 明确限制项口径（注册数 vs 在线数）
- 明确驱动/插件的 id 命名规范（用于 allowlist）
- 明确 instance_id 生成策略与持久化路径

交付：

- 本文档确认版

### Phase 1（3-5 天）：最小可用许可证框架（PoC）

目标：没有“激活服务”也能跑通“默认限制 + 导入 license 文件解锁”的闭环。

- 实现 `Entitlements` + 默认 Community 策略
- 实现 `LicenseStore`（从固定路径加载文件）
- 实现 Ed25519 验签（内置公钥）
- 在 2-3 个关键入口做 enforcement（例如：设备创建、通道创建、驱动加载）
- 提供 `GET /api/license` 返回状态

交付：

- CLI 或日志能看到 license 状态
- UI 先不做，先用 curl 验证

### Phase 2（3-7 天）：UI 许可证管理（基础版）

- UI 菜单：许可证概览 + 限额/用量 + 导入文件
- 后端：`POST /api/license/import`
- 热加载（可选）：导入后无需重启

交付：

- Demo：导入 license 后限额提升、驱动/插件解锁

### Phase 3（5-10 天）：在线激活服务（商业化关键）

- 实现 License Issuer 服务（可先独立小服务）
  - 订单/激活码 -> 生成 claims -> 签名 -> 返回 license
- 网关端：`POST /api/license/activate`（或 UI 直连许可服务）
- 加入 `kid` 与公钥轮换机制

交付：

- Demo：输入激活码即可激活商业版

### Phase 4（5-10 天）：离线激活、撤销与订阅校验（企业能力）

- 离线 activation request 下载
- 手工导入 license
- 可选：撤销列表与周期性在线校验（订阅制）

交付：

- 企业客户可离线部署完成激活

### Phase 5（持续迭代）：全面 enforcement + 细粒度功能开关

- 覆盖所有资源创建入口
- 覆盖所有 driver/plugin 加载点
- 增加更多 feature flags（按商业路线图）
- 增加审计日志页面（可选）

---

## 14. 风险与对策（真实世界经验）

- **容器/集群硬件绑定不稳定**：优先绑定 `instance_id`，机器指纹作为可选增强。
- **用户篡改二进制绕过限制**：license 属于“软防护”，要用法律/合同与服务端能力（订阅校验/审计）配合；同时可做基础反篡改（校验码、关键路径集中校验），但不要牺牲稳定性。
- **时间被手工回拨**：允许小 skew，但对 Trial/Subscription 强制周期在线校验。
- **性能回归**：enforcement 只在创建/加载入口；热点采集路径避免锁与复杂逻辑。

---

## 15. 下一步建议（我建议你立刻做的 3 件事）

1. 先确定默认 Community 限额与允许的 driver/plugin 清单（这是产品策略）。
2. 确定 `instance_id` 的生成与持久化位置（这是绑定与激活的根）。
3. Phase 1 先打通“导入 license 文件 -> 解锁驱动/限额”的闭环，再做在线激活服务。

