//! Camera supervised session implementation.
//!
//! The camera session registers the channel with the AI engine's internal
//! GStreamer pipeline for hardware-accelerated frame acquisition and
//! zero-copy inference. It monitors for stream errors or cancellation.

use crate::handle::CameraHandle;
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Camera session — manages the lifecycle of a single camera channel.
///
/// The session registers the channel's stream URL with the AI engine,
/// which creates a GStreamer pipeline internally. The session then
/// monitors for stream errors or cancellation.
pub struct CameraSession {
    /// Shared data-plane handle.
    handle: Arc<CameraHandle>,
    /// Resolved stream URL (RTSP / HTTP / V4L2).
    stream_url: String,
    /// Cancellation token for cooperative shutdown.
    cancel: CancellationToken,
}

impl CameraSession {
    /// Create a new camera session from a resolved stream URL.
    pub fn new(handle: Arc<CameraHandle>, stream_url: String, cancel: CancellationToken) -> Self {
        Self {
            handle,
            stream_url,
            cancel,
        }
    }
}

#[async_trait::async_trait]
impl Session for CameraSession {
    type Handle = CameraHandle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    /// Post-connect initialization:
    /// 1. Set the reconnect handle for the data-plane
    /// 2. Register the channel with the AI engine for continuous analysis
    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle.set_reconnect(ctx.reconnect.clone());
        self.handle.register_stream(self.stream_url.clone()).await?;
        tracing::info!("Camera channel registered with AI engine");
        Ok(())
    }

    /// Drive the session until disconnect, error, or cancellation.
    ///
    /// The AI engine runs the GStreamer frame loop internally.
    /// This session monitors for:
    /// - Cancellation (shutdown requested) → graceful unregister
    /// - Stream error (GStreamer pipeline failure) → request reconnection
    async fn run(self, _ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        tokio::select! {
            biased;

            _ = self.cancel.cancelled() => {
                self.handle.unregister_stream().await;
                Ok(RunOutcome::Disconnected)
            }

            err = self.handle.wait_for_stream_error() => {
                tracing::warn!(error = %err, "Camera stream error, requesting reconnect");
                self.handle.unregister_stream().await;
                Ok(RunOutcome::ReconnectRequested(Arc::from(
                    format!("stream error: {err}")
                )))
            }
        }
    }
}
