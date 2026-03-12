//! GStreamer frame source implementation.
//!
//! This is the primary [`FrameSource`](super::FrameSource) implementation,
//! using a GStreamer pipeline that automatically selects hardware-accelerated
//! decoders and color space converters based on the detected platform.
//!
//! # Pipeline construction
//!
//! Pipelines are built programmatically via [`PipelineBuilder`] using
//! `ElementFactory::make()` API calls (not string concatenation). This
//! provides:
//! - **Type safety**: element/property names are checked at runtime creation
//!   rather than buried in format strings.
//! - **Multi-codec support**: `decodebin3` automatically detects the incoming
//!   codec (H.264, H.265, VP9, AV1, MJPEG) and selects the best decoder.
//! - **Fine-grained control**: each element's properties can be tuned
//!   individually after creation.
//!
//! # Buffer extraction
//!
//! Platform-specific [`BufferExtractor`] implementations handle the critical
//! bridge between GStreamer's buffer model and our [`FrameMemory`] abstraction:
//!
//! | Platform | Extractor | Zero-copy path |
//! |----------|-----------|----------------|
//! | RK3588 | `DmaBufExtractor` | DMA-buf fd → `FrameMemory::DmaBuf` |
//! | Jetson | `JetsonExtractor` | NVMM → CUDA ptr (with `cuda` feature) |
//! | VAAPI | `DmaBufExtractor` | DMA-buf fd → `FrameMemory::DmaBuf` |
//! | Generic | `CpuExtractor` | GstBuffer map → `FrameMemory::GstBufferRef` |

use super::{extractor, pipeline::PipelineBuilder, FrameSource, SourceEvent};
use crate::{decoded::DecodedFrame, frame::platform::PlatformCapabilities};
use gstreamer::prelude::*;
use ng_gateway_error::ai::AiEngineError;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// Configuration for starting a GStreamer frame source.
#[derive(Debug, Clone)]
pub struct GstFrameSourceConfig {
    /// Video stream URI (rtsp://, http://, file://, v4l2://).
    pub uri: String,
    /// Whether to request DMA-buf output (if hardware supports it).
    pub request_dma_buf: bool,
    /// Target output resolution (hardware resize if available).
    pub target_resolution: Option<(u32, u32)>,
    /// RTSP transport preference.
    pub rtsp_transport: RtspTransport,
    /// Connection timeout.
    pub connect_timeout: std::time::Duration,
    /// Maximum buffer count for appsink (drop older frames on overflow).
    pub max_buffers: u32,
    /// Optional GStreamer-level frame rate cap via `videorate`.
    ///
    /// When set, a `videorate ! video/x-raw,framerate=<fps>/1` pair is inserted
    /// after decoding + CSC to limit how many frames reach the appsink, saving
    /// downstream CPU/GPU cost. This is **Level 1** of the two-level sampling
    /// architecture — the coarse, pipeline-level rate limiter.
    pub target_fps: Option<f32>,
}

impl Default for GstFrameSourceConfig {
    fn default() -> Self {
        Self {
            uri: String::new(),
            request_dma_buf: true,
            target_resolution: None,
            rtsp_transport: RtspTransport::TcpFallback,
            connect_timeout: std::time::Duration::from_secs(10),
            max_buffers: 2,
            target_fps: None,
        }
    }
}

/// RTSP transport protocol preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspTransport {
    /// Prefer UDP, fall back to TCP interleaved on failure.
    UdpFallback,
    /// TCP interleaved only (reliable, higher latency).
    Tcp,
    /// Prefer TCP, fall back to UDP on failure.
    TcpFallback,
}

/// Latest liveness state published by the watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceLiveness {
    /// Frames are flowing normally.
    Healthy,
    /// The source stopped producing frames for too long.
    Stalled,
}

