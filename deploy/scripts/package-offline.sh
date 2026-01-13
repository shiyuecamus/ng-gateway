#!/usr/bin/env bash
set -euo pipefail

# 离线部署打包脚本（All-in-one 单镜像）
# 功能：构建网关单镜像（内嵌 UI 静态资源），导出为 tar，打包部署文件到 zip

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEPLOY_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# 配置参数
REGISTRY="${REGISTRY:-}"
IMAGE_PREFIX="${IMAGE_PREFIX:-ng}"
VERSION="${VERSION:-$(date +%Y%m%d-%H%M%S)}"
PLATFORM="${PLATFORM:-linux/arm64}"

# 计算仓库前缀（如设置REGISTRY则使用REGISTRY/前缀）
REGISTRY_PREFIX="${REGISTRY:+$REGISTRY/}"

# 镜像名称（单镜像）
gateway_image="${REGISTRY:+$REGISTRY/}${IMAGE_PREFIX}-gateway:${VERSION}"

# 临时打包目录
PACKAGE_DIR="${REPO_ROOT}/deploy-package-${VERSION}"
PACKAGE_NAME="ng-gateway-offline-${VERSION}.zip"

echo "=========================================="
echo "NG Gateway 离线部署打包工具"
echo "=========================================="
echo "版本: ${VERSION}"
echo "平台: ${PLATFORM}"
echo "Gateway 镜像: ${gateway_image}"
echo "=========================================="
echo ""

# 清理旧的打包目录
if [ -d "$PACKAGE_DIR" ]; then
  echo "[清理] 删除旧的打包目录..."
  rm -rf "$PACKAGE_DIR"
fi

# 创建打包目录结构
echo "[准备] 创建打包目录..."
mkdir -p "$PACKAGE_DIR/images"
mkdir -p "$PACKAGE_DIR/env"

# 检查 Docker buildx 是否支持多平台
echo "[检查] Docker buildx 支持..."
if ! docker buildx version &>/dev/null; then
  echo "错误: 需要 Docker buildx 支持多平台构建"
  echo "请运行: docker buildx create --use"
  exit 1
fi

# 使用本地 docker 驱动的 builder（方案A）
docker buildx use default 2>/dev/null || true
if ! docker buildx ls | grep -q '^default\*\s\+docker'; then
  docker buildx create --name local-docker --driver docker --use 2>/dev/null || true
fi

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

# 处理 .env 文件
echo ""
echo "[处理] 环境变量配置..."
# 离线包通过 `--env-file ${SCRIPT_DIR}/.env` 传入配置（不依赖 compose 内的 env_file）
if [ -f "$REPO_ROOT/.env" ]; then
  echo "使用项目根目录的 .env 文件"
  cp "$REPO_ROOT/.env" "$PACKAGE_DIR/env/.env"
  # 同时复制一份到根目录，供部署脚本使用
  cp "$REPO_ROOT/.env" "$PACKAGE_DIR/.env"
  # 覆盖镜像相关变量以匹配当前打包的镜像
  for f in "$PACKAGE_DIR/env/.env" "$PACKAGE_DIR/.env"; do
    tmp_file="${f}.tmp"
    grep -vE '^(GATEWAY_IMAGE|GATEWAY_TAG)=' "$f" > "$tmp_file" || true
    {
      echo "GATEWAY_IMAGE=${REGISTRY_PREFIX}${IMAGE_PREFIX}-gateway"
      echo "GATEWAY_TAG=${VERSION}"
    } >> "$tmp_file"
    mv "$tmp_file" "$f"
  done
