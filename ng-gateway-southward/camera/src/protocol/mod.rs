//! Camera protocol implementations.
//!
//! Provides transport-layer abstractions for different camera connection
//! protocols (RTSP, ONVIF, MJPEG). All protocols produce a unified
//! [`VideoStream`] that yields raw video frames.

pub mod mjpeg;
pub mod onvif;
pub mod rtsp;

use bytes::Bytes;
use ng_gateway_ai::api::FrameFormat;
use ng_gateway_sdk::DriverError;

/// A raw video frame received from the camera transport.
///
/// This is the transport-layer representation before any decoding or
/// preprocessing. The frame data may be encoded (H.264 NAL) or already
/// in a displayable format (JPEG, RGB).
#[derive(Debug, Clone)]
pub struct RawFrame {
    /// Raw frame bytes (ownership via `Bytes` for zero-copy sharing).
    pub data: Bytes,
    /// Frame encoding format.
    pub format: FrameFormat,
    /// Frame width in pixels (may be 0 if unknown before decoding).
    pub width: u32,
    /// Frame height in pixels (may be 0 if unknown before decoding).
    pub height: u32,
    /// Whether this is a key frame (I-frame / IDR for H.264/H.265).
    pub is_key: bool,
}

/// Unified video stream abstraction across all camera protocols.
///
/// Each protocol implementation (RTSP, ONVIF, MJPEG) converts its native
/// stream into this trait, providing a common `next_frame()` interface
/// for the camera session's frame loop.
#[async_trait::async_trait]
pub trait VideoStream: Send + 'static {
    /// Pull the next video frame from the stream.
    ///
    /// This method blocks (async) until a frame is available or the stream
    /// encounters an error. For H.264/H.265 streams, each call yields one
    /// NAL unit (or access unit). For MJPEG, each call yields one JPEG frame.
    ///
    /// # Errors
    /// Returns [`DriverError::SessionError`] on stream disconnection or
    /// protocol-level errors.
    async fn next_frame(&mut self) -> Result<RawFrame, DriverError>;
}