/// GStreamer-based frame source.
///
/// Constructs a GStreamer pipeline programmatically for each video stream:
///
/// ```text
/// source → decodebin3 → [platform CSC/resize] → [videorate] → appsink
/// ```
///
/// The pipeline automatically negotiates the best available hardware
/// decoder and color space converter based on the platform via `decodebin3`.
///
/// # Multi-codec support
///
/// `decodebin3` automatically detects the incoming video codec and selects
/// the optimal decoder. Supported codecs include H.264, H.265/HEVC, VP8,
/// VP9, AV1, and MJPEG. Hardware decoders are preferred when available
/// (via GStreamer's element ranking system).
///
/// # Zero-copy output
///
/// On platforms with hardware decoders (RK3588, Jetson, VAAPI), the
/// appsink receives GstBuffers backed by DMA-buf or device memory.
/// Platform-specialized [`BufferExtractor`](super::extractor::BufferExtractor)
/// implementations extract frames without CPU copies.
///
/// On generic x86 (software decode), the appsink receives regular CPU
/// buffers held as `FrameMemory::GstBufferRef` — zero-copy borrow of
/// the GStreamer-managed mapped buffer.
///
/// # Thread Safety
///
/// GStreamer pipelines run on their own thread pool. The appsink callback
/// delivers frames to a bounded async channel, bridging GStreamer's
/// push model to our async pull model.
pub struct GstFrameSource {
    /// GStreamer pipeline instance.
    pipeline: Option<gstreamer::Pipeline>,
    /// Async channel for frame delivery from appsink callback.
    frame_rx: mpsc::Receiver<DecodedFrame>,
    /// Sender held by the appsink callback (kept for lifetime management).
    frame_tx: Option<mpsc::Sender<DecodedFrame>>,
    /// Detected platform capabilities.
    capabilities: PlatformCapabilities,
    /// Whether the pipeline is currently running.
    running: bool,
    /// Source configuration.
    config: GstFrameSourceConfig,
    /// Monotonic frame counter incremented by the appsink callback.
    watchdog_frame_count: Arc<AtomicU64>,
    /// Cooperative cancellation signal for the watchdog task and frame waiters.
    watchdog_cancel: CancellationToken,
    /// Broadcasts the latest liveness state to frame consumers.
    liveness_tx: watch::Sender<SourceLiveness>,
    /// Receiver used by `next_event()` to observe watchdog liveness transitions.
    liveness_rx: watch::Receiver<SourceLiveness>,
    /// Watchdog task handle (cancelled on stop/drop).
    watchdog_handle: Option<tokio::task::JoinHandle<()>>,
    /// Bus watch guard — kept alive to prevent GStreamer from removing the
    /// bus message handler. Dropped when the pipeline is stopped.
    _bus_watch_guard: Option<gstreamer::bus::BusWatchGuard>,
}

impl GstFrameSource {
    /// Create a new GStreamer frame source with the given configuration.
    pub fn new(config: GstFrameSourceConfig, capabilities: PlatformCapabilities) -> Self {
        let (tx, rx) = mpsc::channel(config.max_buffers as usize + 1);
        let (liveness_tx, liveness_rx) = watch::channel(SourceLiveness::Healthy);
        Self {
            pipeline: None,
            frame_rx: rx,
            frame_tx: Some(tx),
            capabilities,
            running: false,
            config,
            watchdog_frame_count: Arc::new(AtomicU64::new(0)),
            watchdog_cancel: CancellationToken::new(),
            liveness_tx,
            liveness_rx,
            watchdog_handle: None,
            _bus_watch_guard: None,
        }
    }

    /// Get the platform capabilities.
    #[inline]
    pub fn capabilities(&self) -> &PlatformCapabilities {
        &self.capabilities
    }
}

