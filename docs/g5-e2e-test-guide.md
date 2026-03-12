# G5 端到端验证测试指南

> 目标：在真实设备上完成 Phase G5 所有 DoD 验证项  
> 覆盖：Dynamic Batching / 轨迹告警 / GPU EP / Canvas Overlay / 硬件 JPEG / pipeline 动态重配置  
> 平台：x86 通用 / RK3588 / NVIDIA Jetson

---

## 目录

1. [环境准备](#1-环境准备)
   - [1.1 x86 通用平台](#11-x86-通用平台)
   - [1.2 RK3588 开发板](#12-rk3588-开发板)
   - [1.3 NVIDIA Jetson](#13-nvidia-jetson)
2. [模型准备](#2-模型准备)
3. [RTSP 测试视频源](#3-rtsp-测试视频源)
4. [构建与启动网关](#4-构建与启动网关)
5. [核心 API 操作流程](#5-核心-api-操作流程)
   - [5.1 安装模型](#51-安装模型)
   - [5.2 创建 Pipeline](#52-创建-pipeline)
   - [5.3 创建通道并绑定 Pipeline](#53-创建通道并绑定-pipeline)
   - [5.4 验证推理运行](#54-验证推理运行)
6. [Dynamic Batching 验证](#6-dynamic-batching-验证)
7. [轨迹告警验证](#7-轨迹告警验证)
   - [7.1 Line-Crossing 方向过滤](#71-line-crossing-方向过滤)
   - [7.2 Zone Dwell 超时](#72-zone-dwell-超时)
   - [7.3 告警去重与冷却](#73-告警去重与冷却)
8. [GPU EP 验证](#8-gpu-ep-验证)
9. [Canvas Overlay 验证](#9-canvas-overlay-验证)
10. [硬件 JPEG 编码验证](#10-硬件-jpeg-编码验证)
11. [Pipeline 动态重配置验证](#11-pipeline-动态重配置验证)
12. [压测：10 路并发](#12-压测10-路并发)
13. [RK3588 专项测试](#13-rk3588-专项测试)
14. [Jetson 专项测试](#14-jetson-专项测试)
15. [检查清单](#15-检查清单)

---

## 1. 环境准备

### 1.1 x86 通用平台

**系统要求**：Ubuntu 22.04+ / Debian 12+

```bash
# 基础依赖
sudo apt update && sudo apt install -y \
  build-essential pkg-config protobuf-compiler \
  libsqlite3-dev libudev-dev libssl-dev

# GStreamer 全家桶 (>= 1.20)
sudo apt install -y \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-libav \
  gstreamer1.0-tools

# 可选: VA-API 硬件解码 (Intel)
sudo apt install -y gstreamer1.0-vaapi intel-media-va-driver-non-free

# 可选: NVIDIA CUDA (x86 桌面)
# 需要先安装 CUDA Toolkit 和 ONNX Runtime GPU 版本
# 参考: https://onnxruntime.ai/docs/install/

# Rust 工具链
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable

# RTSP 测试工具
sudo apt install -y ffmpeg
```

**验证 GStreamer 安装**：

```bash
gst-inspect-1.0 --version
# 应显示 >= 1.20

# 验证关键插件
gst-inspect-1.0 decodebin3
gst-inspect-1.0 rtspsrc
gst-inspect-1.0 x264enc
gst-inspect-1.0 jpegenc

# 如果是 Intel 平台，验证 VA-API
gst-inspect-1.0 vaapih264dec
gst-inspect-1.0 vaapijpegenc  # 硬件 JPEG 编码
```

### 1.2 RK3588 开发板

**硬件**：Orange Pi 5 Plus / Rock 5B / Firefly RK3588 等  
**系统**：Ubuntu 22.04 / Orange Pi OS / Rockchip BSP  

> 说明：RK3588 需要同时具备 **内核驱动** 和 **用户态多媒体组件**。  
> 像 `Orange Pi 1.2.0 Jammy` 这类系统通常已经带有 `rga` / `mpp_service` 设备节点，
> 但默认 `ubuntu-ports` 软件源里**没有** `gstreamer1.0-rockchip1`、`librga2`、`librga-dev`，
> 需要额外添加 Rockchip 多媒体仓库。

```bash
# 先确认内核侧驱动是否存在
ls -l /dev/rga /dev/mpp_service

# 先确认当前软件源里是否已提供 Rockchip 用户态包
apt-cache policy gstreamer1.0-rockchip1 librga2 librga-dev

# 如果上面的包显示 "Unable to locate package"，
# 先添加 Rockchip multimedia 仓库（Jammy 可用）
sudo add-apt-repository -y ppa:jjriek/rockchip-multimedia
sudo apt update

# 安装 GStreamer 基础组件 + Rockchip 用户态组件
# 注意：该 PPA 中的包名是 gstreamer1.0-rockchip1
sudo apt install -y \
  gstreamer1.0-rockchip1 gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-libav librga2 librga-dev

# RKNN Runtime
# 一些 Orange Pi / BSP 镜像会自带旧版 librknnrt.so（例如 1.4.0）
# 如果你的 .rknn 模型由较新的 rknn-toolkit2（如 v2.3.2）导出，
# 建议同步升级 runtime，避免出现 "Invalid RKNN model version"。
strings /usr/lib/librknnrt.so 2>/dev/null | grep -i "librknnrt version" || true

# 从 rknn-toolkit2 对应版本获取 runtime
# 仓库路径：
# https://github.com/airockchip/rknn-toolkit2/tree/v2.3.2/rknpu2/runtime/Linux/librknn_api/aarch64
# 建议先备份旧库，再安装到 /usr/local/lib（该目录默认在 ldconfig 搜索路径内）
# 实测：Orange Pi 1.2.0 Jammy 预装的 librknnrt.so 常见为 1.4.0，
# 升级后 ldconfig 会优先解析 /usr/local/lib/librknnrt.so
sudo mkdir -p /usr/local/lib
sudo cp /usr/lib/librknnrt.so /usr/lib/librknnrt.so.bak 2>/dev/null || true
sudo install -m 0755 librknnrt.so /usr/local/lib/librknnrt.so
sudo ldconfig

# 验证 Rockchip GStreamer 插件（核心能力）
gst-inspect-1.0 mppvideodec    # MPP 硬件解码
gst-inspect-1.0 mpph264enc     # MPP H.264 编码
gst-inspect-1.0 mppjpegenc     # MPP JPEG 硬件编码

# 验证 Rockchip RGA 路径
# 当前 Gateway 统一走 `mppvideodec` 内置 RGA，而不是独立的 `rgaconvert` / `rkrgafilter` 插件。
# 只要 `mppvideodec` 可以正常实例化，并且系统具备 `/dev/rga` + `librga2`，
# Gateway 就会将 Rockchip 视为具备硬件 CSC/resize 能力。
gst-inspect-1.0 mppvideodec
ls -l /dev/rga
ldconfig -p | grep librga

# 验证 RKNN
ldconfig -p | grep librknnrt
strings /usr/local/lib/librknnrt.so 2>/dev/null | grep -i "librknnrt version" || \
  strings /usr/lib/librknnrt.so 2>/dev/null | grep -i "librknnrt version"
dmesg | grep -i rknpu | tail -n 5
```

如果 `gst-inspect-1.0 mppvideodec` / `mpph264enc` / `mppjpegenc` 提示 `No such element or plugin`，通常说明：

- 当前镜像没有接入 Rockchip 多媒体用户态仓库
- 或者所用镜像并未提供与当前内核匹配的 Rockchip GStreamer 插件
- 此时优先选择带完整多媒体支持的 BSP / 社区镜像，或者手动编译 `gstreamer1.0-rockchip1` 与 `librga`

如果 `mppvideodec` / `mpph264enc` / `mppjpegenc` 正常，但你担心 RGA 是否可用：

- 先确认 `/dev/rga` 存在，并且 `ldconfig -p | grep librga` 能看到 `librga.so`
- 对当前 Gateway 实现来说，不需要额外安装 `rgaconvert` / `rkrgafilter`
- 只要 `mppvideodec` 可用，Rockchip 路径就会启用内置 RGA 做硬件 CSC/resize

### 1.3 NVIDIA Jetson

**硬件**：Jetson Nano / Xavier NX / Orin Nano 等  
**系统**：JetPack 5.1+ (L4T R35.x+)

```bash
# JetPack 自带 GStreamer 和 NVIDIA 插件
gst-inspect-1.0 nvv4l2decoder   # NVMM 硬件解码
gst-inspect-1.0 nvvidconv        # NVMM 色彩转换
gst-inspect-1.0 nvv4l2h264enc    # NVMM H.264 编码
gst-inspect-1.0 nvjpegenc        # NVIDIA JPEG 硬件编码

# 验证 CUDA
nvcc --version                   # CUDA Toolkit
nvidia-smi                       # 或 tegrastats
ls /usr/lib/aarch64-linux-gnu/libcuda.so*

# ONNX Runtime with CUDA/TensorRT
# 需要安装 onnxruntime-gpu，可从 https://onnxruntime.ai/docs/build/eps.html
# 或使用预编译 aarch64 wheel
pip3 install onnxruntime-gpu  # 仅用于验证环境，Gateway 使用 Rust binding
```

---

## 2. 模型准备

下载以下 ONNX 模型用于测试，这些是公开可用的预训练模型：

```bash
mkdir -p ./ai/models && cd ./ai/models

# 1) YOLOv8n — 目标检测 (轻量级, 适合嵌入式)
wget https://github.com/ultralytics/assets/releases/download/v8.3.0/yolov8n.onnx

# 2) YOLOv8s — 目标检测 (中等精度, 用于 batch 对比测试)
wget https://github.com/ultralytics/assets/releases/download/v8.3.0/yolov8s.onnx

# 3) YOLOv8n-pose — 关键点检测 (用于测试多输出头)
wget https://github.com/ultralytics/assets/releases/download/v8.3.0/yolov8n-pose.onnx

cd ../..
```

**RK3588 专用 RKNN 模型**（需自行转换）：

```bash
# 使用 rknn-toolkit2 将 ONNX 转为 RKNN
# 在 x86 主机上运行 (rknn-toolkit2 不支持 ARM)
pip3 install rknn-toolkit2
python3 - <<'EOF'
from rknn.api import RKNN

rknn = RKNN()
rknn.config(target_platform='rk3588', quantized_algorithm='normal')
rknn.load_onnx(model='yolov8n.onnx')
rknn.build(do_quantization=True, dataset='coco_calib.txt')
rknn.export_rknn('yolov8n.rknn')
rknn.release()
EOF

# 将 .rknn 文件传输到 RK3588
scp yolov8n.rknn user@rk3588-ip:~/ng-gateway/ai/models/
```

---

## 3. RTSP 测试视频源

### 方案 A: Docker MediaMTX + FFmpeg（推荐）

```bash
# 启动 RTSP 服务器
docker run -d --name rtsp-server \
  -p 8554:8554 \
  bluenviron/mediamtx:latest

# 等待启动
sleep 3

# 推送测试流 (640x480 25fps H.264, 带时间戳)
docker run -d --name ffmpeg-source \
  --network host \
  linuxserver/ffmpeg:latest \
  ffmpeg -re \
    -f lavfi -i "testsrc=size=640x480:rate=25,drawtext=text='%{localtime}':fontsize=24:fontcolor=white:x=10:y=10" \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -profile:v baseline -b:v 1M -g 50 \
    -pix_fmt yuv420p \
    -f rtsp -rtsp_transport tcp \
    rtsp://localhost:8554/test-cam

# 验证流可用
ffprobe rtsp://localhost:8554/test-cam
```

### 方案 B: 使用真实 IP 摄像头

```bash
# 验证 RTSP 流可达
ffprobe rtsp://<camera-ip>:554/stream1

# 记录 RTSP URL，后续创建通道时使用
RTSP_URL="rtsp://admin:password@192.168.1.100:554/Streaming/Channels/101"
```

### 方案 C: 多路测试流（10 路压测用）

```bash
# 在 MediaMTX 上创建 10 路不同分辨率的测试流
for i in $(seq 1 10); do
  PORT=$((8554))
  STREAM_NAME="test-cam-$i"

  # 混合分辨率: 偶数用 1080p, 奇数用 720p
  if [ $((i % 2)) -eq 0 ]; then
    RES="1920x1080"
  else
    RES="1280x720"
  fi

  docker run -d --name "ffmpeg-source-$i" \
    --network host \
    linuxserver/ffmpeg:latest \
    ffmpeg -re \
      -f lavfi -i "testsrc=size=${RES}:rate=25,drawtext=text='CAM${i} %{localtime}':fontsize=20:fontcolor=white:x=10:y=10" \
      -c:v libx264 -preset ultrafast -tune zerolatency \
      -profile:v baseline -b:v 2M -g 50 \
      -pix_fmt yuv420p \
      -f rtsp -rtsp_transport tcp \
      rtsp://localhost:${PORT}/${STREAM_NAME}

  echo "Started stream: rtsp://localhost:${PORT}/${STREAM_NAME} (${RES})"
done
```

---

## 4. 构建与启动网关

### x86 本机构建

```bash
cd /path/to/ng-gateway

# Debug 构建 (快速迭代)
cargo xtask build --profile debug --without-ui

# 或 Release 构建 (性能测试)
cargo xtask build --profile release --without-ui

# 创建配置文件
cat > gateway-test.toml << 'EOF'
[general]
runtime_dir = "."

[general.ai]
enabled = true
models_dir = "./ai/models"
algorithms_dir = "./ai/algorithms"
max_concurrent_inferences = 8
decoder_workers = 4
annotate_queue_capacity = 64

[general.ai.inference]
execution_provider = "cpu"
intra_op_threads = 4
sessions_per_model = 2
request_queue_capacity = 32

# Dynamic Batching (G5)
[general.ai.inference.batching]
enabled = true
max_batch_size = 4
collect_timeout_ms = 10
max_queue_depth = 32
adaptive = true

[general.ai.webrtc]
enabled = true
bitrate_kbps = 2000
max_width = 1280
max_height = 720
max_fps = 30
stun_server = "stun://stun.l.google.com:19302"

[web]
host = "0.0.0.0"
port = 8978
router_prefix = "/api"

[web.ssl]
enabled = false

[db.sqlite]
path = "./test-gateway.db"
EOF

# 创建必要目录
mkdir -p ai/models ai/algorithms

# 启动
./target/debug/ng-gateway-bin --config gateway-test.toml
```

### RK3588 交叉编译 + 部署

```bash
# 在 x86 主机上交叉编译
cargo install cross --git https://github.com/cross-rs/cross

XTASK_CARGO=cross cargo xtask build --profile release --without-ui \
  -- --target aarch64-unknown-linux-gnu --features "rknn,dmabuf"

# 打包传输
scp target/aarch64-unknown-linux-gnu/release/ng-gateway-bin user@rk3588-ip:~/ng-gateway/
scp gateway-test.toml user@rk3588-ip:~/ng-gateway/

# 在 RK3588 上配置
ssh user@rk3588-ip
cd ~/ng-gateway

# 修改配置
sed -i 's/execution_provider = "cpu"/execution_provider = "cpu"/' gateway-test.toml
# RKNN 模型走独立的 rknn backend，不需要修改 execution_provider

# 启动
LD_LIBRARY_PATH=/usr/lib/aarch64-linux-gnu:$LD_LIBRARY_PATH \
  ./ng-gateway-bin --config gateway-test.toml
```

### Jetson 本机构建

```bash
# Jetson 上直接编译 (JetPack 自带 GCC + CUDA)
cd ~/ng-gateway
cargo xtask build --profile release --without-ui

# Jetson CUDA EP 配置
cat > gateway-test.toml << 'EOF'
[general.ai]
enabled = true
models_dir = "./ai/models"
max_concurrent_inferences = 4

[general.ai.inference]
execution_provider = "cuda"  # 或 "tensorrt"
intra_op_threads = 2
sessions_per_model = 1

[general.ai.inference.batching]
enabled = true
max_batch_size = 8
collect_timeout_ms = 15
max_queue_depth = 64
adaptive = true

[general.ai.webrtc]
enabled = true
bitrate_kbps = 1500
max_width = 1280
max_height = 720

[web]
host = "0.0.0.0"
port = 8978

[db.sqlite]
path = "./test-gateway.db"
EOF

# 确保 ONNX Runtime CUDA/TensorRT 库可用
export ORT_DYLIB_PATH=/usr/lib/aarch64-linux-gnu/libonnxruntime.so
./target/release/ng-gateway-bin --config gateway-test.toml
```

### 验证网关启动

```bash
GATEWAY=http://localhost:8978

# 健康检查
curl -s $GATEWAY/health | jq .

# AI 引擎状态
curl -s $GATEWAY/api/ai/engine/status | jq .
# 期望: enabled=true, execution_provider="cpu" (或 "cuda")
```

---

## 5. 核心 API 操作流程

设置环境变量便于后续使用：

```bash
GATEWAY=http://localhost:8978
API=$GATEWAY/api
```

### 5.1 安装模型

```bash
# 探测模型 (查看元数据)
curl -s -X POST "$API/ai/models/probe" \
  -F "file=@./ai/models/yolov8n.onnx" | jq .

# 安装模型
curl -s -X POST "$API/ai/models/install" \
  -F "file=@./ai/models/yolov8n.onnx" \
  -F 'metadata={"name":"yolov8n","task":"object_detection","version":"8.3.0"}' | jq .

# 记录返回的 model_id 和 model_key
MODEL_KEY="yolov8n"
MODEL_ID=$(curl -s "$API/ai/models/list" | jq -r '.data[] | select(.modelKey=="yolov8n") | .id')
echo "Model installed: id=$MODEL_ID, key=$MODEL_KEY"

# 验证模型已安装
curl -s "$API/ai/models/list" | jq '.data[] | {id, modelKey, format, task}'

# 热加载模型到推理运行时
curl -s -X POST "$API/ai/models/$MODEL_ID/load" | jq .
```

### 5.2 创建 Pipeline

```bash
# 创建包含推理+追踪+告警规则的 Pipeline
curl -s -X POST "$API/ai/pipelines" \
  -H "Content-Type: application/json" \
  -d '{
    "key": "test-detection-pipeline",
    "name": "Test Detection Pipeline",
    "sampling": { "type": "target_fps", "fps": 5 },
    "roiRegions": [],
    "annotation": {
      "enabled": true,
      "drawBboxes": true,
      "drawLabels": true,
      "drawConfidence": true,
      "drawTrackIds": true,
      "jpegQuality": 85
    },
    "stages": [
      {
        "stageOrder": 0,
        "name": "YOLOv8n Detection",
        "config": {
          "type": "inference",
          "model_id": "'"$MODEL_KEY"'",
          "confidence_threshold": 0.4,
          "nms_iou_threshold": 0.45
        }
      },
      {
        "stageOrder": 1,
        "name": "DeepSORT Tracker",
        "config": {
          "type": "tracker",
          "algorithm": "deep_sort",
          "max_age": 30
        }
      }
    ],
    "alarmRules": [
      {
        "name": "person_detected",
        "ruleOrder": 0,
        "severity": "info",
        "condition": {
          "type": "class_detected",
          "class": "person",
          "min_confidence": 0.5
        },
        "cooldownSecs": 10
      },
      {
        "name": "crowd_alert",
        "ruleOrder": 1,
        "severity": "warning",
        "condition": {
          "type": "count_exceeds",
          "class": "person",
          "threshold": 3
        },
        "cooldownSecs": 30
      },
      {
        "name": "line_crossing_test",
        "ruleOrder": 2,
        "severity": "warning",
        "condition": {
          "type": "line_crossing",
          "line": [[0.5, 0.0], [0.5, 1.0]],
          "class": "person",
          "direction": "any"
        },
        "cooldownSecs": 60
      },
      {
        "name": "zone_dwell_test",
        "ruleOrder": 3,
        "severity": "critical",
        "condition": {
          "type": "zone_dwell",
          "zone": [[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]],
          "class": "person",
          "dwell_timeout_ms": 5000,
          "cooldown_ms": 60000
        },
        "cooldownSecs": 60
      }
    ]
  }' | jq .

PIPELINE_ID=$(curl -s "$API/ai/pipelines/list" | jq -r '.data[0].id')
echo "Pipeline created: id=$PIPELINE_ID"
```

### 5.3 创建通道并绑定 Pipeline

```bash
# 通道创建需要通过设备和通道管理 API
# 这里通过 Camera 驱动配置绑定 pipeline

# 1) 先创建设备 (如果不存在)
DEVICE_ID=1  # 使用已有设备或按需创建

# 2) 创建通道，指定 RTSP 流和 pipeline_id
curl -s -X POST "$API/channel" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "Test Camera 1",
    "deviceId": '"$DEVICE_ID"',
    "driverKey": "camera",
    "driverConfig": {
      "protocol": {
        "type": "rtsp",
        "url": "rtsp://localhost:8554/test-cam",
        "transport": "tcp"
      },
      "pipelineId": '"$PIPELINE_ID"'
    }
  }' | jq .

CHANNEL_ID=$(curl -s "$API/channel/list" | jq -r '.data[0].id')
echo "Channel created: id=$CHANNEL_ID"
```

### 5.4 验证推理运行

```bash
# 等待几秒让推理开始
sleep 5

# 检查引擎状态 — 应看到 active_count > 0
curl -s "$API/ai/engine/status" | jq '{
  enabled,
  executionProvider: .executionProvider,
  activeInferences: .inference.activeCount,
  totalInferences: .inference.totalInferences,
  avgLatencyMs: .inference.avgLatencyMs,
  modelsLoaded: .models.loaded
}'

# 获取最新标注快照 (JPEG)
curl -s -o snapshot.jpg "$API/ai/channels/$CHANNEL_ID/snapshot"
file snapshot.jpg  # 应显示 JPEG image data
open snapshot.jpg  # macOS: 查看标注结果

# 查看告警事件
curl -s "$API/ai/alarms/page?page=1&pageSize=10" | jq '.data.records[] | {id, alarmType, severity, description, payload}'
```

---

## 6. Dynamic Batching 验证

### 6.1 启用 Batching 的配置

确保 `gateway-test.toml` 中：

```toml
[general.ai.inference.batching]
enabled = true
max_batch_size = 4
collect_timeout_ms = 10
max_queue_depth = 32
adaptive = true
```

### 6.2 创建多路通道

```bash
# 创建 4 路通道，全部绑定同一个 pipeline + 同一个模型
for i in $(seq 1 4); do
  curl -s -X POST "$API/channel" \
    -H "Content-Type: application/json" \
    -d '{
      "name": "Batch Test Camera '"$i"'",
      "deviceId": '"$DEVICE_ID"',
      "driverKey": "camera",
      "driverConfig": {
        "protocol": {
          "type": "rtsp",
          "url": "rtsp://localhost:8554/test-cam-'"$i"'",
          "transport": "tcp"
        },
        "pipelineId": '"$PIPELINE_ID"'
      }
    }' | jq -r '.data.id // "failed"'
done
```

### 6.3 观测 Batching 指标

```bash
# 等待收集数据
sleep 30

# 查看 Prometheus 指标
curl -s "$GATEWAY/metrics" | grep ai_batch

# 期望指标:
# ai_batch_size_bucket{le="1"} ...
# ai_batch_size_bucket{le="2"} ...
# ai_batch_size_bucket{le="4"} ...  ← 大部分应该落在这里
# ai_batch_queue_depth ...
# ai_batch_flushes_total{reason="full"} ...   ← batch 满 flush
# ai_batch_flushes_total{reason="timeout"} ... ← 超时 flush
# ai_batch_flushes_total{reason="single"} ...  ← 单帧 flush
```

### 6.4 Batch=1 vs Batch=4 vs Batch=8 对比

```bash
# 测试脚本: 每个配置运行 60 秒，记录吞吐和延迟

for BATCH_SIZE in 1 4 8; do
  echo "=== Testing batch_size=$BATCH_SIZE ==="

  # 修改配置并重启 (或通过环境变量覆盖)
  # NG__GENERAL__AI__INFERENCE__BATCHING__MAX_BATCH_SIZE=$BATCH_SIZE

  # 清空指标
  # (重启网关)

  sleep 60

  echo "batch_size=$BATCH_SIZE results:"
  curl -s "$API/ai/engine/status" | jq '{
    totalInferences: .inference.totalInferences,
    avgLatencyMs: .inference.avgLatencyMs,
    activeCount: .inference.activeCount,
    availablePermits: .inference.availablePermits
  }'

  # 记录 P95/P99 延迟 (从 Prometheus histogram)
  curl -s "$GATEWAY/metrics" | grep 'ai_inference_latency_seconds'
  echo "---"
done
```

---

## 7. 轨迹告警验证

### 7.1 Line-Crossing 方向过滤

**测试场景**：人从左向右穿越画面中央的垂直线。

```bash
# Pipeline 中已包含 line_crossing_test 规则:
# line: [[0.5, 0.0], [0.5, 1.0]] (垂直中线)
# direction: "any"

# 使用有行人穿越场景的视频流
# 或使用 FFmpeg 生成运动物体的测试流:
ffmpeg -re \
  -f lavfi -i "color=c=green:size=640x480:rate=25,drawbox=x='mod(n*3,640)':y=200:w=40:h=60:c=red:t=fill" \
  -c:v libx264 -preset ultrafast -tune zerolatency \
  -f rtsp -rtsp_transport tcp rtsp://localhost:8554/crossing-test

# 等待检测
sleep 30

# 查看 line_crossing 告警
curl -s "$API/ai/alarms/page?alarmType=line_crossing&page=1&pageSize=10" | jq '.data.records'

# 验证告警 payload 包含轨迹数据
ALARM_ID=$(curl -s "$API/ai/alarms/page?page=1&pageSize=1" | jq -r '.data.records[0].id')
curl -s "$API/ai/alarms/detail/$ALARM_ID" | jq '.data.payload'
# 期望: 包含 trackId, polyline, velocityAtTrigger, directionAtTrigger
```

### 7.2 Zone Dwell 超时

```bash
# Pipeline 中已包含 zone_dwell_test 规则:
# zone: 画面中央区域 (0.2-0.8, 0.2-0.8)
# dwell_timeout_ms: 5000 (5秒)

# 需要有物体在区域内停留超过 5 秒的场景
# 使用静态检测模式更容易触发:

# 等待 DwellTimeout 事件
sleep 30

# 查看 zone_dwell 告警
curl -s "$API/ai/alarms/page?alarmType=zone_dwell&page=1&pageSize=10" | jq '.data.records[] | {
  id, description, severity,
  payload_type: (.payload | type),
  has_trajectory: (.payload.trackId != null)
}'
```

### 7.3 告警去重与冷却

```bash
# 验证: 同一 track + 同一 rule 在 cooldown 窗口内只触发一次

# 1) 查看 60 秒内的所有告警
curl -s "$API/ai/alarms/page?page=1&pageSize=100" | jq '[.data.records[] | {
  id, alarmType, createdAt
}] | group_by(.alarmType) | .[] | {
  type: .[0].alarmType,
  count: length,
  first: .[0].createdAt,
  last: .[-1].createdAt
}'

# 2) 同一个 line_crossing rule 的两次触发间隔应 >= cooldownSecs (60s)
curl -s "$API/ai/alarms/page?alarmType=line_crossing&page=1&pageSize=50" | \
  jq '[.data.records[].createdAt] | sort |
  [range(1; length) as $i | {
    gap_seconds: ((.[i] | fromdateiso8601) - (.[$i-1] | fromdateiso8601))
  }] | .[] | select(.gap_seconds < 60)'
# 期望: 空数组 (所有间隔 >= 60s)
```

---

## 8. GPU EP 验证

### 8.1 CUDA EP (x86 桌面 或 Jetson)

```bash
# 配置: execution_provider = "cuda"
# 启动网关后检查生效的 EP

curl -s "$API/ai/engine/status" | jq '.executionProvider'
# 期望: "cuda"
# 如果 CUDA 不可用，会自动降级到 "cpu" 并在日志中 warn

# 查看网关启动日志
grep -i "execution provider" gateway.log
# 期望: "CUDA execution provider registered"
# 或:   "CUDA EP registration failed, falling back to CPU"
```

### 8.2 TensorRT EP (Jetson)

```bash
# 修改配置
sed -i 's/execution_provider = "cuda"/execution_provider = "tensorrt"/' gateway-test.toml

# 重启后检查
curl -s "$API/ai/engine/status" | jq '.executionProvider'
# 期望: "tensorrt"

# TensorRT 首次加载会建立 engine cache，耗时较长 (1-5 分钟)
# 后续加载使用缓存，速度很快
```

### 8.3 GPU OOM 降级验证

```bash
# 模拟 OOM: 加载一个超大模型，或限制 GPU 内存
# Jetson: 可用 `nvidia-smi` 设置 compute mode 或内存限制

# 观察日志:
grep -i "GPU OOM detected\|degrading model to CPU" gateway.log
# 期望: 出现降级日志，后续推理自动使用 CPU

# 验证降级后推理仍正常
curl -s "$API/ai/engine/status" | jq '.inference'
```

---

## 9. Canvas Overlay 验证

### 9.1 连接 WebRTC 预览

1. 打开浏览器访问 `http://<gateway-ip>:8978`
2. 导航到 **AI → Live Preview**
3. 选择一个已创建的通道
4. 点击 **Start Preview**

### 9.2 图层管理器

1. 点击工具栏的 **◫ (Layers)** 按钮
2. 逐个取消勾选：
   - [ ] 取消 **BBox / Labels** → BBox 和标签消失，视频正常播放
   - [ ] 取消 **ROI Regions** → ROI 框消失
   - [ ] 取消 **Trajectories** → 轨迹线消失
   - [ ] 取消 **Heatmap** → 热力图叠加消失
3. 重新全部勾选，验证恢复

### 9.3 ROI 交互式编辑

1. 点击工具栏的 **▭ (Draw ROI)** 按钮（变蓝高亮）
2. 在视频画面上拖拽绘制一个矩形区域
3. 松开鼠标后，应看到蓝色虚线 ROI 框出现
4. 再绘制第二个 ROI
5. 点击 **ROI 2** 按钮打开 ROI 管理面板
6. 删除一个 ROI，验证画面上相应框消失
7. 点击 **Clear All**，验证所有 ROI 清除
8. 再次点击 **▭** 退出编辑模式

### 9.4 快照合成

1. 确保有 BBox 和 ROI 叠加显示
2. 点击 **📸 (Snapshot)** 按钮
3. 验证下载的 JPEG 图片包含视频 + 所有叠加层

---

## 10. 硬件 JPEG 编码验证

```bash
# 查看网关启动日志中的 JPEG 编码器探测结果
grep -i "jpeg encoder" gateway.log

# x86 VA-API: 期望 "hardware JPEG encoder available: vaapijpegenc"
# RK3588:     期望 "hardware JPEG encoder available: mppjpegenc"
# Jetson:     期望 "hardware JPEG encoder available: nvjpegenc"
# Generic:    期望 "no hardware JPEG encoder, using software fallback"

# 对比标注快照生成速度:
# 1) 强制软件 JPEG (不使用 GStreamer 编码)
# 2) 硬件 JPEG (自动检测)
# 通过抓取快照并计时:
time curl -s -o /dev/null "$API/ai/channels/$CHANNEL_ID/snapshot"
```

---

## 11. Pipeline 动态重配置验证

### 11.1 动态帧率调整

```bash
# 通过 WebRTC DataChannel control 或 Engine API 调整帧率
# (需要在 Engine 层暴露 set_fps_live 的 API，当前通过内部调用)

# 验证: 修改采样策略不需要重启 pipeline
# 观察日志:
grep "pipeline framerate dynamically reconfigured" gateway.log
```

### 11.2 动态分辨率调整

```bash
# 通过 WebRTC 控制面板调整分辨率
# 在 Live Preview 页面:
# 1. 点击 ⚙ (Settings)
# 2. 将 Resolution 从 720p 改为 480p
# 3. 点击 Set

# 观察日志:
grep "pipeline resolution dynamically reconfigured" gateway.log

# 验证视频流分辨率变化 (无中断)
```

---

## 12. 压测：10 路并发

### 12.1 环境准备

```bash
# 确保 10 路 RTSP 测试流已启动 (参考第 3 节方案 C)
# 确保 gateway-test.toml 中:
#   max_concurrent_inferences = 16
#   annotate_queue_capacity = 128
#   batching.enabled = true
#   batching.max_batch_size = 4
```

### 12.2 创建 10 路通道

```bash
for i in $(seq 1 10); do
  curl -s -X POST "$API/channel" \
    -H "Content-Type: application/json" \
    -d '{
      "name": "Stress Test Camera '"$i"'",
      "deviceId": '"$DEVICE_ID"',
      "driverKey": "camera",
      "driverConfig": {
        "protocol": {
          "type": "rtsp",
          "url": "rtsp://localhost:8554/test-cam-'"$i"'",
          "transport": "tcp"
        },
        "pipelineId": '"$PIPELINE_ID"'
      }
    }' | jq -r '.data.id // "failed"'
done
```

### 12.3 数据采集 (运行 5 分钟)

```bash
# 1) 每 10 秒采样引擎状态
for i in $(seq 1 30); do
  echo "=== Sample $i ($(date)) ==="
  curl -s "$API/ai/engine/status" | jq '{
    totalInferences: .inference.totalInferences,
    avgLatencyMs: .inference.avgLatencyMs,
    activeCount: .inference.activeCount,
    availablePermits: .inference.availablePermits,
    modelsLoaded: .models.loaded,
    totalMemory: .models.totalMemoryBytes
  }'
  sleep 10
done | tee stress-test-results.jsonl

# 2) 采集 Prometheus 指标
curl -s "$GATEWAY/metrics" | grep -E '^ai_' > stress-metrics.txt

# 3) 采集系统资源
top -bn1 | head -20 > stress-system.txt
free -m >> stress-system.txt

# Jetson: tegrastats 采集 GPU/NPU 利用率
# tegrastats --interval 1000 --logfile stress-tegrastats.log &
# sleep 300 && kill %1

# RK3588: 采集 NPU 利用率
# cat /sys/kernel/debug/rknpu/load
```

### 12.4 压测报告检查点

| 指标 | 期望值 (x86 CPU) | 期望值 (Jetson CUDA) | 期望值 (RK3588 RKNN) |
|------|----------|----------|----------|
| 10 路 P95 延迟 | < 200 ms | < 50 ms | < 30 ms |
| 10 路 P99 延迟 | < 500 ms | < 100 ms | < 60 ms |
| 吞吐 (frames/sec) | > 30 | > 80 | > 100 |
| CPU 利用率 | < 80% | < 30% | < 20% |
| 内存 (RSS) | < 2 GB | < 1 GB | < 800 MB |
| 丢帧率 | < 5% | < 1% | < 1% |
| Batch 平均大小 | ~3-4 | ~4-8 | ~4 |

---

## 13. RK3588 专项测试

### 13.1 验证硬件加速链路

```bash
# 检查平台检测
grep -E "platform|hw_decode|hw_csc|dma_buf|hw_encoder|hw_jpeg" gateway.log
# 期望:
#   platform: Rockchip
#   hw_decode: true
#   hw_csc: true
#   hw_resize: true
#   说明：当前实现中，Rockchip 的 hw_csc/hw_resize 来自 mppvideodec 内置 RGA，
#         不再依赖独立的 rkrgafilter / rgaconvert 插件
#   dma_buf: true
#   hw_encoder: Some("mpph264enc")
#   hw_jpeg_encoder: Some("mppjpegenc")

# 验证 DMA-buf 零拷贝
grep "DMA-buf" gateway.log
# 期望: 不应看到 "DMA-buf extraction failed"
```

### 13.1.1 验证 `decodebin3 -> mppvideodec -> 内置 RGA`

```bash
# 1) 先验证 decodebin3 在 Rockchip 上会选中 mppvideodec
# 这里用本地合成 H.264 码流，避免依赖外部 RTSP 源
gst-launch-1.0 -v \
  videotestsrc num-buffers=10 ! video/x-raw,format=NV12,width=640,height=480 \
  ! mpph264enc ! h264parse ! decodebin3 ! fakesink 2>&1 | grep -E "GstMppVideoDec|mppvideodec"

# 期望输出中出现类似：
#   GstDecodebin3:decodebin3-0/GstMppVideoDec:mppvideodec0

# 2) 再直接验证 mppvideodec 的内置 RGA 能完成 RGB 输出 + resize
gst-launch-1.0 -v \
  videotestsrc num-buffers=10 ! video/x-raw,format=NV12,width=640,height=480 \
  ! mpph264enc ! h264parse \
  ! mppvideodec format=RGB width=320 height=320 \
  ! identity silent=false ! fakesink 2>&1 | grep -E "GstMppVideoDec|identity0|caps = video/x-raw"

# 期望输出中出现类似：
#   GstMppVideoDec:mppvideodec0.GstPad:src: caps = video/x-raw, format=(string)RGB, width=(int)320, height=(int)320

# 3) 最后验证 Gateway 运行时，decodebin3 内部创建的 mppvideodec
#    已被我们的 deep-element-added 回调配置为 RGA CSC/resize 路径
grep -E "configuring mppvideodec built-in RGA|mppvideodec RGA resize configured" gateway.log

# 期望：
#   configuring mppvideodec built-in RGA for hardware CSC + resize
#   mppvideodec RGA resize configured
#
# 如果你的 channel / model 没配置 target_resolution，则第二条 resize 日志可能不存在，属正常。
```

### 13.1.2 验证 RTSP 实流走到 Rockchip 硬件链路

```bash
# 建议直接复用第 3 节准备好的 RTSP_URL
# 例如：
# RTSP_URL="rtsp://localhost:8554/test-cam"

# 1) 如果测试流是 H.264，先在 GStreamer 层验证 RTSP -> depay -> parse -> decodebin3
#    最终是否落到 mppvideodec
gst-launch-1.0 -v \
  rtspsrc location="$RTSP_URL" latency=100 protocols=tcp \
  ! rtph264depay ! h264parse ! decodebin3 ! fakesink 2>&1 \
  | grep -E "GstMppVideoDec|mppvideodec|caps = video/x-raw"

# 期望输出中出现类似：
#   GstDecodebin3:decodebin3-0/GstMppVideoDec:mppvideodec0
#
# 如果你的 RTSP 实流是 H.265 / H.265+，把上面的 depay/parser 改成：
#   ! rtph265depay ! h265parse !

# 2) 启动 Gateway 后，直接从日志确认 RTSP 场景下已经进入 Rockchip 硬件路径
grep -E "RTSP stream codec detected|decodebin3 produced video pad|configuring mppvideodec built-in RGA|mppvideodec RGA resize configured" gateway.log

# 期望日志中至少出现：
#   RTSP stream codec detected
#   decodebin3 produced video pad, building postprocess chain
#   configuring mppvideodec built-in RGA for hardware CSC + resize
#
# 如果 pipeline/channel 配置了目标分辨率，还应看到：
#   mppvideodec RGA resize configured

# 3) 结合平台能力日志，确认 RTSP 运行时仍保持硬件链路
grep -E "platform|hw_decode|hw_csc|hw_resize|dma_buf" gateway.log

# 期望：
#   platform: Rockchip
#   hw_decode: true
#   hw_csc: true
#   hw_resize: true
#   dma_buf: true
```

### 13.2 RKNN NPU 推理

```bash
# 安装 RKNN 模型 (需先转换)
curl -s -X POST "$API/ai/models/install" \
  -F "file=@./ai/models/yolov8n.rknn" \
  -F 'metadata={"name":"yolov8n-rknn","task":"object_detection"}' | jq .

# 创建使用 RKNN 模型的 pipeline
# ... (同 5.2，替换 model_id 为 rknn 模型)

# 验证 NPU 利用率
cat /sys/kernel/debug/rknpu/load
# 期望: > 50%

# 验证全链路零拷贝 (理想情况下 0 CPU 内存拷贝)
grep "copy_bytes_total" <(curl -s "$GATEWAY/metrics")
```

### 13.3 RK3588 三核 NPU 配置

```bash
# 修改 gateway-test.toml
# [general.ai.inference]
# rknn_core_mask = 7  # 使用全部 3 个 NPU 核心

# 重启并对比单核 vs 三核吞吐:
# rknn_core_mask = 1 → 单核
# rknn_core_mask = 7 → 三核
```

---

## 14. Jetson 专项测试

### 14.1 验证 CUDA EP

```bash
grep "execution provider" gateway.log
# 期望: "CUDA execution provider registered"
# 或:   "TensorRT + CUDA execution providers registered"
```

### 14.2 TensorRT 引擎缓存

```bash
# 首次加载 TensorRT 会很慢 (编译 engine)
# 第二次加载应使用缓存

# 首次加载时间
grep "model loaded\|model load" gateway.log | head -5

# 检查 TRT engine 缓存
ls -la ai/models/*.engine 2>/dev/null
```

### 14.3 Jetson 功耗模式

```bash
# 设置最大性能模式
sudo nvpmodel -m 0
sudo jetson_clocks

# 或低功耗模式测试
sudo nvpmodel -m 1

# 采集 tegrastats
tegrastats --interval 1000 --logfile jetson-stats.log &
sleep 120 && kill %1
cat jetson-stats.log | tail -20
```

### 14.4 GPU OOM 降级测试

```bash
# 加载多个大模型耗尽 GPU 内存
for model in yolov8n yolov8s yolov8m yolov8l yolov8x; do
  curl -s -X POST "$API/ai/models/install" \
    -F "file=@./ai/models/${model}.onnx" \
    -F "metadata={\"name\":\"${model}\"}" | jq -r '.data.id'
done

# 为每个模型创建 pipeline 和通道
# 观察日志:
grep "GPU OOM detected\|degrading model to CPU" gateway.log
```

---

## 15. 检查清单

### Phase G5 DoD 验证

| # | 验证项 | 方法 | 通过 |
|---|--------|------|------|
| 1 | 压测: 10 路 1080p x 5 FPS，batch=4 时 P95/P99 延迟 | 第 12 节 | [ ] |
| 2 | Dynamic Batching 对比: batch=1 vs batch=4 vs batch=8 | 第 6.4 节 | [ ] |
| 3 | Line-crossing 方向过滤正确 | 第 7.1 节 | [ ] |
| 4 | Zone dwell 超时触发正确 | 第 7.2 节 | [ ] |
| 5 | 告警去重: cooldown 窗口内不重复触发 | 第 7.3 节 | [ ] |
| 6 | 告警 payload 包含轨迹数据 (polyline + velocity) | 第 7.1 节，查看 payload | [ ] |
| 7 | GPU EP 至少一个平台验证通过 (CUDA 或 TensorRT) | 第 8 节 | [ ] |
| 8 | GPU OOM 自动降级到 CPU | 第 8.3 节 或 14.4 节 | [ ] |
| 9 | Canvas BBox 图层开关 | 第 9.2 节 | [ ] |
| 10 | Canvas ROI 编辑: 绘制 → 删除 → 清除 | 第 9.3 节 | [ ] |
| 11 | Canvas 快照合成包含叠加层 | 第 9.4 节 | [ ] |
| 12 | 硬件 JPEG 编码检测并使用 (非 Generic 平台) | 第 10 节 | [ ] |
| 13 | Pipeline 动态帧率重配置 (无需重启) | 第 11.1 节 | [ ] |
| 14 | Pipeline 动态分辨率重配置 (无需重启) | 第 11.2 节 | [ ] |
| 15 | FrameSampler::KeyFrameOnly 只处理关键帧 | 修改 sampling 为 key_frame_only | [ ] |
| 16 | 自适应策略: 高负载时 timeout 自动增大 | 观测 Prometheus 指标 | [ ] |
| 17 | Batch 指标上报: histogram + queue_depth + flush_reason | 第 6.3 节 | [ ] |

### 平台矩阵

| 验证项 | x86 CPU | x86 CUDA | RK3588 RKNN | Jetson CUDA | Jetson TRT |
|--------|---------|----------|-------------|-------------|------------|
| 基础推理 | [ ] | [ ] | [ ] | [ ] | [ ] |
| Batching | [ ] | [ ] | [ ] | [ ] | [ ] |
| 轨迹告警 | [ ] | [ ] | [ ] | [ ] | [ ] |
| WebRTC 预览 | [ ] | [ ] | [ ] | [ ] | [ ] |
| 硬件 JPEG | N/A | [ ] | [ ] | [ ] | [ ] |
| DMA-buf 零拷贝 | N/A | N/A | [ ] | [ ] | N/A |
| 72h 稳定性 | [ ] | [ ] | [ ] | [ ] | [ ] |
