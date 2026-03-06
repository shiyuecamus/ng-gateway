//! Camera supervision connector.
//!
//! Implements the SDK [`Connector`] trait:
//! - `new(ctx)`: parse config, capture AI engine handle (no I/O)
//! - `connect(ctx)`: resolve the stream URL (ONVIF discovery if needed)
//!   and create a session that registers the channel with the AI engine
//!
//! The AI engine's internal GStreamer pipeline handles the actual RTSP
//! stream connection, hardware-accelerated decoding, and zero-copy inference.

use crate::{
    handle::CameraHandle,
    protocol::onvif::OnvifClient,
    ptz::PtzController,
    session::CameraSession,
    types::{CameraChannel, CameraProtocol},
};
use ng_gateway_ai::api::AiEngineApi;
use ng_gateway_sdk::{
    supervision::{Connector, Session, SessionContext},
    CollectorConcurrencyProfile, DriverError, FailureKind, FailurePhase, SouthwardInitContext,
};
use std::sync::Arc;

/// Camera connector — resolves stream URLs and creates sessions.
///
/// The connector's `connect()` resolves the stream URL (which may require
/// ONVIF discovery), then creates a [`CameraSession`] that registers the
/// URL with the AI engine's internal GStreamer pipeline.
pub struct CameraConnector {
    /// Parsed camera channel configuration.
    channel: Arc<CameraChannel>,
    /// AI Engine API handle (retained for pipeline lifecycle queries).
    #[allow(dead_code)]
    ai_engine: Arc<dyn AiEngineApi>,
    /// Shared data-plane handle.
    handle: Arc<CameraHandle>,
}

#[async_trait::async_trait]
impl Connector for CameraConnector {
    type InitContext = SouthwardInitContext;
    type Handle = CameraHandle;
    type Session = CameraSession;

    fn new(ctx: Self::InitContext) -> Result<Self, <Self::Session as Session>::Error>
    where
        Self: Sized,
    {
        let ai_engine = ctx.extensions.get_cloned::<Arc<dyn AiEngineApi>>().ok_or(
            DriverError::ConfigurationError(
                "AI engine not available — ensure [general.ai] is enabled".to_string(),
            ),
        )?;

        let channel = ctx
            .runtime_channel
            .downcast_arc::<CameraChannel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid CameraChannel type".to_string())
            })?;

        if ai_engine
            .pipelines()
            .get_channel_pipeline(channel.id)
            .is_none()
        {
            return Err(DriverError::ConfigurationError(format!(
                "No AI pipeline bound to channel {} — bind pipeline {} via API first",
                channel.id, channel.config.pipeline_id
            )));
        }

        let handle = Arc::new(CameraHandle::new(
            Arc::clone(&channel),
            Arc::clone(&ai_engine),
        ));

        Ok(Self {
            channel,
            ai_engine,
            handle,
        })
    }

    #[inline]
    fn collector_concurrency_profile_hint(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    /// Resolve the stream URL and create a camera session.
    ///
    /// For RTSP: the URL is used directly.
    /// For ONVIF: discovers the RTSP URL via ONVIF protocol, sets up PTZ.
    /// For MJPEG: the HTTP URL is used directly.
    ///
    /// The actual stream connection is handled by the AI engine's internal
    /// GStreamer pipeline — this method only resolves the URL.
    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let stream_url: String = match &self.channel.config.protocol {
            CameraProtocol::Rtsp { url, transport: _ } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    url = %url.host_str().unwrap_or("unknown"),
                    "Resolved RTSP camera URL"
                );
                url.to_string()
            }

            CameraProtocol::Onvif {
                endpoint,
                profile,
                username,
                password,
                transport: _,
            } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    endpoint = %endpoint.host_str().unwrap_or("unknown"),
                    "Discovering ONVIF camera stream"
                );

                let timeout = std::time::Duration::from_millis(
                    self.channel.connection_policy.connect_timeout_ms,
                );

                let onvif_client = OnvifClient::connect(
                    endpoint,
                    username.as_deref(),
                    password.as_deref(),
                    timeout,
                )
                .await?;

                let profiles = onvif_client.get_profiles().await?;

                let selected_profile = if profile.is_empty() {
                    profiles.first().ok_or(DriverError::SessionError(
                        "No ONVIF media profiles available".into(),
                    ))?
                } else {
                    profiles.iter().find(|p| p.token == *profile).ok_or(
                        DriverError::ConfigurationError(format!(
                            "ONVIF profile '{profile}' not found. Available: {}",
                            profiles
                                .iter()
                                .map(|p| p.token.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )),
                    )?
                };

                tracing::info!(
                    profile_token = %selected_profile.token,
                    profile_name = %selected_profile.name,
                    "Selected ONVIF media profile"
                );

                let stream_uri = onvif_client.get_stream_uri(&selected_profile.token).await?;

                tracing::info!(rtsp_url = %stream_uri.uri, "ONVIF stream URI resolved");

                let mut rtsp_url = url::Url::parse(&stream_uri.uri).map_err(|e| {
                    DriverError::SessionError(format!("Invalid RTSP URL from ONVIF: {e}"))
                })?;
                if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    let _ = rtsp_url.set_username(u);
                    let _ = rtsp_url.set_password(Some(p));
                }

                // Set up PTZ controller if available
                let onvif_arc = Arc::new(onvif_client);
                if onvif_arc.ptz_url().is_some() {
                    let ptz =
                        PtzController::new(Arc::clone(&onvif_arc), selected_profile.token.clone());
                    self.handle.set_ptz_controller(ptz);
                    tracing::info!("ONVIF PTZ controller initialized");
                }

                rtsp_url.to_string()
            }

            CameraProtocol::Mjpeg { url } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    url = %url.host_str().unwrap_or("unknown"),
                    "Resolved MJPEG camera URL"
                );
                url.to_string()
            }
        };

        Ok(CameraSession::new(
            Arc::clone(&self.handle),
            stream_url,
            ctx.cancel.clone(),
        ))
    }

    fn classify_error(
        &self,
        _phase: FailurePhase,
        err: &<Self::Session as Session>::Error,
    ) -> FailureKind {
        match err {
            DriverError::ConfigurationError(_) => FailureKind::Fatal,
            _ => FailureKind::Retryable,
        }
    }
}
