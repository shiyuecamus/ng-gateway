# E2E Test Infrastructure — AI Vision Pipeline

End-to-end test environment for the ng-gateway AI vision pipeline.

## Architecture

```
┌──────────────────┐     RTSP/TCP     ┌──────────────────┐
│  FFmpeg           │ ───────────────▶ │  MediaMTX         │
│  (test pattern    │   publish        │  (RTSP server)    │
│   640×480 25fps)  │                  │  :8554            │
└──────────────────┘                  └────────┬─────────┘
                                               │ rtsp://rtsp-server:8554/test-cam
                                               ▼
                                     ┌──────────────────┐
                                     │  NG Gateway       │
                                     │  (AI engine)      │
                                     │  :5678 / :5679    │
                                     └──────────────────┘
```

## Quick Start

```bash
# Start infrastructure
docker compose up -d

# Wait for services to initialize
sleep 10

# Run E2E tests
./run-e2e.sh

# Teardown
docker compose down -v
```

## Test Stream

The FFmpeg container generates a **SMPTE color bars test pattern** at 640×480 25fps
with a real-time timestamp overlay, published as H.264 baseline profile over RTSP.

To verify the stream manually:

```bash
# VLC
vlc rtsp://localhost:8554/test-cam

# FFplay
ffplay -rtsp_transport tcp rtsp://localhost:8554/test-cam

# FFprobe (metadata only)
ffprobe -v quiet -print_format json -show_streams rtsp://localhost:8554/test-cam
```

## Test Matrix

| # | Test | Validates |
|---|------|-----------|
| 1 | Gateway health | Service is running |
| 2 | Engine status API | AI engine initialized, config correct |
| 3 | Model listing | Registry accessible |
| 4 | Pipeline listing | Pipeline CRUD works |
| 5 | Processor listings | Built-in processors registered |
| 6 | Snapshot (no pipeline) | Correct 404 response |
| 7 | RTSP stream check | Test source is publishing |
| 8 | Reconnection resilience | Gateway survives RTSP server restart |

## Customization

```bash
# Use a custom gateway image
GATEWAY_IMAGE=my-registry/ng-gateway GATEWAY_TAG=dev docker compose up -d

# Point test runner at a different gateway
./run-e2e.sh http://my-gateway:5678
```
