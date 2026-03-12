//! Per-channel GStreamer frame acquisition and inference loop.
//!
//! Each registered channel owns a [`GstFrameSource`] and a background tokio
//! task that continuously pulls decoded frames and runs them through the
//! AI pipeline. Results are forwarded to an optional caller-provided
//! latest-value `watch::Sender<Option<Arc<AnalysisResult>>>`.
//!
//! When WebRTC live preview is enabled and a client has connected,
//! inferred frames are pushed to the WebRTC publisher for encoding and streaming.

use crate::{
    engine::webrtc::{FrameMetadata, StreamStats, WebRtcRegistry},
    frame::{
        memory::{FrameMemory, PixelFormat},
        platform::{detect_hardware_platform, PlatformCapabilities},
        source::{FrameSource, GstFrameSource, GstFrameSourceConfig, RtspTransport, SourceEvent},
    },
    pipeline::sampler::FrameSampler,
    DecodedFrame,
};
use bytes::Bytes;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{AnalysisResult, ChannelRegistration, StreamTransport},
    enums::ai::SamplingStrategy,
};
use std::sync::{Arc, OnceLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Cached platform capabilities (detected once per process).
static PLATFORM_CAPS: OnceLock<PlatformCapabilities> = OnceLock::new();

/// Initialize GStreamer and detect platform capabilities.
///
/// Must be called once before any [`GstFrameSource`] is created.
/// Idempotent — subsequent calls return the cached result.
pub fn ensure_gstreamer_init() -> Result<&'static PlatformCapabilities, AiEngineError> {
    if let Some(caps) = PLATFORM_CAPS.get() {
        return Ok(caps);
    }

    gstreamer::init()
        .map_err(|e| AiEngineError::FrameError(format!("GStreamer initialization failed: {e}")))?;

    let platform = detect_hardware_platform();
    let caps = PlatformCapabilities::probe(platform);

    tracing::info!(
        platform = ?caps.platform,
        hw_decode = caps.supports_hw_decode,
        hw_csc = caps.supports_hw_csc,
        dma_buf = caps.supports_dma_buf,
        hw_encoder = ?caps.hw_encoder,
        "GStreamer initialized, platform capabilities probed"
    );

    Ok(PLATFORM_CAPS.get_or_init(|| caps))
}

/// Get the cached platform capabilities.
///
/// Returns `None` if [`ensure_gstreamer_init`] has not been called yet.
pub fn platform_capabilities() -> Option<&'static PlatformCapabilities> {
    PLATFORM_CAPS.get()
}

/// Trait for the inference logic that a channel's frame loop delegates to.
///
/// This decouples the channel runtime from `AiEngine` directly, avoiding
/// circular references while allowing the frame loop to invoke the full
/// inference pipeline.
#[async_trait::async_trait]
pub trait ChannelFrameProcessor: Send + Sync + 'static {
    /// Process a single decoded frame through the inference pipeline.
    ///
    /// Returns an analysis result or an error. The channel loop handles
    /// backpressure and error logging externally.
    async fn process_frame(
        &self,
        channel_id: i32,
        device_id: i32,
        frame: DecodedFrame,
        frame_seq: u64,
    ) -> Result<AnalysisResult, AiEngineError>;
}

/// Maximum consecutive pipeline restart attempts before giving up.
const MAX_RESTART_ATTEMPTS: u32 = 5;
/// Initial backoff between restart attempts.
const RESTART_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(2);
/// Maximum backoff between restart attempts.
const RESTART_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);

/// Runtime state for a single registered camera channel.
pub struct ChannelRuntime {
    /// Channel identifier.
    pub channel_id: i32,
    /// Device identifier.
    pub device_id: i32,
    /// Cancellation token for graceful shutdown.
    cancel: CancellationToken,
    /// Frame processing task handle.
    task_handle: JoinHandle<()>,
}

