# Linux Packaging & AP Hotspot Deployment

## 端到端生命周期（完整闭环）

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        全新 Ubuntu 设备                                  │
│  (香橙派 RK3588 / Jetson Orin / x86 工控机)                              │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                     sudo dpkg -i ng-gateway_*.deb
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  postinstall.sh                                                        │
│  ├── 创建运行目录 (/var/lib/ng-gateway, /etc/ng-gateway)                │
│  ├── 复制默认配置 (gateway.toml → /etc/ng-gateway/)                     │
│  ├── 复制驱动/插件 → 运行目录                                            │
│  ├── systemctl daemon-reload                                           │
│  └── 调用 init-network.sh ←─── 核心：AP 首次初始化                       │
│       ├── iw dev / iw phy → 检测 Wi-Fi 硬件                             │
│       ├── 无 Wi-Fi → 静默退出（纯有线网关）                               │
│       ├── 有 Wi-Fi:                                                     │
│       │   ├── 检测 AP 模式支持 + STA+AP 并发能力                         │
│       │   ├── 生成 /etc/ng-gateway/ap-env                               │
│       │   ├── 生成 /etc/ng-gateway/hostapd.conf                         │
│       │   ├── 生成 /etc/ng-gateway/dnsmasq-ap.conf                      │
│       │   ├── 按发行版包管理器安装 hostapd/dnsmasq/iw/iptables (如缺失)   │
│       │   ├── 部署 3 个 systemd unit → /lib/systemd/system/             │
│       │   ├── systemctl enable + start AP 服务                           │
│       │   └── AP 热点开始广播 ✅                                         │
│       └── 输出 SSID / 密码 / IP 信息                                    │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                     systemctl start ng-gateway
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  NG Gateway 进程启动                                                     │
│  ├── NetworkService::new() → 检测 AP systemd 状态                       │
│  ├── Web UI 可用 (http://10.47.0.1:5678)                                │
│  ├── 用户通过手机连接 AP 热点 → 访问 Web UI                               │
│  └── Web UI「网络配置」→ 修改 AP/Wi-Fi/有线网络                           │
│       └── configure_ap() → 重写配置 → restart hostapd                    │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                     systemctl stop ng-gateway
                     (或进程崩溃)
                               │
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  AP 热点仍在广播 ✅ (hostapd 由 systemd 独立管理，Restart=always)         │
│  手机仍可连接热点 (但 Web UI 不可用，因为网关进程停了)                      │
└─────────────────────────────────────────────────────────────────────────┘
```

## 关键设计原则

| 原则 | 说明 |
|------|------|
| **AP 独立于网关进程** | hostapd/dnsmasq 由 systemd 管理，`Restart=always`。杀掉网关进程不影响 AP |
| **配置生成与服务控制分离** | `init-network.sh` 在安装时生成初始配置；网关进程只做运行时配置更新 |
| **无 Wi-Fi 优雅降级** | 纯有线设备上 `init-network.sh` 静默退出，不部署 AP 服务，不报错 |
| **幂等可重入** | 所有脚本可安全重复执行。已存在的配置文件不覆盖（除非 `FORCE_REGENERATE=1`） |
| **配置回滚保护** | `configure_ap()` 先 backup → 写入 → restart → 失败则 restore + restart |

## 产物布局

```
/opt/ng-gateway/                      ← 只读安装区
├── bin/ng-gateway-bin                 ← 网关二进制
├── gateway.toml                       ← 默认配置（首次安装时复制到 /etc）
├── drivers/builtin/*.so               ← 内置南向驱动
├── plugins/builtin/*.so               ← 内置北向插件
├── systemd/                           ← AP systemd unit 模板
│   ├── ng-gateway-ap-setup.service
│   ├── ng-gateway-hostapd.service
│   └── ng-gateway-dnsmasq.service
└── scripts/
    └── init-network.sh                ← 首次网络初始化脚本

/etc/ng-gateway/                       ← 配置目录
├── gateway.toml                       ← 网关主配置
├── env                                ← 环境变量覆盖
├── ap-env                             ← AP 接口/IP 变量 (init-network.sh 生成)
├── hostapd.conf                       ← hostapd 配置 (init-network.sh 生成)
└── dnsmasq-ap.conf                    ← dnsmasq AP 配置 (init-network.sh 生成)

/var/lib/ng-gateway/                   ← 运行时可写目录 (WorkingDirectory)
├── data/ng-gateway.db                 ← SQLite 数据库
├── certs/                             ← TLS 证书
├── drivers/{builtin,custom}/          ← 驱动
├── plugins/{builtin,custom}/          ← 插件
└── logs/                              ← 日志文件

/lib/systemd/system/                   ← systemd units
├── ng-gateway.service                 ← 主网关服务
├── ng-gateway-ap-setup.service        ← AP 接口初始化 (oneshot)
├── ng-gateway-hostapd.service         ← AP 热点 (simple, Restart=always)
└── ng-gateway-dnsmasq.service         ← AP DHCP/DNS (simple, Restart=always)
```

## systemd 服务依赖关系

```
                    multi-user.target
                    ┌───────┴───────┐
                    │               │
            ng-gateway.service   ng-gateway-ap-setup.service (oneshot)
            After=hostapd        │
            Wants=hostapd        ├─ 创建虚拟 AP 接口
                                 ├─ 分配 IP
                                 └─ 配置 iptables NAT
                                         │
                              ┌──────────┴──────────┐
                              │                     │
                    ng-gateway-hostapd       ng-gateway-dnsmasq
                    (simple, Restart=always) (simple, Restart=always)
                    广播 AP 热点              DHCP + DNS for AP clients
```

## 脚本说明

| 脚本 | 触发时机 | 职责 |
|------|---------|------|
| `_common.sh` | 被其他脚本 source | 公共函数库（log/die、设备解析、NAT 规则、包管理器检测等） |
| `stage-rootfs.sh` | CI 打包时 | 编译二进制 + 暂存文件系统布局（含 AP unit + init-network.sh） |
| `package.sh` | CI 打包时 | 调用 stage-rootfs → 渲染 nfpm 模板 → 生成 `.deb` 或 `.rpm`（`--format deb\|rpm`） |
| `render-nfpm-config.sh` | CI 打包时 | 模板变量替换 |
| `postinstall.sh` | `deb/rpm` 安装后 | 创建目录 + 复制配置 + 调用 `init-network.sh` |
| `preremove.sh` | `dpkg -r` 前 | stop + disable 所有服务 + 清理 AP unit 文件 |
| `init-network.sh` | 首次安装时 | 探测硬件 → 生成 AP 配置 → 安装依赖 → 部署 unit → enable+start AP |
| `first-boot-resize.sh` | 首次启动 | 扩展分区 + 重生成 machine-id/SSH keys + AP 重新初始化 |
| `create-golden-image.sh` | 手动执行 | 从黄金样机 eMMC 制作最小化压缩镜像 |
| `flash-image.sh` | 产线工位 | 将镜像烧录到目标 eMMC |
| `verify-image.sh` | 产线 QA | 烧录后自动化验证（启动/服务/网络/数据/唯一性） |

## 手动操作指南

### 全新设备首次部署

```bash
# 1. 安装 DEB 包（自动执行 postinstall → init-network）
sudo dpkg -i ng-gateway_1.0.0_arm64.deb

# 2. 检查 AP 服务状态
systemctl status ng-gateway-hostapd --no-pager
systemctl status ng-gateway-dnsmasq --no-pager

# 3. 启动网关
systemctl enable --now ng-gateway

# 4. 验证
# 手机搜索 Wi-Fi → 连接 NG-Gateway-XXXX → 浏览器访问 http://10.47.0.1:5678
```

### 重新初始化 AP（排障/重置）

```bash
# 强制重新生成配置（覆盖已有文件）
sudo FORCE_REGENERATE=1 bash /opt/ng-gateway/scripts/init-network.sh

# 或手动编辑后重启
sudo vim /etc/ng-gateway/hostapd.conf
sudo systemctl restart ng-gateway-hostapd
```

### 验证 AP 独立性

```bash
# 停止网关进程 → AP 应仍在广播
sudo systemctl stop ng-gateway
# 用手机确认仍可连接 AP 热点

# 重启网关 → AP 不中断
sudo systemctl start ng-gateway
```

### 完全卸载

```bash
sudo dpkg -r ng-gateway
# 配置和数据保留在 /etc/ng-gateway 和 /var/lib/ng-gateway
# 如需彻底清除：
sudo rm -rf /etc/ng-gateway /var/lib/ng-gateway
```

## 依赖

| 工具 | 用途 | 安装方式 |
|------|------|---------|
| `cross` | CI 多架构编译 | `cargo install cross` |
| `nfpm` | 生成 deb/rpm | `go install github.com/goreleaser/nfpm/v2/cmd/nfpm@latest` |
| `hostapd` | AP 热点 | `init-network.sh` 自动安装（apt/dnf/yum/zypper） |
| `dnsmasq(-base)` | DHCP/DNS | Debian 用 `dnsmasq-base`，RPM 系常见为 `dnsmasq` |
| `iw` | 无线能力检测 | `init-network.sh` 自动安装（apt/dnf/yum/zypper） |
| `iptables` | NAT 规则 | `init-network.sh` 自动安装（apt/dnf/yum/zypper） |

## 已知约束

- AP 固定 2.4GHz（`hw_mode=g`），不支持 5GHz AP
- 首次安装时需要网络连接以安装 hostapd/dnsmasq（如离线部署需预装）
- `init-network.sh` 会自动识别包管理器（apt/dnf/yum/zypper）；若都不存在则跳过自动安装并给出告警
- STA+AP 共存是否可用取决于具体 Wi-Fi 芯片（RTL8852BE 可能有驱动问题）
