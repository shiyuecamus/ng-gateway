# NG Gateway 部署与发布（All-in-one 单镜像/单进程）

本目录（`deploy/`）统一管理 **部署、离线交付** 相关内容。项目主线采用 **All-in-one** 架构：
- **单个网关镜像/二进制** 同时提供 **Web UI（`/`）+ API（`/api`）**

---

## 构建代理（Proxy）

如你的网络环境需要代理（公司内网 / CI / 海外依赖拉取慢），以下脚本均支持透传：
- `deploy/docker/scripts/build-single.sh`
- `deploy/docker/scripts/package-offline.sh`
- `deploy/helm/scripts/package-helm-offline.sh`

使用方式：设置环境变量即可（未设置则不会传递 build-arg）：

```bash
export HTTP_PROXY=http://x.x.x.x:7890
export HTTPS_PROXY=http://x.x.x.x:7890
export NO_PROXY=localhost,127.0.0.1
```

---

## 目录结构

```
deploy/
├── docker/                        # 网关单镜像 Dockerfile（包含 UI dist）
│   ├── gateway.Dockerfile
│   └── scripts/
│       ├── build-single.sh            # 在线构建 all-in-one 网关镜像
│       ├── build-push-docker.sh       # buildx 多架构构建/推送
│       └── package-offline.sh         # Docker 离线包（zip，目标机 docker run）
├── helm/                          # Helm Chart（all-in-one：仅 gateway）
│   ├── ng-gateway/
│   └── scripts/
│       ├── package-helm-offline.sh    # Helm 离线包（zip）
│       └── package-push-helm.sh       # Helm Chart 打包并 push（OCI）
├── compose/                       # docker-compose 示例（本地/离线/可观测性）
```

---

## Observability（本地 Prometheus + Grafana，一键可观测）

本仓库提供一个开箱即用的可观测性栈（Prometheus + Grafana），内置 dashboards 与告警规则：

- Compose 文件：`deploy/compose/docker-compose.observability.yaml`
- 配置目录：`deploy/compose/observability/`
  - Prometheus 配置与 rules：`observability/prometheus/`
  - Grafana provisioning 与 dashboards：`observability/grafana/`

### 启动

```bash
# 默认：拉起 Prometheus + Grafana + Gateway（同栈一键闭环）
docker compose -f deploy/compose/docker-compose.observability.yaml up -d
```

如果你只想部署 Prometheus + Grafana，并抓取宿主机上运行的网关（host 模式）：

```bash
docker compose -f deploy/compose/docker-compose.observability.host.yaml up -d
```

### 访问

- Prometheus：`http://localhost:9090`
- Grafana：`http://localhost:3000`（默认账号：`admin/admin`）

### 接入 ng-gateway 指标

Prometheus 预置了两条抓取路径：

- **Host 模式**：`host.docker.internal:8978/metrics`（macOS/Windows 默认可用；Linux compose 已加 `host-gateway` 映射）
- **Docker 模式**：`gateway:5678/metrics`（使用 `docker-compose.observability.yaml` 同栈启动网关时生效）

> 注意：不要同时启用 host + docker 两个网关实例，否则 Prometheus 可能会抓到两份相同指标，导致 dashboard/告警双倍统计。

---

## All-in-one（Docker 在线部署）

### 构建镜像

```bash
./deploy/docker/scripts/build-single.sh
```

支持的环境变量（不传则使用默认值）：
- `VERSION`：镜像 tag（默认 `latest`）
- `IMAGE_PREFIX`：镜像名前缀（默认 `ng`，最终镜像名为 `${IMAGE_PREFIX}-gateway`）
- `REGISTRY`：可选镜像仓库前缀（如 `ghcr.io/xxx`）
- `PLATFORM`：可选平台（如 `linux/amd64`），会透传到 `docker build --platform`
- `TAG_LATEST`：是否额外打 `:latest`（`true/false`，默认 `false`）

示例：

```bash
VERSION=v1.2.3 IMAGE_PREFIX=ng ./deploy/docker/scripts/build-single.sh
```

### 验证镜像是否可用（推荐流程）

1) 启动容器（最小可运行配置）：

```bash
docker run -d \
  --name ng-gateway \
  --privileged=true \
  --restart unless-stopped \
  -p 8978:5678 \
  -p 8979:5679 \
  -v gateway-data:/app/data \
  -v gateway-drivers:/app/drivers/custom \
  -v gateway-plugins:/app/plugins/custom \
  -v gateway-ai:/app/ai \
  ng-gateway:latest
```

2) 检查容器状态与日志：

```bash
docker ps --filter "name=ng-gateway"
docker logs -f ng-gateway
```

3) 健康检查（HTTP）：

- `GET /health`

```bash
curl -fsS "http://127.0.0.1:8978/health" && echo
```

4) UI 验证（如果开启 UI）：

- 访问：`http://127.0.0.1:8978/`

5) 停止/清理（可选）：

```bash
docker rm -f ng-gateway
```

### 运行

```bash
# 说明：这是一个“单容器”网关，直接 docker run 即可
# 如需自定义环境变量，建议准备一个 .env（可参考 deploy/env/.env 或项目根目录 .env）

docker run -d \
  --name ng-gateway \
  --privileged=true \
  --restart unless-stopped \
  --env-file ./.env \
  -p 8978:5678 \
  -p 8979:5679 \
  -v gateway-data:/app/data \
  -v gateway-drivers:/app/drivers/custom \
  -v gateway-plugins:/app/plugins/custom \
  -v gateway-ai:/app/ai \
  ng-gateway:latest
```

