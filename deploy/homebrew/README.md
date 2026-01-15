# Homebrew 发布（规划 + 脚本入口）

本目录用于管理 NG Gateway 的 **Homebrew（macOS）发布** 相关资源，目标是实现：

- **用户侧安装简单**：`brew install <tap>/ng-gateway`
- **运行即用**：默认提供 `gateway.toml`、内置 drivers/plugins；SQLite DB 在首次启动时自动创建与迁移
- **单机分发友好**：支持 `ui-embedded`（将 `ui-dist.zip` 内嵌到二进制），避免用户装 Node/pnpm

---

## 目录结构

```
deploy/homebrew/
├── Formula/
│   └── ng-gateway.rb.tmpl             # Formula 模板（Tap 仓库使用）
├── resources/
│   └── gateway.toml                   # Homebrew 默认运行配置（ui.mode=embedded_zip）
└── scripts/
    ├── package-tarball.sh             # 构建并打包 macOS 发行 tar.gz（含二进制+资源）
    └── generate-formula.sh            # 根据 URL + SHA256 生成 Formula（从模板渲染）
```

---

## 发布路径（推荐：Binary Tarball + Tap Formula）

### 1) 构建并打包发行包（本机架构）

在仓库根目录执行：

```bash
VERSION=v1.2.3 ./deploy/homebrew/scripts/package-tarball.sh
```

产物：
- `deploy/homebrew/dist/ng-gateway-${VERSION}-darwin-${ARCH}.tar.gz`
- `deploy/homebrew/dist/ng-gateway-${VERSION}-darwin-${ARCH}.tar.gz.sha256`

> 说明：该脚本会执行 `cargo xtask build --profile release -- --features ng-gateway-bin/ui-embedded`，
> 生成 `ng-gateway-web/ui-dist.zip` 并把 UI 内嵌到网关二进制。

建议同时在两种 macOS 架构机器上各打一次包：
- Apple Silicon：`darwin-arm64`
- Intel：`darwin-amd64`

### 2) 上传 tarball 到 GitHub Release（或任意可公开下载的 URL）

你需要准备一个可被 Homebrew 访问的下载链接（例如 GitHub Releases 的 asset URL）。

### 3) 生成 Homebrew Formula（Tap 仓库使用）

```bash
VERSION=v1.2.3 \
URL_ARM64="https://github.com/<org>/<repo>/releases/download/v1.2.3/ng-gateway-v1.2.3-darwin-arm64.tar.gz" \
SHA256_ARM64="<sha256_arm64>" \
URL_AMD64="https://github.com/<org>/<repo>/releases/download/v1.2.3/ng-gateway-v1.2.3-darwin-amd64.tar.gz" \
SHA256_AMD64="<sha256_amd64>" \
./deploy/homebrew/scripts/generate-formula.sh
```

输出文件：
- `deploy/homebrew/Formula/ng-gateway.rb`

### 4) 发布到 Tap 仓库

本项目的 Tap 仓库为：
- `shiyuecamus/homebrew-ng-gateway`

将 `ng-gateway.rb` 放到 Tap 仓库的 `Formula/` 目录并推送：

```bash
# 推荐（隐式 tap）：Homebrew 会自动拉取 shiyuecamus/ng-gateway 这个 tap
brew install shiyuecamus/ng-gateway/ng-gateway

# 可选（显式 tap）：用于离线环境或调试
brew tap shiyuecamus/ng-gateway
brew install ng-gateway
```

---

## 运行时目录设计（重要）

因为网关运行时会将 drivers/plugins 的路径以“运行根目录相对路径”的方式写入 SQLite：

- `./drivers/builtin/libng_driver_*.dylib`
- `./plugins/builtin/libng_plugin_*.dylib`

因此 Homebrew Formula 会在安装时创建运行目录：

- `$(brew --prefix)/var/lib/ng-gateway/`

并把如下文件/目录放到这个运行目录（或建立符号链接）：

- `gateway.toml`
- `data/`（数据库文件会在首次启动时生成到 `./data/ng-gateway.db`）
- `drivers/`、`plugins/`
- `certs/`、`pki/`（运行时生成/写入）

最终通过包装脚本（wrapper）保证网关进程的 **工作目录** 正确，从而 DB 中的相对路径可用。

---

## 用户侧验证（macOS + Homebrew）

> 目标：验证 tap 安装、二进制可运行、并能通过 `brew services` 管理为后台服务。

### 1) 安装

```bash
# 推荐（隐式 tap）：Homebrew 会自动拉取 shiyuecamus/ng-gateway 这个 tap
brew install shiyuecamus/ng-gateway/ng-gateway

# 可选（显式 tap）：用于离线环境或调试
brew tap shiyuecamus/ng-gateway
brew install ng-gateway
```

> 提示：安装完成后，Homebrew 会输出一段「caveats」说明，包含配置文件、日志路径、运行时目录与健康检查等常用信息。

### 2) 基础运行验证

```bash
ng-gateway --help
ng-gateway --version || true
```

默认运行目录与配置文件位置：
- 运行目录：`$(brew --prefix)/var/lib/ng-gateway/`
- 配置文件：`$(brew --prefix)/var/lib/ng-gateway/gateway.toml`

默认 Web 端口（来自 Homebrew 默认 `gateway.toml`）：
- HTTP：`8978`
- HTTPS（auto TLS）：`8979`

健康检查：

```bash
curl -fsS "http://127.0.0.1:8978/health"
```

### 3) brew services 管理

如果你的环境里还没有 `brew services`：

```bash
brew tap homebrew/services
```

启动/查看/停止：

```bash
brew services start ng-gateway
brew services list
brew services stop ng-gateway
```

查看日志（Homebrew service 定义写入到 `$(brew --prefix)/var/log/`）：

```bash
tail -n 200 "$(brew --prefix)/var/log/ng-gateway.log"
tail -n 200 "$(brew --prefix)/var/log/ng-gateway.error.log"
```

### 4) 常见排障

- 端口被占用：编辑 `$(brew --prefix)/var/lib/ng-gateway/gateway.toml`（`[web].port` / `[web.ssl].port`），然后 `brew services restart ...`
- 配置改坏导致启动失败：先看 `ng-gateway.error.log`，再用 `brew services restart ...` 重启验证

---

## 卸载与数据清理（重要）

Homebrew 的惯例是：**卸载软件不自动删除 `var/` 下的用户数据**（避免误删）。  
因此 `brew uninstall ng-gateway` 只会卸载程序本体，不会删除运行目录与数据。

如果你确认要“彻底卸载”（包含运行目录与数据），建议按以下顺序执行：

```bash
# 1) 停止后台服务
brew services stop ng-gateway || true

# 2) 卸载程序本体
brew uninstall ng-gateway

# 3) 删除运行目录（会删除 SQLite DB、证书、drivers/plugins 等）
rm -rf "$(brew --prefix)/var/lib/ng-gateway"

# 4) 可选：删除日志
rm -f "$(brew --prefix)/var/log/ng-gateway.log" \
      "$(brew --prefix)/var/log/ng-gateway.error.log"
```