#[async_trait::async_trait]
impl FrameSource for GstFrameSource {
    async fn start(&mut self) -> Result<(), AiEngineError> {
        if self.running {
            return Ok(());
        }

        tracing::info!(
            uri = %self.config.uri,
            platform = ?self.capabilities.platform,
            "building programmatic GStreamer pipeline"
        );

        let builder = PipelineBuilder::new(&self.config, &self.capabilities);
        let built = builder.build()?;

        let frame_tx = self
            .frame_tx
            .clone()
            .ok_or_else(|| AiEngineError::FrameError("frame sender already consumed".into()))?;

        // Create the platform-specific buffer extractor.
        let buffer_extractor = extractor::create_extractor(&self.capabilities);

        // Reset watchdog coordination state for a fresh start.
        self.watchdog_frame_count.store(0, Ordering::Relaxed);
        self.watchdog_cancel = CancellationToken::new();
        let (liveness_tx, liveness_rx) = watch::channel(SourceLiveness::Healthy);
        self.liveness_tx = liveness_tx;
        self.liveness_rx = liveness_rx;
        let watchdog_frame_count = Arc::clone(&self.watchdog_frame_count);
        let liveness_tx_for_sample = self.liveness_tx.clone();

        built.appsink.set_callbacks(
            gstreamer_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink
                        .pull_sample()
                        .map_err(|_| gstreamer::FlowError::Error)?;
                    // Bump watchdog liveness counter on every frame.
                    watchdog_frame_count.fetch_add(1, Ordering::Relaxed);
                    let _ = liveness_tx_for_sample.send_if_modified(|state| {
                        if *state == SourceLiveness::Stalled {
                            *state = SourceLiveness::Healthy;
                            true
                        } else {
                            false
                        }
                    });
                    match buffer_extractor.extract(&sample) {
                        Ok(frame) => {
                            if frame_tx.try_send(frame).is_err() {
                                tracing::trace!("frame channel full, dropping oldest frame");
                            }
                            Ok(gstreamer::FlowSuccess::Ok)
                        }
                        Err(e) => {
                            tracing::warn!("failed to extract frame from GstSample: {e}");
                            Ok(gstreamer::FlowSuccess::Ok)
                        }
                    }
                })
                .build(),
        );

        // Install GStreamer bus message handler for error/EOS/warning
        // detection. Errors and EOS are logged prominently — the watchdog
        // picks up the stall and triggers a restart.
        let bus = built
            .pipeline
            .bus()
            .ok_or_else(|| AiEngineError::FrameError("pipeline has no bus".into()))?;
        let uri_for_bus = self.config.uri.clone();
        let bus_watch_guard = bus
            .add_watch(move |_bus, msg| {
                use gstreamer::MessageView;
                match msg.view() {
                    MessageView::Error(err) => {
                        let src = err.src().map(|s| s.name().to_string()).unwrap_or_default();
                        tracing::error!(
                            source = %src,
                            error = %err.error(),
                            debug = ?err.debug(),
                            uri = %uri_for_bus,
                            "GStreamer pipeline error"
                        );
                    }
                    MessageView::Eos(_) => {
                        tracing::warn!(uri = %uri_for_bus, "GStreamer pipeline reached EOS");
                    }
                    MessageView::Warning(warn) => {
                        let src = warn.src().map(|s| s.name().to_string()).unwrap_or_default();
                        tracing::warn!(
                            source = %src,
                            warning = %warn.error(),
                            uri = %uri_for_bus,
                            "GStreamer pipeline warning"
                        );
                    }
                    MessageView::StateChanged(sc) => {
                        if sc
                            .src()
                            .map(|s| s.name().as_str() == "pipeline")
                            .unwrap_or(false)
                        {
                            tracing::debug!(
                                old = ?sc.old(),
                                new = ?sc.current(),
                                "pipeline state changed"
                            );
                        }
                    }
                    _ => {}
                }
                gstreamer::glib::ControlFlow::Continue
            })
            .map_err(|_| AiEngineError::FrameError("failed to add bus watch".into()))?;

        built
            .pipeline
            .set_state(gstreamer::State::Playing)
            .map_err(|e| {
                AiEngineError::FrameError(format!("failed to set pipeline to Playing: {e}"))
            })?;

        self.pipeline = Some(built.pipeline);
        self._bus_watch_guard = Some(bus_watch_guard);
        self.running = true;

        // Launch the watchdog task that monitors frame liveness and
        // triggers a warning when no frames are received for too long.
        let watchdog_frame_count = Arc::clone(&self.watchdog_frame_count);
        let watchdog_cancel = self.watchdog_cancel.clone();
        let liveness_tx_for_task = self.liveness_tx.clone();
        let uri_for_wd = self.config.uri.clone();
        let watchdog_timeout = std::time::Duration::from_secs(10);
        self.watchdog_handle = Some(tokio::spawn(async move {
            let mut last_count = 0u64;
            let mut stall_rounds = 0u32;
            loop {
                tokio::select! {
                    _ = watchdog_cancel.cancelled() => break,
                    _ = tokio::time::sleep(watchdog_timeout) => {}
                }

                let current = watchdog_frame_count.load(Ordering::Relaxed);
                if current == last_count {
                    stall_rounds += 1;
                    tracing::warn!(
                        uri = %uri_for_wd,
                        stall_rounds,
                        last_frame_count = current,
                        timeout_secs = watchdog_timeout.as_secs(),
                        "GStreamer pipeline watchdog: no new frames detected"
                    );
                    // After 3 consecutive stall rounds (30s default), emit
                    // a critical warning. The engine's channel monitor should
                    // pick this up and trigger a restart via
                    // `GstFrameSource::restart()`.
                    if stall_rounds >= 3 {
                        let _ = liveness_tx_for_task.send_if_modified(|state| {
                            if *state != SourceLiveness::Stalled {
                                *state = SourceLiveness::Stalled;
                                true
                            } else {
                                false
                            }
                        });
                        tracing::error!(
                            uri = %uri_for_wd,
                            "GStreamer pipeline stalled for {}s — restart recommended",
                            watchdog_timeout.as_secs() * u64::from(stall_rounds)
                        );
                    }
                } else {
                    if stall_rounds > 0 {
                        tracing::info!(
                            uri = %uri_for_wd,
                            "GStreamer pipeline watchdog: frames resumed after {stall_rounds} stall rounds"
                        );
                    }
                    let _ = liveness_tx_for_task.send_if_modified(|state| {
                        if *state != SourceLiveness::Healthy {
                            *state = SourceLiveness::Healthy;
                            true
                        } else {
                            false
                        }
                    });
                    stall_rounds = 0;
                    last_count = current;
                }
            }
            tracing::debug!(uri = %uri_for_wd, "watchdog task exiting");
        }));

        tracing::info!(uri = %self.config.uri, "GStreamer pipeline started with watchdog");
        Ok(())
    }

    async fn next_event(&mut self) -> Result<SourceEvent, AiEngineError> {
        let watchdog_cancel = self.watchdog_cancel.clone();
        let mut liveness_rx = self.liveness_rx.clone();

        loop {
            if *liveness_rx.borrow_and_update() == SourceLiveness::Stalled {
                tracing::warn!(
                    uri = %self.config.uri,
                    "watchdog marked pipeline stalled, emitting SourceEvent::Stalled"
                );
                return Ok(SourceEvent::Stalled);
            }

            tokio::select! {
                _ = watchdog_cancel.cancelled() => return Ok(SourceEvent::EndOfStream),
                changed = liveness_rx.changed() => {
                    if changed.is_err() {
                        return Ok(SourceEvent::EndOfStream);
                    }
                }
                frame = self.frame_rx.recv() => {
                    return Ok(match frame {
                        Some(frame) => SourceEvent::Frame(frame),
                        None => SourceEvent::EndOfStream,
                    });
                }
            }
        }
    }

    async fn stop(&mut self) -> Result<(), AiEngineError> {
        // Signal watchdog to exit before tearing down the pipeline.
        self.watchdog_cancel.cancel();
        if let Some(handle) = self.watchdog_handle.take() {
            let _ = handle.await;
        }

        if let Some(ref pipeline) = self.pipeline {
            // Setting state to Null automatically tears down the bus watch
            // callback — no need to manually remove it.
            pipeline.set_state(gstreamer::State::Null).map_err(|e| {
                AiEngineError::FrameError(format!("failed to set pipeline to Null: {e}"))
            })?;
            tracing::info!(uri = %self.config.uri, "GStreamer pipeline stopped");
        }
        // Drop the bus watch guard so the callback is cleaned up.
        self._bus_watch_guard = None;
        self.pipeline = None;
        self.running = false;
        Ok(())
    }

    #[inline]
    fn is_running(&self) -> bool {
        self.running
    }
}