### 访问

- **Web UI**：`http://<host>:8978/`
- **API**：`http://<host>:8978/api`

> 网关镜像构建时会把 `web-antd` 的 `dist` 复制到容器 `/app/ui`，并默认启用：
> - `NG_WEB__UI__ENABLED=true`
> - `NG_WEB__UI__MODE=filesystem`
> - `NG_WEB__UI__FILESYSTEM_ROOT=/app/ui`

---

## Offline（Docker 离线交付，zip 包）

### 生成离线包（构建 + 导出镜像 tar + 打包 zip）

```bash
./deploy/docker/scripts/package-offline.sh
```

常用参数：
- `VERSION`：版本号（默认时间戳）
- `PLATFORM`：平台（默认 `linux/arm64`）
- `IMAGE_PREFIX`：镜像仓库前缀（默认 `ng`）
- `REGISTRY`：可选镜像仓库前缀

示例：

```bash
VERSION=v1.2.3 PLATFORM=linux/amd64 ./deploy/docker/scripts/package-offline.sh
```

### 目标机部署

```bash
unzip ng-gateway-offline-*.zip
cd ng-gateway-offline-*
./deploy.sh
```

离线包会自动：
- `docker load` 加载镜像
- `docker run` 启动单容器网关（`deploy.sh` 内已包含 volume/端口/重启策略）

---

## Offline（Helm 离线交付，zip 包）

### 生成离线包（构建 + 导出镜像 tar + 打包 Chart tgz + zip）

```bash
./deploy/helm/scripts/package-helm-offline.sh
```

常用参数：
- `VERSION`：版本号（默认时间戳）
- `PLATFORM`：平台（默认 `linux/arm64`）
- `IMAGE_PREFIX`：镜像仓库前缀（默认 `ng`）
- `REGISTRY`：可选镜像仓库前缀

### 目标机部署（推送镜像到 Harbor 可选）

```bash
unzip ng-gateway-helm-offline-*.zip
cd helm-package-*
./deploy.sh
```

之后在 Kubernetes 集群执行：

```bash
helm install ng-gateway charts/*.tgz -n default --create-namespace
```

> Helm Chart 采用 all-in-one：Ingress（如果启用）会把 `/` 路由到 gateway Service，由网关进程同时提供 UI 与 API。

---

## Observability（Helm / Prometheus Operator / Grafana）

Helm Chart 中提供了 4 类可观测性资源（可选，通过 values 开关启用）。推荐直接使用 **一键开关**：

- `observability.enabled=true`

它会默认开启：
- ServiceMonitor（推荐）
- PrometheusRule
- Grafana dashboards ConfigMaps

并默认关闭 PodMonitor（避免与 ServiceMonitor 重复抓取）。

你也可以逐项启用/关闭（手动模式）：

- `templates/servicemonitor.yaml`：Prometheus Operator 的 **ServiceMonitor**
- `templates/podmonitor.yaml`：Prometheus Operator 的 **PodMonitor**
- `templates/prometheusrule.yaml`：Prometheus Operator 的 **PrometheusRule**
- `templates/grafana-dashboards.yaml`：Grafana sidecar 自动导入的 **Dashboard ConfigMaps**

要“开箱即用”，你通常还需要：

1) 集群安装 Prometheus Operator（建议直接用 Helm 安装 `kube-prometheus-stack`）
2) 确认 Prometheus 的 selector label 规则：kube-prometheus-stack 常见要求 `release=<kps-release>`，本 Chart 默认用 `release=kube-prometheus-stack`；如不同请改：
   - `observability.prometheusOperator.selectorLabelValue`
3) Grafana dashboards 的“自动导入”依赖 Grafana sidecar 的 namespace watch 策略：
   - 要么让 sidecar watch `ALL`（或包含网关 namespace）
   - 要么把 dashboards ConfigMap 直接创建到 Grafana namespace：
     - `observability.grafanaDashboards.namespace=<grafana-namespace>`

详细参数见：

- `deploy/helm/ng-gateway/values.yaml` 中的 `observability`

---

## 本地开发（建议）

### 网关 + UI（统一用 cargo xtask）

```bash
# 1) 构建网关（含 drivers/plugins 部署）并构建 UI（会生成 dist + ui-dist.zip）
#    说明：需要本机具备 Node + pnpm（用于 UI build）
cargo xtask build

# 如当前环境没有 Node/pnpm，或只想构建后端，可显式跳过 UI：
# cargo xtask build --without-ui

# 2) 运行
./target/debug/ng-gateway-bin
```

### UI dev server（更快的前端开发体验）

推荐让 UI dev server 走自己的端口（如 5666/），API 指向网关 `http://localhost:8978/api`

---

## Homebrew（macOS 单机分发）

Homebrew 发行相关资源统一放在 `deploy/homebrew/`：
- 目标：提供 **单二进制（可选内嵌 UI）+ 内置 drivers/plugins + 初始 SQLite DB** 的 tarball，
  并生成 Homebrew Formula（Tap 模式）用于安装与升级。

> 详细流程与脚本入口请见：`deploy/homebrew/README.md`
