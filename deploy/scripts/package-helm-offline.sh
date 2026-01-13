#!/usr/bin/env bash
set -euo pipefail

# Helm Chart 离线部署打包脚本（All-in-one 单镜像）
# 功能：构建网关单镜像（内嵌 UI 静态资源），导出为 tar，打包 Helm Chart 和镜像到 zip

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEPLOY_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HELM_DIR="${DEPLOY_DIR}/helm"

# 配置参数
REGISTRY="${REGISTRY:-}"
IMAGE_PREFIX="${IMAGE_PREFIX:-ng}"
VERSION="${VERSION:-$(date +%Y%m%d-%H%M%S)}"
PLATFORM="${PLATFORM:-linux/arm64}"
# Harbor 配置（可选，用于推送镜像）
HARBOR_URL="${HARBOR_URL:-}"
HARBOR_USER="${HARBOR_USER:-}"
HARBOR_PASSWORD="${HARBOR_PASSWORD:-}"

# 计算仓库前缀（如设置REGISTRY则使用REGISTRY/前缀）
REGISTRY_PREFIX="${REGISTRY:+$REGISTRY/}"

# 镜像名称（单镜像）
gateway_repository="${IMAGE_PREFIX}-gateway"
gateway_image="${REGISTRY:+$REGISTRY/}${gateway_repository}:${VERSION}"

# 临时打包目录
PACKAGE_DIR="${REPO_ROOT}/helm-package-${VERSION}"
PACKAGE_NAME="ng-gateway-helm-offline-${VERSION}.zip"
CHART_NAME="ng-gateway"

echo "=========================================="
echo "NG Gateway Helm Chart 离线部署打包工具"
echo "=========================================="
echo "版本: ${VERSION}"
echo "平台: ${PLATFORM}"
echo "Gateway 镜像: ${gateway_image}"
echo "=========================================="
echo ""

# 检查 Helm 是否安装
if ! command -v helm &> /dev/null; then
  echo "错误: 未找到 Helm，请先安装 Helm 3.0+"
  exit 1
fi

# 检查 yq 是否安装
if ! command -v yq &> /dev/null; then
  echo "错误: 未找到 yq，请先安装 yq (https://github.com/mikefarah/yq)"
  exit 1
fi

# 检查 Docker buildx 是否支持多平台
echo "[检查] Docker buildx 支持..."
if ! docker buildx version &>/dev/null; then
  echo "错误: 需要 Docker buildx 支持多平台构建"
  echo "请运行: docker buildx create --use"
  exit 1
fi

# 使用本地 docker 驱动的 builder
docker buildx use default 2>/dev/null || true
if ! docker buildx ls | grep -q '^default\*\s\+docker'; then
  docker buildx create --name local-docker --driver docker --use 2>/dev/null || true
fi

# 清理旧的打包目录
if [ -d "$PACKAGE_DIR" ]; then
  echo "[清理] 删除旧的打包目录..."
  rm -rf "$PACKAGE_DIR"
fi

# 创建打包目录结构
echo "[准备] 创建打包目录..."
mkdir -p "$PACKAGE_DIR/images"
mkdir -p "$PACKAGE_DIR/charts"

# 构建并导出 Gateway 镜像
echo ""
echo "[构建] Gateway 镜像 (${PLATFORM})..."
gateway_tar="${PACKAGE_DIR}/images/gateway-${VERSION}.tar"

# 如果设置了代理，则透传到构建过程中；未设置时不传递对应 build-arg
gateway_proxy_args=()
if [ -n "${HTTP_PROXY:-}" ]; then
  gateway_proxy_args+=(--build-arg "HTTP_PROXY=${HTTP_PROXY}")
fi
if [ -n "${HTTPS_PROXY:-}" ]; then
  gateway_proxy_args+=(--build-arg "HTTPS_PROXY=${HTTPS_PROXY}")
fi
if [ -n "${NO_PROXY:-}" ]; then
  gateway_proxy_args+=(--build-arg "NO_PROXY=${NO_PROXY}")
fi

docker buildx build \
  --platform "${PLATFORM}" \
  ${gateway_proxy_args[@]+"${gateway_proxy_args[@]}"} \
  --tag "${gateway_image}" \
  --load \
  -f "$REPO_ROOT/deploy/docker/gateway.Dockerfile" \
  "$REPO_ROOT" || {
  echo "错误: Gateway 镜像构建失败"
  exit 1
}
docker save "${gateway_image}" -o "${gateway_tar}"
echo "已构建并导出: ${gateway_tar} ($(du -h "${gateway_tar}" | cut -f1))"

