# NG Gateway 量产烧录工位操作规程 (SOP)

> 适用于: Orange Pi 5 Plus (RK3588) + eMMC
> 烧录方式: Windows PC + RKDevTool + USB Maskrom
> 不需要: 设备网络 / SSH / 键盘 / 显示器 / SD 卡

---

## 1. 工位 PC 环境准备 (一次性)

### 1.1 安装 Rockchip USB 驱动

1. 下载 [DriverAssistant v5.0+](https://dl.radxa.com/tools/windows/DriverAssitant_v5.0.zip)
2. 解压，以管理员权限运行 `DriverInstall.exe`
3. 点击"安装驱动"

### 1.2 安装 RKDevTool

1. 下载 [RKDevTool v2.96+](https://dl.radxa.com/tools/windows/RKDevTool_Release_v2.96_zh.zip)
2. 解压到固定目录，如 `D:\Tools\RKDevTool\`
3. 直接运行 `RKDevTool.exe`（无需安装）

### 1.3 下载 Loader

下载 RK3588 SPL Loader:
- [rk3588_spl_loader_v1.15.113.bin](https://dl.radxa.com/rock5/sw/images/loader/rk3588_spl_loader_v1.15.113.bin)

### 1.4 建立标准工位目录

```
D:\ng-gateway-factory\
├── loader\
│   └── rk3588_spl_loader_v1.15.113.bin
├── images\
│   └── ng-gateway-v1.0.0\
│       ├── ng-gateway-v1.0.0.img              ← 量产主产物 (raw 镜像)
│       ├── ng-gateway-v1.0.0.img.sha256
│       ├── ng-gateway-v1.0.0.img.zst           ← 归档/传输用
│       ├── ng-gateway-v1.0.0.img.zst.sha256
│       └── ng-gateway-v1.0.0.manifest.json
├── scripts\
│   └── flash-preflight.ps1                      ← 工位预检脚本
└── docs\
    └── FLASH-SOP.md                            ← 本文件
```

> `.img.zst` 是压缩格式，用于存储和传输。烧录前必须解压为 `.img`。
> 如果收到的是 `.img.zst`，使用 [zstd](https://github.com/facebook/zstd/releases) 解压:
> `zstd -d ng-gateway-v1.0.0.img.zst`

---

## 2. 逐台烧录操作步骤

### Step 0: 运行工位预检 (必做)

在 PowerShell 中执行：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
D:\ng-gateway-factory\scripts\flash-preflight.ps1 `
  -FactoryRoot "D:\ng-gateway-factory" `
  -ImageVersion "v1.0.0"
```

预检通过后，才允许打开 RKDevTool 继续烧录。

### Step 1: 设备进入 Maskrom 模式

1. 找到板子上的 **Maskrom 按钮**
2. **按住** Maskrom 按钮不松
3. 用 USB Type-C 数据线连接设备到工位 PC
4. 给设备上电（或 USB 供电）
5. 等待 RKDevTool 底部状态栏显示 **"发现一个 MASKROM 设备"**
6. 松开 Maskrom 按钮

### Step 2: 配置 RKDevTool

1. 打开 RKDevTool
2. 切换到 **"高级功能"** 选项卡
3. **Loader** 一栏选择: `D:\ng-gateway-factory\loader\rk3588_spl_loader_v1.15.113.bin`
4. **Image** 一栏选择: `D:\ng-gateway-factory\images\ng-gateway-v1.0.0\ng-gateway-v1.0.0.img`
5. **Storage** 选择: `eMMC`

### Step 3: 执行写入

1. 点击 **"按地址写"** (Write by Address)
2. 起始地址固定为: `0x00000000`
3. 等待进度条完成
4. 显示 **"Download image OK"** 表示写入成功

> 为避免工位操作歧义，量产统一使用 **"按地址写" + `0x00000000`** 作为唯一标准动作，不混用其他按钮路径。

### Step 4: 完成

1. 断开 USB
2. 设备转入下一工位（首次启动 / QA）

---

## 3. 首次启动 (自动，无需人工干预)

烧录完成后，设备首次上电时会自动执行:

1. GPT 分区表修复
2. rootfs 分区自动扩展到 eMMC 全部可用空间
3. ext4 文件系统在线扩容
4. machine-id 重新生成（每台设备唯一）
5. SSH host keys 重新生成
6. AP 热点重新初始化（SSID 含当前设备 MAC 后缀）

整个过程约 30-60 秒，完成后设备自动进入正常运行状态。

> 若 first-boot 中的关键步骤（扩容、machine-id、SSH host key）失败，脚本会**直接失败退出且不会写入完成标记**，便于 QA 或返修工位发现并重试，不会出现“初始化没做完却永远跳过”的隐蔽状态。

---

## 4. QA 验证

### 4.1 Smoke QA (每台必做)

- [ ] 设备上电后 AP 热点广播 `NG-Gateway-XXXX`
- [ ] 手机/PC 连接 AP 热点
- [ ] 浏览器访问 `http://10.47.0.1` 可看到 Web UI
- [ ] HTTP 健康检查: `http://10.47.0.1:8978/health` 返回 200

### 4.2 Full QA (抽检或首批必做)

通过 SSH 或串口登录后执行:

```bash
sudo bash /opt/ng-gateway/scripts/verify-image.sh
```

期望结果: `Overall: PASS`

---

## 5. 常见问题

### Q: RKDevTool 没有检测到设备？

- 确认 DriverAssistant 已正确安装
- 确认 USB 线是数据线（不是纯充电线）
- 确认设备在 Maskrom 模式（先按住按钮再上电）

### Q: 写入后设备无法启动？

- 确认使用的是 raw `.img` 文件，不是 `.img.zst` 压缩文件
- 确认 Loader 版本正确
- 确认起始地址是 `0x00000000`

### Q: 首次启动后 AP 没有广播？

- 等待 60-90 秒，首启初始化需要时间
- 如仍无广播，通过串口查看 `journalctl -b -u ng-gateway-first-boot.service`

---

## 6. 镜像命名规范

```
ng-gateway-{version}.img              量产主产物
ng-gateway-{version}.img.sha256       raw 校验
ng-gateway-{version}.img.zst          归档/传输
ng-gateway-{version}.img.zst.sha256   压缩校验
ng-gateway-{version}.manifest.json    产物清单
```

其中 `{version}` 格式为 `vX.Y.Z`，例如 `v1.0.0`。

---

## 7. 版本控制

| 项目 | 说明 |
| --- | --- |
| 镜像版本 | 记录在 `manifest.json` 的 `version` 字段 |
| Loader 版本 | `rk3588_spl_loader_v1.15.113.bin`，升级需同步更新本文档 |
| RKDevTool 版本 | v2.96+，建议统一工位版本 |
| 驱动版本 | DriverAssistant v5.0+ |
