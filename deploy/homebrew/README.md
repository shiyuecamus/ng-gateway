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

推荐新建一个 Tap 仓库，例如：
- `ng/homebrew-tap`

将 `ng-gateway.rb` 放到 Tap 仓库的 `Formula/` 目录并推送：

```bash
brew tap ng/tap
brew install ng/tap/ng-gateway
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