# 步骤 1: 复制 Chart 并修改版本号
echo ""
echo "[步骤 1] 复制 Chart 并更新版本号..."
CHART_TEMP_DIR="${PACKAGE_DIR}/${CHART_NAME}"
cp -r "${HELM_DIR}/${CHART_NAME}" "${CHART_TEMP_DIR}"

# 更新 Chart.yaml 中的版本号
echo "[更新] Chart.yaml 版本号..."
VERSION="$VERSION" yq -i '.version = strenv(VERSION) | .appVersion = strenv(VERSION)' "${CHART_TEMP_DIR}/Chart.yaml"

# 更新 values.yaml 中的镜像配置
echo "[更新] values.yaml 中的镜像配置..."
# 确保变量有值（即使是空字符串）
REGISTRY_VALUE="${REGISTRY:-}"
GATEWAY_REPO_VALUE="${gateway_repository}"

REGISTRY_VALUE="$REGISTRY_VALUE" yq -i '
  .gateway.image.registry = strenv(REGISTRY_VALUE)
' "${CHART_TEMP_DIR}/values.yaml"

GATEWAY_REPO_VALUE="$GATEWAY_REPO_VALUE" yq -i '.gateway.image.repository = strenv(GATEWAY_REPO_VALUE)' "${CHART_TEMP_DIR}/values.yaml"

VERSION="$VERSION" yq -i '.gateway.image.tag = strenv(VERSION)' "${CHART_TEMP_DIR}/values.yaml"