impl GstFrameSource {
    /// Update the target resolution for adaptive degradation.
    ///
    /// When inference load is too high or memory pressure is detected,
    /// the engine can call this to downscale the pipeline output, reducing
    /// per-frame preprocessing cost. A `None` value reverts to native
    /// resolution.
    ///
    /// Applies on next `restart()` — does not affect the running pipeline.
    pub fn set_target_resolution(&mut self, resolution: Option<(u32, u32)>) {
        self.config.target_resolution = resolution;
    }

    /// Dynamically reconfigure the frame rate limit on a running pipeline.
    ///
    /// Modifies the `rate_caps` capsfilter's caps property in-place, which
    /// causes GStreamer to renegotiate the framerate without tearing down
    /// the pipeline. This is the preferred runtime tuning path — faster
    /// than `restart()` and preserves decoder state / DMA buffer pools.
    ///
    /// If `fps` is `None`, the rate cap is removed (unlimited).
    /// No-op if the pipeline is not running or has no `framerate_limiter`
    /// element (i.e. was started without `target_fps`).
    pub fn set_fps_live(&self, fps: Option<f32>) {
        let Some(ref pipeline) = self.pipeline else {
            return;
        };

        let Some(rate_caps_el) = pipeline.by_name("rate_caps") else {
            // Pipeline was built without videorate — cannot adjust dynamically.
            // The caller should use restart() with an updated config instead.
            tracing::debug!("no rate_caps element in pipeline, fps change requires restart");
            return;
        };

        let new_caps = match fps {
            Some(f) => {
                let fps_int = (f.ceil() as i32).max(1);
                gstreamer::Caps::builder("video/x-raw")
                    .field("framerate", gstreamer::Fraction::new(fps_int, 1))
                    .build()
            }
            None => gstreamer::Caps::new_any(),
        };

        rate_caps_el.set_property("caps", &new_caps);
        tracing::info!(fps = ?fps, "pipeline framerate dynamically reconfigured");
    }

