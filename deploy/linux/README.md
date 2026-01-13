# Linux Packaging (deb/rpm)

## 产物布局（与设计文档一致）

- 只读安装区：`/opt/ng-gateway/`
  - `bin/ng-gateway-bin`
  - `drivers/builtin/*.so`
  - `plugins/builtin/*.so`
  - `gateway.toml`（默认配置，首次安装时由 postinstall 复制到 `/etc`）
- 配置目录：`/etc/ng-gateway/gateway.toml`
- 可写运行目录（WorkingDirectory）：`/var/lib/ng-gateway/`

> 说明：SQLite 数据库文件会在网关首次启动时自动创建与迁移：
> - 路径：`/var/lib/ng-gateway/data/ng-gateway.db`（由 WorkingDirectory + `./data` 决定）
> - drivers/plugins 的 DB 存储路径为运行根目录相对路径（便于迁移 `runtime_dir`）

systemd unit：

- deb：`/lib/systemd/system/ng-gateway.service`
- rpm：`/usr/lib/systemd/system/ng-gateway.service`

## 脚本说明

- `scripts/stage-rootfs.sh`
  - 负责 **构建**（默认全量 + `ui-embedded`）并把文件 staged 到一个 `ROOTFS_DIR` 目录下
  - staged 的 rootfs 会包含：
    - `opt/ng-gateway/...`
    - `etc/ng-gateway/`（空目录）
    - `var/lib/ng-gateway/...`（空目录树）

- `scripts/package-deb.sh`
  - 调用 `stage-rootfs.sh`
  - 渲染 nfpm 模板并生成 `.deb`

- `scripts/package-rpm.sh`
  - 调用 `stage-rootfs.sh`
  - 渲染 nfpm 模板并生成 `.rpm`

- `scripts/postinstall.sh` / `scripts/preremove.sh`
  - nfpm 维护脚本（首次安装创建目录、复制默认配置、停止服务等）

## 依赖

- `cross`：用于 CI 多架构编译（`x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu`）
- `nfpm`：用于生成 `.deb` / `.rpm`
- `ng-gateway-web/ui-dist.zip`：用于 `ui-embedded` 编译期嵌入（CI 会先下载 workflow artifact）

## 本地打包示例（仅做开发调试）

构建 `.deb`（amd64）：

```bash
export PKG_VERSION="1.2.3"
export TARGET_TRIPLE="x86_64-unknown-linux-gnu"
export DEB_ARCH="amd64"

# 若需要多架构/容器构建，可在外层设置：
# export CARGO=cross

bash deploy/linux/scripts/package-deb.sh
```

## 重要说明：rpm 兼容性（glibc/OpenSSL ABI）

rpm 能否在目标发行版上运行，关键取决于 **二进制/插件的构建环境（glibc/OpenSSL ABI）**，而不仅是 nfpm 的元数据与 depends。

Phase 2 的最终验收必须包含：

- CentOS 7/8 实机安装并通过 systemd 启动
- 启动时无 `libssl` / `libsasl` / `zstd` 等动态库缺失或 ABI 不匹配错误

