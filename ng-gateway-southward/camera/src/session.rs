//! Camera supervised session implementation.
//!
//! Unlike poll-based drivers (Modbus, S7, etc.) where the session simply waits
//! for cancellation, the camera session actively drives the frame acquisition
//! loop through [`CameraHandle::start_frame_loop`] and monitors for stream
//! errors that trigger reconnection via the supervision loop.

use crate::{handle::CameraHandle, protocol::VideoStream};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Camera session — manages the lifecycle of a single video stream connection.
///
/// The session owns a [`VideoStream`] and passes it to the handle's frame loop
/// during initialization. It then monitors for stream errors or cancellation.
pub struct CameraSession {
    /// Shared data-plane handle.
    handle: Arc<CameraHandle>,
    /// Transport-layer video stream (RTSP / ONVIF / MJPEG).
    stream: Option<Box<dyn VideoStream>>,
    /// Cancellation token for cooperative shutdown.
    cancel: CancellationToken,
}

impl CameraSession {
    /// Create a new camera session from a connected video stream.
    pub fn new(
        handle: Arc<CameraHandle>,
        stream: Box<dyn VideoStream>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            handle,
            stream: Some(stream),
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
    /// 2. Start the background frame acquisition loop
    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle.set_reconnect(ctx.reconnect.clone());

        let stream = self.stream.take().ok_or(DriverError::SessionError(
            "Video stream already consumed".into(),
        ))?;

        self.handle
            .start_frame_loop(stream, self.cancel.clone())
            .await?;

        tracing::info!("Camera frame loop started");
        Ok(())
    }

    /// Drive the session until disconnect, error, or cancellation.
    ///
    /// The frame loop runs in a spawned task. This session monitors for:
    /// - Cancellation (shutdown requested) → graceful disconnect
    /// - Stream error (transport failure) → request reconnection
    async fn run(self, _ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        tokio::select! {
            biased;

            _ = self.cancel.cancelled() => {
                self.handle.stop_frame_loop().await;
                Ok(RunOutcome::Disconnected)
            }

            err = self.handle.wait_for_stream_error() => {
                tracing::warn!(error = %err, "Camera stream error, requesting reconnect");
                self.handle.stop_frame_loop().await;
                Ok(RunOutcome::ReconnectRequested(Arc::from(
                    format!("stream error: {err}")
                )))
            }
        }
    }
}
