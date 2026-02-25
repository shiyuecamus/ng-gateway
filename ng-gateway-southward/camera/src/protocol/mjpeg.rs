//! HTTP MJPEG stream implementation.
//!
//! Provides [`MjpegStream`] which connects to an HTTP MJPEG endpoint and
//! yields individual JPEG frames by parsing the multipart boundary protocol.
//!
//! # MJPEG Protocol
//!
//! MJPEG over HTTP uses `multipart/x-mixed-replace` content type:
//! ```text
//! HTTP/1.1 200 OK
//! Content-Type: multipart/x-mixed-replace; boundary=myboundary
//!
//! --myboundary
//! Content-Type: image/jpeg
//! Content-Length: 12345
//!
//! <JPEG bytes>
//! --myboundary
//! Content-Type: image/jpeg
//! ...
//! ```

use super::{RawFrame, VideoStream};
use bytes::{Bytes, BytesMut};
use ng_gateway_ai::api::FrameFormat;
use ng_gateway_sdk::DriverError;
use tokio_util::sync::CancellationToken;

/// JPEG frame start marker (SOI — Start of Image).
const JPEG_SOI: [u8; 2] = [0xFF, 0xD8];
/// JPEG frame end marker (EOI — End of Image).
const JPEG_EOI: [u8; 2] = [0xFF, 0xD9];

/// Maximum allowed frame size (16 MB) to prevent memory exhaustion
/// from malformed streams.
const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// HTTP MJPEG stream.
///
/// Connects to an HTTP endpoint that serves `multipart/x-mixed-replace`
/// content, extracts individual JPEG frames from the multipart stream.
pub struct MjpegStream {
    /// Streaming HTTP response body.
    response: reqwest::Response,
    /// Internal read buffer for accumulating partial frames.
    buffer: BytesMut,
    /// Frame sequence counter.
    frame_count: u64,
}

impl MjpegStream {
    /// Connect to an MJPEG HTTP stream.
    pub async fn connect(url: &url::Url, cancel: &CancellationToken) -> Result<Self, DriverError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .danger_accept_invalid_certs(true)
            .build()
            .map_err(|e| DriverError::SessionError(format!("Failed to create HTTP client: {e}")))?;

        let response = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            result = client.get(url.as_str()).send() => {
                result.map_err(|e| DriverError::SessionError(
                    format!("MJPEG HTTP connection failed: {e}")
                ))?
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Err(DriverError::SessionError(format!(
                "MJPEG HTTP error: {status}"
            )));
        }

        tracing::info!(url = %url, "MJPEG stream connected");

        Ok(Self {
            response,
            buffer: BytesMut::with_capacity(256 * 1024),
            frame_count: 0,
        })
    }

    /// Find a complete JPEG frame in the buffer.
    ///
    /// Scans for SOI (0xFFD8) and EOI (0xFFD9) markers to extract
    /// a valid JPEG frame from the accumulated buffer data.
    fn extract_jpeg_frame(&mut self) -> Option<Bytes> {
        let buf = &self.buffer[..];
        if buf.len() < 4 {
            return None;
        }

        // Find SOI marker
        let soi_pos = buf.windows(2).position(|w| w == JPEG_SOI)?;

        // Find EOI marker after SOI
        let search_start = soi_pos + 2;
        if search_start >= buf.len() {
            return None;
        }

        let eoi_pos = buf[search_start..]
            .windows(2)
            .position(|w| w == JPEG_EOI)
            .map(|p| search_start + p + 2)?;

        // Extract the complete JPEG frame (SOI to EOI inclusive)
        let frame_data = Bytes::copy_from_slice(&buf[soi_pos..eoi_pos]);

        // Advance the buffer past the extracted frame
        let _ = self.buffer.split_to(eoi_pos);

        Some(frame_data)
    }
}

#[async_trait::async_trait]
impl VideoStream for MjpegStream {
    async fn next_frame(&mut self) -> Result<RawFrame, DriverError> {
        loop {
            // Check if we already have a complete frame in the buffer
            if let Some(jpeg_data) = self.extract_jpeg_frame() {
                self.frame_count += 1;
                return Ok(RawFrame {
                    data: jpeg_data,
                    format: FrameFormat::Jpeg,
                    width: 0,
                    height: 0,
                    is_key: true, // Every MJPEG frame is an I-frame
                });
            }

            // Guard against buffer overflow
            if self.buffer.len() > MAX_FRAME_SIZE {
                tracing::warn!(
                    buffer_size = self.buffer.len(),
                    "MJPEG buffer exceeded max size, resetting"
                );
                self.buffer.clear();
            }

            // Read more data from the HTTP stream
            let chunk = self
                .response
                .chunk()
                .await
                .map_err(|e| DriverError::SessionError(format!("MJPEG stream read error: {e}")))?
                .ok_or(DriverError::SessionError("MJPEG stream ended".into()))?;

            self.buffer.extend_from_slice(&chunk);
        }
    }
}