    /// Dynamically reconfigure the output resolution on a running pipeline.
    ///
    /// Works by updating the capsfilter after the platform CSC/resize
    /// element. This forces GStreamer to renegotiate the resolution
    /// downstream without rebuilding the pipeline.
    ///
    /// Supported capsfilter names (platform-dependent):
    /// - `rga_caps` (Rockchip — mppvideodec built-in RGA)
    /// - `nv_caps` (Jetson nvvidconv)
    /// - `sw_caps` (generic videoconvert + videoscale)
    ///
    /// Returns `true` if the reconfiguration was applied, `false` if
    /// the pipeline has no matching capsfilter (requires restart).
    pub fn set_resolution_live(&self, width: u32, height: u32) -> bool {
        let Some(ref pipeline) = self.pipeline else {
            return false;
        };

        // Try platform-specific capsfilters in priority order.
        let capsfilter_names = ["rga_caps", "nv_caps", "sw_caps"];
        for name in &capsfilter_names {
            if let Some(el) = pipeline.by_name(name) {
                let new_caps = gstreamer::Caps::builder("video/x-raw")
                    .field("width", width as i32)
                    .field("height", height as i32)
                    .build();
                el.set_property("caps", &new_caps);
                tracing::info!(
                    name,
                    width,
                    height,
                    "pipeline resolution dynamically reconfigured"
                );
                return true;
            }
        }

        tracing::debug!("no supported capsfilter for dynamic resolution change");
        false
    }

    /// Restart the pipeline with an optional new configuration.
    ///
    /// Stops the current pipeline, applies the new config (if any), and
    /// starts a fresh pipeline. This is the primary mechanism for handling
    /// pipeline hot-updates when stream parameters change at runtime
    /// (e.g. resolution change, URI change, transport switch).
    ///
    /// If `new_config` is `None`, the pipeline restarts with the same config.
    pub async fn restart(
        &mut self,
        new_config: Option<GstFrameSourceConfig>,
    ) -> Result<(), AiEngineError> {
        tracing::info!(uri = %self.config.uri, "restarting GStreamer pipeline");
        self.stop().await?;

        if let Some(cfg) = new_config {
            self.config = cfg;
        }

        // Rebuild the channel to discard stale frames from the old pipeline.
        let (tx, rx) = mpsc::channel(self.config.max_buffers as usize + 1);
        self.frame_rx = rx;
        self.frame_tx = Some(tx);

        self.start().await
    }
}

impl Drop for GstFrameSource {
    fn drop(&mut self) {
        self.watchdog_cancel.cancel();
        if let Some(handle) = self.watchdog_handle.take() {
            handle.abort();
        }
        if let Some(ref pipeline) = self.pipeline {
            let _ = pipeline.set_state(gstreamer::State::Null);
        }
    }
}