elif [ -f "$DEPLOY_DIR/env/.env" ]; then
  echo "使用 deploy/env/.env 文件"
  cp "$DEPLOY_DIR/env/.env" "$PACKAGE_DIR/env/.env"
  # 同时复制一份到根目录，供部署脚本使用
  cp "$DEPLOY_DIR/env/.env" "$PACKAGE_DIR/.env"
  # 覆盖镜像相关变量以匹配当前打包的镜像
  for f in "$PACKAGE_DIR/env/.env" "$PACKAGE_DIR/.env"; do
    tmp_file="${f}.tmp"
    grep -vE '^(GATEWAY_IMAGE|GATEWAY_TAG)=' "$f" > "$tmp_file" || true
    {
      echo "GATEWAY_IMAGE=${REGISTRY_PREFIX}${IMAGE_PREFIX}-gateway"
      echo "GATEWAY_TAG=${VERSION}"
    } >> "$tmp_file"
    mv "$tmp_file" "$f"
  done
else
  echo "创建示例 .env 文件..."
  cat > "$PACKAGE_DIR/env/.env" <<EOF
# Gateway 镜像配置
GATEWAY_IMAGE=${REGISTRY_PREFIX}${IMAGE_PREFIX}-gateway
GATEWAY_TAG=${VERSION}

# 端口配置
GATEWAY_HTTP_PORT=8978

# 运行时环境变量（可选）
RUST_LOG=info
TZ=Asia/Shanghai
EOF
  # 同时复制一份到根目录，供部署脚本使用
  cp "$PACKAGE_DIR/env/.env" "$PACKAGE_DIR/.env"
fi

# 创建部署脚本
echo ""
echo "[创建] 部署脚本..."
cat > "$PACKAGE_DIR/deploy.sh" <<'DEPLOY_SCRIPT'
#!/bin/bash
set -euo pipefail

# NG Gateway 离线部署脚本
# 在目标服务器上运行此脚本来部署网关

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGES_DIR="${SCRIPT_DIR}/images"
CONTAINER_NAME="${CONTAINER_NAME:-ng-gateway}"

echo "=========================================="
echo "NG Gateway 离线部署"
echo "=========================================="
echo ""

# 检查 Docker 是否安装
if ! command -v docker &> /dev/null; then
  echo "错误: 未找到 Docker，请先安装 Docker"
  exit 1
fi

echo "[检查] Docker 环境..."
docker info > /dev/null || {
  echo "错误: Docker 服务未运行，请启动 Docker 服务"
  exit 1
}