impl ChannelRuntime {
    /// Spawn a new channel runtime with a GStreamer frame source.
    ///
    /// The `processor` handles inference for each decoded frame. Results
    /// are forwarded to `result_tx` if provided.
    ///
    /// # Two-level sampling
    ///
    /// The `sampling` strategy (owned by the Pipeline, not the channel)
    /// drives both levels:
    ///
    /// 1. **GStreamer level**: when the strategy yields a finite target FPS,
    ///    a `videorate` element is inserted into the pipeline to cap the
    ///    decoded frame rate, reducing hardware decode and CSC cost.
    /// 2. **Application level**: a [`FrameSampler`] gates which decoded
    ///    frames actually enter the inference pipeline, providing adaptive
    ///    sampling with feedback from inference latency and backpressure.
    pub fn spawn(
        registration: &ChannelRegistration,
        sampling: SamplingStrategy,
        processor: Arc<dyn ChannelFrameProcessor>,
        webrtc_registry: Option<Arc<WebRtcRegistry>>,
        parent_cancel: CancellationToken,
    ) -> Result<Self, AiEngineError> {
        let caps = ensure_gstreamer_init()?;
        let cancel = parent_cancel.child_token();

        let config = GstFrameSourceConfig {
            uri: registration.stream_url.clone(),
            request_dma_buf: caps.supports_dma_buf,
            target_resolution: None,
            rtsp_transport: match registration.transport {
                StreamTransport::TcpFallback => RtspTransport::TcpFallback,
                StreamTransport::Tcp => RtspTransport::Tcp,
                StreamTransport::UdpFallback => RtspTransport::UdpFallback,
            },
            connect_timeout: registration.connect_timeout,
            max_buffers: 2,
            target_fps: sampling_target_fps(&sampling),
        };

        let mut source = GstFrameSource::new(config, caps.clone());
        let channel_id = registration.channel_id;
        let device_id = registration.device_id;
        let cancel_token = cancel.clone();
        let result_tx = registration.result_tx.clone();
        let error_tx = registration.error_tx.clone();
        let webrtc_registry = webrtc_registry.clone();

        let task_handle = tokio::spawn(async move {
            if let Err(e) = source.start().await {
                tracing::error!(channel_id, error = %e, "failed to start GStreamer pipeline");
                if let Some(ref tx) = error_tx {
                    let _ = tx
                        .send(format!("failed to start GStreamer pipeline: {e}"))
                        .await;
                }
                return;
            }

            tracing::info!(channel_id, ?sampling, "channel frame loop started");
            let mut frame_seq: u64 = 0;
            let mut restart_attempts: u32 = 0;
            let mut sampler = FrameSampler::new(&sampling);

            loop {
                tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        tracing::info!(channel_id, "channel frame loop cancelled");
                        break;
                    }
                    source_event = source.next_event() => {
                        match source_event {
                            Ok(SourceEvent::Frame(decoded)) => {
                                restart_attempts = 0;
                                frame_seq = frame_seq.wrapping_add(1);

                                // Level 2: application-layer sampling gate.
                                // Pass the keyframe flag from the GStreamer buffer so
                                // `KeyFrameOnly` strategy can filter delta frames.
                                if !sampler.should_process(frame_seq, decoded.is_keyframe) {
                                    tracing::trace!(
                                        channel_id,
                                        frame_seq,
                                        is_keyframe = decoded.is_keyframe,
                                        "frame skipped (sampler)"
                                    );
                                    continue;
                                }

                                // Clone frame for WebRTC push only when a client is connected.
                                // Resize to config max dimensions to match the publisher's pipeline.
                                let frame_for_webrtc =
                                    webrtc_registry.as_ref().and_then(|reg| reg.get(channel_id))
                                        .filter(|h| h.is_running())
                                        .and_then(|_| {
                                            let f = decoded.try_clone().ok()?;
                                            if f.pixel_format != PixelFormat::Rgb24 {
                                                return None;
                                            }
                                            let (data, w, h) = match &f.memory {
                                                FrameMemory::Cpu(b) => (b.clone(), f.width, f.height),
                                                _ => {
                                                    let cpu = f.memory.to_cpu().ok()?;
                                                    (cpu, f.width, f.height)
                                                }
                                            };
                                            let config = webrtc_registry.as_ref()?.config();
                                            let (tw, th) = (
                                                config.max_width,
                                                config.max_height,
                                            );
                                            if (w, h) == (tw, th) {
                                                Some((data, w, h))
                                            } else {
                                                resize_rgb_to(&data, w, h, tw, th).map(|resized| (resized, tw, th))
                                            }
                                        });

                                let infer_start = std::time::Instant::now();
                                match processor.process_frame(
                                    channel_id,
                                    device_id,
                                    decoded,
                                    frame_seq,
                                ).await {
                                    Ok(result) => {
                                        let latency = infer_start.elapsed().as_secs_f64();
                                        sampler.on_feedback(Some(latency), false);
                                        // Push to WebRTC live preview when client is connected.
                                        if let (Some(reg), Some((data, w, h))) =
                                            (webrtc_registry.as_ref(), frame_for_webrtc)
                                        {
                                            if let Some(handle) = reg.get(channel_id) {
                                                handle.push_frame(data, w, h);
                                                let stats = StreamStats {
                                                    fps_in: 0.0,
                                                    fps_ai: 0.0,
                                                    fps_out: 0.0,
                                                    drop: 0,
                                                };
                                                let metadata =
                                                    FrameMetadata::from_analysis_result(
                                                        &result, stats,
                                                    );
                                                handle.push_metadata(&metadata);
                                            }
                                        }
                                        if let Some(ref tx) = result_tx {
                                            let _ = tx.send(Some(Arc::new(result)));
                                        }
                                    }
                                    Err(AiEngineError::Backpressure) => {
                                        sampler.on_feedback(None, true);
                                        tracing::trace!(
                                            channel_id,
                                            frame_seq,
                                            "frame dropped (backpressure)"
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            channel_id,
                                            frame_seq,
                                            error = %e,
                                            "frame processing error"
                                        );
                                    }
                                }
                            }
                            Ok(event @ (SourceEvent::EndOfStream | SourceEvent::Stalled)) => {
                                let restart_reason = match event {
                                    SourceEvent::EndOfStream => "end-of-stream",
                                    SourceEvent::Stalled => "watchdog stall",
                                    SourceEvent::Frame(_) => unreachable!("frame handled above"),
                                };

                                // Attempt auto-restart with exponential backoff when
                                // the source ends cleanly or the watchdog detects a stall.
                                restart_attempts += 1;
                                if restart_attempts > MAX_RESTART_ATTEMPTS {
                                    tracing::error!(
                                        channel_id,
                                        attempts = MAX_RESTART_ATTEMPTS,
                                        restart_reason,
                                        "pipeline restart limit reached, giving up"
                                    );
                                    if let Some(ref tx) = error_tx {
                                        let _ = tx
                                            .send(format!(
                                                "pipeline restart limit reached ({MAX_RESTART_ATTEMPTS}), reason={restart_reason}"
                                            ))
                                            .await;
                                    }
                                    break;
                                }
                                let backoff = std::cmp::min(
                                    RESTART_BACKOFF_BASE * 2u32.saturating_pow(restart_attempts - 1),
                                    RESTART_BACKOFF_MAX,
                                );
                                tracing::warn!(
                                    channel_id,
                                    restart_reason,
                                    attempt = restart_attempts,
                                    backoff_ms = backoff.as_millis() as u64,
                                    "frame source emitted terminal event, restarting pipeline"
                                );

                                tokio::select! {
                                    _ = tokio::time::sleep(backoff) => {}
                                    _ = cancel_token.cancelled() => {
                                        tracing::info!(channel_id, "restart cancelled during backoff");
                                        break;
                                    }
                                }

                                // Reset sampler state on reconnection.
                                sampler.reset();

                                if let Err(e) = source.restart(None).await {
                                    tracing::error!(
                                        channel_id,
                                        error = %e,
                                        "failed to restart pipeline"
                                    );
                                    continue;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    channel_id,
                                    error = %e,
                                    "frame acquisition error"
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }

            if let Err(e) = source.stop().await {
                tracing::warn!(channel_id, error = %e, "error stopping GStreamer pipeline");
            }
            tracing::info!(channel_id, "channel frame loop exited");
        });

        Ok(Self {
            channel_id,
            device_id,
            cancel,
            task_handle,
        })
    }

    /// Gracefully shut down this channel's frame processing.
    pub async fn shutdown(self) {
        self.cancel.cancel();
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.task_handle).await {
            Ok(Ok(())) => {
                tracing::info!(channel_id = self.channel_id, "channel shut down cleanly");
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    channel_id = self.channel_id,
                    error = %e,
                    "channel task panicked during shutdown"
                );
            }
            Err(_) => {
                tracing::warn!(
                    channel_id = self.channel_id,
                    "channel shutdown timed out (5s)"
                );
            }
        }
    }
}

/// Derive an optional GStreamer-level target FPS from the sampling strategy.
///
/// Returns `Some(fps)` when the strategy implies a bounded frame rate that
/// can be enforced at the GStreamer `videorate` level, reducing decode and
/// CSC overhead. Returns `None` for strategies that require every decoded
/// frame (e.g. `EveryFrame`, `FixedInterval`, `KeyFrameOnly`).
fn sampling_target_fps(strategy: &SamplingStrategy) -> Option<f32> {
    match strategy {
        SamplingStrategy::TargetFps { fps } => Some(*fps),
        _ => None,
    }
}

/// Resize RGB24 bytes to target dimensions for WebRTC pipeline compatibility.
#[cfg(feature = "engine")]
fn resize_rgb_to(data: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Option<Bytes> {
    use fast_image_resize::{images::Image, PixelType, Resizer};
    if dst_w == 0 || dst_h == 0 {
        return None;
    }
    let mut src_buf = data.to_vec();
    let src = Image::from_slice_u8(src_w, src_h, &mut src_buf, PixelType::U8x3).ok()?;
    let mut dst = Image::new(dst_w, dst_h, PixelType::U8x3);
    let mut resizer = Resizer::new();
    resizer.resize(&src, &mut dst, None).ok()?;
    Some(Bytes::from(dst.into_vec()))
}
