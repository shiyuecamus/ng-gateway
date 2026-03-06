//! Video frame source abstraction and GStreamer backend.
//!
//! This module defines the [`FrameSource`] trait — a uniform async interface
//! for pulling decoded video frames from any backend — and provides the
//! production [`GstFrameSource`] implementation backed by GStreamer.
//!
//! # Trait design
//!
//! The trait is intentionally `async` and object-safe (via `async_trait`),
//! enabling:
//! - **Testing**: inject a mock / file-based source without GStreamer.
//! - **Hot-swap**: replace a running source (e.g. on stream URL change).
//! - **Composition**: wrap a source with rate-limiting, metrics, etc.
//!
//! # Pipeline architecture
//!
//! Pipelines are built programmatically via [`pipeline::PipelineBuilder`]:
//!
//! ```text
//! source → decodebin3 → [platform CSC/resize] → [videorate] → appsink
//! ```
//!
//! `decodebin3` provides automatic codec detection (H.264, H.265, VP9, AV1,
//! MJPEG) and hardware decoder selection via GStreamer's element ranking.
//!
//! # Buffer extraction
//!
//! Platform-specialized [`extractor::BufferExtractor`] implementations bridge
//! GStreamer's buffer model to our [`FrameMemory`](super::memory::FrameMemory):
//!
//! - **RK3588**: DMA-buf fd extraction → `FrameMemory::DmaBuf`
//! - **Jetson**: NVMM → CUDA device ptr (with `cuda` feature)
//! - **VAAPI**: DMA-buf fd extraction → `FrameMemory::DmaBuf`
//! - **Generic**: GstBuffer map → `FrameMemory::GstBufferRef` (zero-copy)

pub(crate) mod codec;
pub(crate) mod extractor;
mod gst_source;
pub(crate) mod pipeline;

#[cfg(all(feature = "cuda", target_os = "linux"))]
pub(crate) mod jetson_ffi;

use crate::DecodedFrame;
use ng_gateway_error::ai::AiEngineError;

pub use codec::VideoCodec;
pub use extractor::{create_extractor, BufferExtractor};
pub use gst_source::{GstFrameSource, GstFrameSourceConfig, RtspTransport};

/// Asynchronous video frame source.
///
/// Implementors produce [`DecodedFrame`]s from an underlying stream.
/// The lifecycle is: `new` → [`start`](Self::start) → repeated
/// [`next_frame`](Self::next_frame) → [`stop`](Self::stop).
///
/// # Contract
///
/// - `start()` must be called before `next_frame()`.
/// - `next_frame()` returns `Ok(None)` on end-of-stream (EOS).
/// - `stop()` is idempotent; calling it on an already-stopped source is a no-op.
/// - Dropping a started source must release all resources (pipeline, fd, etc.).
#[async_trait::async_trait]
pub trait FrameSource: Send + 'static {
    /// Start the underlying media pipeline.
    async fn start(&mut self) -> Result<(), AiEngineError>;

    /// Pull the next decoded frame.
    ///
    /// Returns `Ok(None)` when the stream has ended (EOS) or after `stop()`.
    async fn next_frame(&mut self) -> Result<Option<DecodedFrame>, AiEngineError>;

    /// Stop the pipeline and release all resources.
    ///
    /// Idempotent — calling on an already-stopped source is a no-op.
    async fn stop(&mut self) -> Result<(), AiEngineError>;

    /// Whether the source is currently producing frames.
    fn is_running(&self) -> bool;
}