# 加载镜像
echo ""
echo "[加载] Docker 镜像..."
for image_tar in "${IMAGES_DIR}"/*.tar; do
  if [ -f "$image_tar" ]; then
    echo "加载镜像: $(basename "$image_tar")"
    docker load -i "$image_tar"
  fi
done

# 检查 .env 文件
if [ ! -f "${SCRIPT_DIR}/.env" ]; then
  echo ""
  echo "警告: 未找到 .env 文件，将使用默认配置"
  echo "如需自定义配置，请在部署目录创建 .env 文件"
fi

# 停止旧容器（如果存在）
echo ""
echo "[停止] 旧容器（如果存在）..."
cd "$SCRIPT_DIR"
docker rm -f "$CONTAINER_NAME" 2>/dev/null || true

# 创建持久化 volumes（幂等）
echo ""
echo "[准备] 创建持久化 volumes..."
docker volume create gateway-data >/dev/null
docker volume create gateway-drivers >/dev/null
docker volume create gateway-plugins >/dev/null

# 读取镜像名（优先 .env，其次 fallback）
GATEWAY_IMAGE="${GATEWAY_IMAGE:-ng-gateway}"
GATEWAY_TAG="${GATEWAY_TAG:-local}"
GATEWAY_HTTP_PORT="${GATEWAY_HTTP_PORT:-8978}"
GATEWAY_WS_PORT="${GATEWAY_WS_PORT:-8979}"

if [ -f "${SCRIPT_DIR}/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  source "${SCRIPT_DIR}/.env"
  set +a
fi

# 启动服务
echo ""
echo "[启动] 服务..."
env_file_args=()
if [ -f "${SCRIPT_DIR}/.env" ]; then
  env_file_args+=(--env-file "${SCRIPT_DIR}/.env")
fi

docker run -d \
  --name "$CONTAINER_NAME" \
  --restart unless-stopped \
  --privileged=true \
  "${env_file_args[@]}" \
  -p "${GATEWAY_HTTP_PORT}:8978" \
  -p "${GATEWAY_WS_PORT}:8979" \
  -v gateway-data:/app/data \
  -v gateway-drivers:/app/drivers/custom \
  -v gateway-plugins:/app/plugins/custom \
  "${GATEWAY_IMAGE}:${GATEWAY_TAG}"

# 等待服务启动
echo ""
echo "[等待] 服务启动..."
sleep 5

# 显示服务状态
echo ""
echo "[状态] 服务状态:"
docker ps --filter "name=${CONTAINER_NAME}"

echo ""
echo "=========================================="
echo "部署完成！"
echo "=========================================="
echo ""
echo "服务访问地址:"
echo "  - Web UI: http://localhost:\${GATEWAY_HTTP_PORT:-8978}/"
echo "  - API: http://localhost:\${GATEWAY_HTTP_PORT:-8978}/api"
echo ""
echo "常用命令:"
echo "  查看日志: docker logs -f $CONTAINER_NAME"
echo "  停止服务: docker rm -f $CONTAINER_NAME"
echo "  重启服务: docker restart $CONTAINER_NAME"
echo ""
DEPLOY_SCRIPT

chmod +x "$PACKAGE_DIR/deploy.sh"

# 创建 README
echo ""
echo "[创建] 部署说明文档..."
cat > "$PACKAGE_DIR/README.md" <<EOF
# NG Gateway 离线部署包

## 版本信息
- 版本: ${VERSION}
- 平台: ${PLATFORM}
- 构建时间: $(date '+%Y-%m-%d %H:%M:%S')

## 文件说明
- \`deploy.sh\`: 部署脚本，在目标服务器上运行
- \`images/\`: Docker 镜像文件（.tar 格式）
- \`.env\`: 环境变量配置文件（可自定义）

## 部署步骤

### 1. 传输文件到目标服务器
将整个部署包传输到目标 Linux 服务器（ARM64 架构）

### 2. 解压部署包
\`\`\`bash
unzip ng-gateway-offline-${VERSION}.zip
cd ng-gateway-offline-${VERSION}
\`\`\`

### 3. 配置环境变量（可选）
编辑 \`.env\` 文件，修改端口等配置：
\`\`\`bash
nano .env
\`\`\`

### 4. 运行部署脚本
\`\`\`bash
chmod +x deploy.sh
./deploy.sh
\`\`\`

## 服务管理

### 查看服务状态
\`\`\`bash
docker ps --filter "name=ng-gateway"
\`\`\`

### 查看日志
\`\`\`bash
docker logs -f ng-gateway
\`\`\`

### 停止服务
\`\`\`bash
docker rm -f ng-gateway
\`\`\`

### 重启服务
\`\`\`bash
docker restart ng-gateway
\`\`\`

## 端口说明
- Web UI / API: 默认 8978（同端口，网关进程内嵌 UI 静态资源）
- Gateway WebSocket: 默认 8979

可通过修改 \`.env\` 文件中的端口配置来更改。

## 数据持久化
服务使用 Docker volumes 持久化数据：
- \`gateway-data\`: 网关数据（SQLite 等）
- \`gateway-drivers\`: 自定义驱动目录
- \`gateway-plugins\`: 自定义插件目录

## 故障排查
1. 确保 Docker 已正确安装
2. 确保端口未被占用
3. 查看日志: \`docker logs ng-gateway\`
4. 检查镜像是否加载: \`docker images\`
EOF

# 打包为 zip
echo ""
echo "[打包] 创建 ZIP 压缩包..."
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
echo "  - .env (环境变量配置)"
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
