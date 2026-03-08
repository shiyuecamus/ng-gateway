# Linux Packaging And Factory Flow

`deploy/linux/` 负责 Linux 平台的三类事情：

1. **打包**：把 `ng-gateway` 构造成 `.deb` / `.rpm`
2. **运行时安装**：定义设备安装后的 `/opt/ng-gateway`、`/etc/ng-gateway`、`/var/lib/ng-gateway` 布局
3. **工厂量产**：黄金样机封板、镜像导出、烧录、QA 校验

这三类职责的生命周期不同，所以脚本目录按职责做了分层，但**不会为了分层而增加设备运行时路径复杂度**：

- `packaging/`：CI / 发布入口
- `factory/`：工厂 / 量产工具
- `runtime/`：仓库中的运行时脚本源码
- `shared/`：运行时 / 工厂共享函数
- `hooks/`：包管理器 hook

## 目录分层

```text
deploy/linux/
├── README.md
├── nfpm/                        # nfpm 模板
├── resources/                   # 默认配置资源
├── systemd/                     # systemd unit 模板
└── scripts/
    ├── packaging/               # 打包入口（仓库/CI 使用）
    │   ├── package.sh
    │   ├── stage-rootfs.sh
    │   └── render-nfpm-config.sh
    ├── factory/                 # 工厂/量产工具（样机/工位使用）
    │   ├── golden-sanitize.sh
    │   ├── create-golden-image.sh
    │   └── flash-image.sh
    ├── hooks/                   # 包管理器 hook
    │   ├── postinstall.sh
    │   ├── preremove.sh
    │   └── postremove.sh
    ├── runtime/                 # 运行时脚本源码（仓库内）
    │   ├── init-network.sh
    │   ├── ap-setup.sh
    │   ├── ap-teardown.sh
    │   ├── ap-auto-provision.sh
    │   ├── first-boot-resize.sh
    │   └── verify-image.sh
    └── shared/
        └── _common.sh
```

## 分层原则

| 层级 | 典型脚本 | 谁来执行 | 是否进入最终设备安装产物 |
| --- | --- | --- | --- |
| `packaging/` | `package.sh`、`stage-rootfs.sh` | CI / 发布工程师 | 否 |
| `factory/` | `golden-sanitize.sh`、`create-golden-image.sh`、`flash-image.sh` | 工厂 / 工位 / 样机制作人员 | 否 |
| `runtime/` | `init-network.sh`、`first-boot-resize.sh`、`verify-image.sh` | 安装脚本 / systemd / QA | 其产物会进入设备 |
| `shared/` | `_common.sh` | 运行时 / 工厂脚本共享 | 会以 `_common.sh` 名称进入设备 |
| `hooks/` | `postinstall.sh`、`preremove.sh`、`postremove.sh` | `dpkg` / `rpm` / `nfpm` | 以 hook 形式引用 |

关键规则：

- **只有设备运行真正依赖的脚本**才会被 `stage-rootfs.sh` 复制到 `/opt/ng-gateway/scripts`
- **工厂工具**保留在仓库、SD 制作系统或工位环境，不默认塞进最终设备
- **不保留兼容 wrapper**，仓库内统一使用 canonical 路径

## Canonical 入口

推荐以后优先使用这些路径：

### 打包

```bash
bash deploy/linux/scripts/packaging/package.sh --format deb
bash deploy/linux/scripts/packaging/package.sh --format rpm
```

### 工厂工具

```bash
bash deploy/linux/scripts/factory/golden-sanitize.sh
bash deploy/linux/scripts/factory/create-golden-image.sh --help
bash deploy/linux/scripts/factory/flash-image.sh --help
```

### 运行时/设备侧

```bash
/opt/ng-gateway/scripts/init-network.sh
/opt/ng-gateway/scripts/first-boot-resize.sh
/opt/ng-gateway/scripts/verify-image.sh
```

## 设备安装后的实际布局

包安装完成后，运行时关注的是这套目录，而不是仓库里的 `deploy/linux/scripts/`：

```text
/opt/ng-gateway/
├── bin/ng-gateway-bin
├── gateway.toml
├── drivers/builtin/
├── plugins/builtin/
├── systemd/
│   ├── ng-gateway.service
│   ├── ng-gateway-ap-setup.service
│   ├── ng-gateway-hostapd.service
│   ├── ng-gateway-dnsmasq.service
│   ├── ng-gateway-ap-auto.service
│   └── ng-gateway-first-boot.service
└── scripts/
    ├── _common.sh
    ├── init-network.sh
    ├── ap-setup.sh
    ├── ap-teardown.sh
    ├── ap-auto-provision.sh
    ├── first-boot-resize.sh
    └── verify-image.sh
```

运行期不会默认携带这些工厂工具：

- `golden-sanitize.sh`
- `create-golden-image.sh`
- `flash-image.sh`

## 打包链路

### `.deb` / `.rpm` 生成

```text
release-publish.yml
  └─ deploy/linux/scripts/packaging/package.sh
      ├─ deploy/linux/scripts/packaging/stage-rootfs.sh
      ├─ deploy/linux/scripts/packaging/render-nfpm-config.sh
      ├─ nfpm/*.tmpl
      ├─ deploy/linux/scripts/hooks/postinstall.sh
      ├─ deploy/linux/scripts/hooks/preremove.sh
      └─ deploy/linux/scripts/hooks/postremove.sh
```

### `stage-rootfs.sh` 负责什么

- 构建 Linux 目标二进制
- 暂存 `/opt/ng-gateway` 安装区内容
- 拷贝内置 drivers/plugins
- 拷贝运行时 systemd unit
- 只拷贝运行期脚本到 `/opt/ng-gateway/scripts`

## 运行时链路

### 正常安装

```text
apt install ./ng-gateway_*.deb
  └─ postinstall.sh
      ├─ 初始化 /etc/ng-gateway 和 /var/lib/ng-gateway
      ├─ 调用 /opt/ng-gateway/scripts/init-network.sh
      ├─ 部署并 enable first-boot service
      └─ enable/start ng-gateway.service
```

### 首次启动

```text
ng-gateway-first-boot.service
  └─ /opt/ng-gateway/scripts/first-boot-resize.sh
      ├─ growpart + resize2fs
      ├─ regenerate machine-id / SSH host keys
      └─ FORCE_REGENERATE=1 init-network.sh
```

### QA

```text
目标设备首启完成后
  └─ /opt/ng-gateway/scripts/verify-image.sh
```

## 工厂链路

### 黄金样机封板

```bash
sudo bash deploy/linux/scripts/factory/golden-sanitize.sh
sudo shutdown -h now
```

### 导出黄金镜像

```bash
sudo bash deploy/linux/scripts/factory/create-golden-image.sh \
  --device /dev/mmcblk1 \
  --output /mnt/usb/ng-gateway-v1.0.0 \
  --version v1.0.0
```

### 烧录目标板

```bash
sudo bash deploy/linux/scripts/factory/flash-image.sh \
  --image /mnt/usb/ng-gateway-v1.0.0.img.zst \
  --device /dev/mmcblk1
```

## 当前终态原则

这版目录整理后，统一遵循下面的终态原则：

1. `packaging/` 和 `factory/` 使用子目录
2. `runtime/` 只影响仓库源码组织，不改变设备内 `/opt/ng-gateway/scripts/*.sh` 路径
3. 不再保留兼容 wrapper
4. 仓库内文档、CI、脚本引用全部指向 canonical 路径