# 打包 Helm Chart
echo "[打包] Helm Chart..."
cd "${PACKAGE_DIR}"
helm package "${CHART_NAME}" --destination "${PACKAGE_DIR}/charts" || {
  echo "错误: Helm Chart 打包失败"
  exit 1
}
CHART_FILE=$(ls -t "${PACKAGE_DIR}/charts"/*.tgz | head -1)
echo "已打包 Chart: $(basename "$CHART_FILE")"

# 清理临时目录
rm -rf "${CHART_TEMP_DIR}"

# 步骤 3: 创建部署脚本
echo ""
echo "[步骤 3] 创建部署脚本..."
cat > "$PACKAGE_DIR/deploy.sh" <<'DEPLOY_SCRIPT'
#!/bin/bash
set -euo pipefail

# NG Gateway 离线镜像部署脚本
# 功能：加载离线镜像，推送到 Harbor 私服

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGES_DIR="${SCRIPT_DIR}/images"

# Harbor 配置（可选）
HARBOR_URL="${HARBOR_URL:-}"
HARBOR_USER="${HARBOR_USER:-}"
HARBOR_PASSWORD="${HARBOR_PASSWORD:-}"

echo "=========================================="
echo "NG Gateway 离线镜像部署"
echo "=========================================="
if [ -n "${HARBOR_URL}" ]; then
  echo "Harbor URL: ${HARBOR_URL}"
fi
echo "=========================================="
echo ""

# 检查 Docker 是否安装
if ! command -v docker &> /dev/null; then
  echo "错误: 未找到 docker，请先安装 docker"
  exit 1
fi

# 步骤 1: 加载离线镜像
echo "[步骤 1] 加载离线镜像..."
LOADED_IMAGES=()
for image_tar in "${IMAGES_DIR}"/*.tar; do
  if [ -f "$image_tar" ]; then
    echo "加载镜像: $(basename "$image_tar")"
    LOAD_OUTPUT=$(docker load -i "$image_tar" 2>&1)
    echo "$LOAD_OUTPUT"
    # 获取镜像名称（从输出中提取）
    IMAGE_NAME=$(echo "$LOAD_OUTPUT" | grep "Loaded image" | sed 's/Loaded image: //' | head -1)
    if [ -n "$IMAGE_NAME" ]; then
      echo "  镜像: $IMAGE_NAME"
      LOADED_IMAGES+=("$IMAGE_NAME")
    fi
  fi
done

if [ ${#LOADED_IMAGES[@]} -eq 0 ]; then
  echo "警告: 未找到任何镜像文件"
  exit 1
fi

echo "镜像加载完成，共加载 ${#LOADED_IMAGES[@]} 个镜像"

# 步骤 2: 推送到 Harbor（如果配置了）
if [ -n "${HARBOR_URL}" ] && [ -n "${HARBOR_USER}" ] && [ -n "${HARBOR_PASSWORD}" ]; then
  echo ""
  echo "[步骤 2] 推送镜像到 Harbor..."
  echo "登录 Harbor: ${HARBOR_URL}"
  echo "${HARBOR_PASSWORD}" | docker login "${HARBOR_URL}" -u "${HARBOR_USER}" --password-stdin || {
    echo "错误: Harbor 登录失败"
    exit 1
  }
  
  # 推送所有已加载的镜像
  for IMAGE_NAME in "${LOADED_IMAGES[@]}"; do
    if [ -n "$IMAGE_NAME" ]; then
      # 构建新的镜像标签（Harbor 格式）
      IMAGE_TAG=$(echo "$IMAGE_NAME" | cut -d: -f2)
      IMAGE_REPO=$(echo "$IMAGE_NAME" | cut -d: -f1)
      HARBOR_IMAGE="${HARBOR_URL}/${IMAGE_REPO}:${IMAGE_TAG}"
      
      echo "推送镜像: $IMAGE_NAME -> $HARBOR_IMAGE"
      docker tag "$IMAGE_NAME" "$HARBOR_IMAGE"
      docker push "$HARBOR_IMAGE" || {
        echo "错误: 镜像推送失败: $HARBOR_IMAGE"
        exit 1
      }
    fi
  done
  
  echo ""
  echo "=========================================="
  echo "镜像推送完成！"
  echo "=========================================="
  echo ""
  echo "已推送的镜像:"
  for IMAGE_NAME in "${LOADED_IMAGES[@]}"; do
    if [ -n "$IMAGE_NAME" ]; then
      IMAGE_TAG=$(echo "$IMAGE_NAME" | cut -d: -f2)
      IMAGE_REPO=$(echo "$IMAGE_NAME" | cut -d: -f1)
      HARBOR_IMAGE="${HARBOR_URL}/${IMAGE_REPO}:${IMAGE_TAG}"
      echo "  - $HARBOR_IMAGE"
    fi
  done
else
  echo ""
  echo "[步骤 2] 跳过 Harbor 推送（未配置 HARBOR_URL/HARBOR_USER/HARBOR_PASSWORD）"
  echo ""
  echo "如需推送到 Harbor，请设置以下环境变量:"
  echo "  export HARBOR_URL=harbor.example.com"
  echo "  export HARBOR_USER=admin"
  echo "  export HARBOR_PASSWORD=your-password"
  echo ""
  echo "然后重新运行: ./deploy.sh"
fi

echo ""
DEPLOY_SCRIPT

chmod +x "$PACKAGE_DIR/deploy.sh"

# 步骤 4: 创建 README
echo ""
echo "[步骤 4] 创建部署说明文档..."
cat > "$PACKAGE_DIR/README.md" <<EOF
# NG Gateway Helm Chart 离线部署包

## 版本信息
- 版本: ${VERSION}
- 平台: ${PLATFORM}
- 构建时间: $(date '+%Y-%m-%d %H:%M:%S')

## 文件说明
- \`deploy.sh\`: 部署脚本，在目标 Kubernetes 集群上运行
- \`charts/\`: Helm Chart 文件（.tgz 格式）
- \`images/\`: Docker 镜像文件（.tar 格式）

## 前置要求

1. **Docker** 已安装并运行
2. **Harbor 访问权限**（如果使用 Harbor 私服）

## 部署步骤

### 1. 传输文件到目标服务器
将整个部署包传输到目标服务器

### 2. 解压部署包
\`\`\`bash
unzip ng-gateway-helm-offline-${VERSION}.zip
cd helm-package-${VERSION}
\`\`\`

### 3. 配置 Harbor 环境变量（如果使用）
\`\`\`bash
export HARBOR_URL=harbor.example.com
export HARBOR_USER=admin
export HARBOR_PASSWORD=your-password
\`\`\`

### 4. 运行部署脚本
\`\`\`bash
chmod +x deploy.sh
./deploy.sh
\`\`\`

部署脚本会自动执行以下步骤：
1. 加载离线镜像到本地 Docker
2. 推送镜像到 Harbor（如果配置了 HARBOR_URL/HARBOR_USER/HARBOR_PASSWORD）

### 5. 使用 Helm 部署（在 Kubernetes 集群上）
镜像加载/推送完成后，在 Kubernetes 集群上使用 Helm 部署：

\`\`\`bash
# 安装
helm install ng-gateway charts/*.tgz \\
  -n default \\
  --create-namespace

# 或升级（如果已存在）
helm upgrade ng-gateway charts/*.tgz \\
  -n default
\`\`\`

## 手动部署

如果自动部署脚本不适用，可以手动执行：

### 1. 加载镜像
\`\`\`bash
for tar in images/*.tar; do
  docker load -i "\$tar"
done
\`\`\`

### 2. 推送镜像到 Harbor（可选）
\`\`\`bash
docker login harbor.example.com -u admin -p password

# 为镜像打标签
docker tag ng-gateway:${VERSION} harbor.example.com/ng-gateway:${VERSION}

# 推送镜像
docker push harbor.example.com/ng-gateway:${VERSION}
\`\`\`

### 3. 安装 Helm Chart
\`\`\`bash
# 安装
helm install ng-gateway charts/*.tgz \\
  -n default \\
  --create-namespace

# 或升级（如果已存在）
helm upgrade ng-gateway charts/*.tgz \\
  -n default
\`\`\`

## 服务管理

### 查看 Release 状态
\`\`\`bash
helm status ng-gateway -n default
\`\`\`

### 查看 Pod 状态
\`\`\`bash
kubectl get pods -n default -l app.kubernetes.io/instance=ng-gateway
\`\`\`

### 查看日志
\`\`\`bash
# Gateway 日志
kubectl logs -n default -l app.kubernetes.io/component=gateway -f
\`\`\`

### 查看服务
\`\`\`bash
kubectl get svc -n default -l app.kubernetes.io/instance=ng-gateway
\`\`\`

### 端口转发（用于测试）
\`\`\`bash
# Gateway
kubectl port-forward -n default svc/ng-gateway-gateway 8978:8978
\`\`\`

### 升级 Release
\`\`\`bash
helm upgrade ng-gateway charts/*.tgz \\
  -n default
\`\`\`

### 回滚 Release
\`\`\`bash
# 查看历史版本
helm history ng-gateway -n default

# 回滚到上一个版本
helm rollback ng-gateway -n default

# 回滚到指定版本
helm rollback ng-gateway 2 -n default
\`\`\`

### 卸载 Release
\`\`\`bash
helm uninstall ng-gateway -n default
\`\`\`

**注意:** 卸载默认会保留 PVC（持久化存储）。如需删除 PVC，请手动删除。

## 配置说明

### Gateway 配置

Gateway 的配置通过 Helm Chart 的 \`values.yaml\` 进行配置。主要配置项在 \`gateway.config\` 下：

- \`gateway.config.web.port\`: Gateway HTTP 端口（自动用于 Service）
- \`gateway.config.web.ssl.port\`: Gateway WebSocket/SSL 端口（自动用于 Service）
- 其他配置项请参考 Chart 的 values.yaml

### 持久化存储

默认启用三个 PVC：
- \`gateway-data\`: 网关数据（SQLite 数据库等）
- \`gateway-drivers\`: 自定义驱动目录
- \`gateway-plugins\`: 自定义插件目录

### Ingress

如果启用 Ingress（all-in-one），Ingress 将 \`/\` 路由到 Gateway Service，
由网关进程同时提供 UI（`/`）和 API（`/api`）。

## 故障排查

1. **Pod 无法启动**
   - 检查镜像是否正确加载: \`docker images | grep ng\`
   - 查看 Pod 事件: \`kubectl describe pod <pod-name> -n default\`
   - 查看 Pod 日志: \`kubectl logs <pod-name> -n default\`

2. **无法访问服务**
   - 检查 Service 是否创建: \`kubectl get svc -n default\`
   - 检查 Pod 是否就绪: \`kubectl get pods -n default\`
   - 使用端口转发测试: \`kubectl port-forward svc/<service-name> <local-port>:<service-port>\`

3. **存储问题**
   - 检查 PVC 状态: \`kubectl get pvc -n default\`
   - 检查 StorageClass: \`kubectl get storageclass\`
   - 查看 PVC 详情: \`kubectl describe pvc <pvc-name> -n default\`

4. **配置问题**
   - 检查 ConfigMap: \`kubectl get configmap -n default\`
   - 查看 ConfigMap 内容: \`kubectl get configmap <configmap-name> -n default -o yaml\`

## 更多信息

- [Helm Chart README](../helm/ng-gateway/README.md)
- [项目主 README](../../README.md)
EOF

# 步骤 5: 打包为 zip
echo ""
echo "[步骤 5] 打包 ZIP 压缩包..."
cd "$REPO_ROOT"
zip -r "$PACKAGE_NAME" "$(basename "$PACKAGE_DIR")" > /dev/null

# 显示打包结果
echo ""
echo "=========================================="
echo "打包完成！"
echo "=========================================="
echo "打包文件: ${PACKAGE_NAME}"
echo "文件大小: $(du -h "$PACKAGE_NAME" | cut -f1)"
echo ""
echo "打包内容:"
echo "  - deploy.sh (部署脚本)"
echo "  - charts/ (Helm Chart 文件)"
echo "  - images/ (Docker 镜像文件)"
echo "  - README.md (部署说明)"
echo ""

# 清理临时打包目录
echo "[清理] 删除临时打包目录..."
rm -rf "$PACKAGE_DIR"
echo "已清理临时目录: ${PACKAGE_DIR}"

echo ""
echo "下一步:"
echo "  1. 将 ${PACKAGE_NAME} 传输到目标服务器"
echo "  2. 解压: unzip ${PACKAGE_NAME}"
echo "  3. 运行: cd $(basename "${PACKAGE_NAME%.zip}") && ./deploy.sh"
echo ""

