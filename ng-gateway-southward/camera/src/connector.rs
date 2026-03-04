//! Camera supervision connector.
//!
//! Implements the SDK [`Connector`] trait following the same pattern as
//! `ModbusConnector`:
//! - `new(ctx)`: parse config, capture AI engine handle (no I/O)
//! - `connect(ctx)`: establish RTSP/ONVIF/MJPEG video stream session

use crate::{
    handle::CameraHandle,
    protocol::{mjpeg::MjpegStream, onvif::OnvifClient, rtsp::RtspStream, VideoStream},
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

/// Camera connector — establishes video stream sessions.
///
/// Follows the same Connector pattern as `ModbusConnector`:
/// - `new(ctx)`: parse config, extract AI engine handle (sync, no I/O)
/// - `connect(ctx)`: establish RTSP/ONVIF/MJPEG session (async, may fail)
pub struct CameraConnector {
    /// Parsed camera channel configuration.
    channel: Arc<CameraChannel>,
    /// AI Engine API handle (injected from host process via extensions).
    /// Retained for ONVIF protocol discovery (Phase 2) and pipeline lifecycle.
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
        // 1. Extract AI engine handle (must be done before downcast_arc consumes ctx fields)
        let ai_engine = ctx.extensions.get_cloned::<Arc<dyn AiEngineApi>>().ok_or(
            DriverError::ConfigurationError(
                "AI engine not available — ensure [general.ai] is enabled".to_string(),
            ),
        )?;

        // 2. Downcast runtime channel to CameraChannel
        let channel = ctx
            .runtime_channel
            .downcast_arc::<CameraChannel>()
            .map_err(|_| {
                DriverError::ConfigurationError("Invalid CameraChannel type".to_string())
            })?;

        // 3. Verify pipeline binding for this channel.
        //
        // Pipeline bindings are established at the API level when creating or
        // updating a channel. By the time the connector is instantiated, the
        // binding should already be active in the PipelineRegistry.
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

        // 4. Create shared handle
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

    /// Camera is I/O bound (video stream); single concurrent session per channel.
    #[inline]
    fn collector_concurrency_profile_hint(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    /// Establish a video stream session based on the configured protocol.
    async fn connect(
        &self,
        ctx: SessionContext,
    ) -> Result<Self::Session, <Self::Session as Session>::Error> {
        let stream: Box<dyn VideoStream> = match &self.channel.config.protocol {
            CameraProtocol::Rtsp { url, transport } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    url = %url.host_str().unwrap_or("unknown"),
                    transport = ?transport,
                    "Connecting to RTSP camera"
                );
                let rtsp = RtspStream::connect(url, *transport, &ctx.cancel).await?;
                Box::new(rtsp)
            }
            CameraProtocol::Onvif {
                endpoint,
                profile,
                username,
                password,
                transport,
            } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    endpoint = %endpoint.host_str().unwrap_or("unknown"),
                    "Connecting to ONVIF camera"
                );

                let timeout = std::time::Duration::from_millis(
                    self.channel.connection_policy.connect_timeout_ms,
                );

                // 1. Establish ONVIF client and discover services
                let onvif_client = OnvifClient::connect(
                    endpoint,
                    username.as_deref(),
                    password.as_deref(),
                    timeout,
                )
                .await?;

                // 2. Get media profiles
                let profiles = onvif_client.get_profiles().await?;

                // 3. Select the requested profile or auto-select first
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

                // 4. Get the RTSP stream URI
                let stream_uri = onvif_client.get_stream_uri(&selected_profile.token).await?;

                tracing::info!(
                    rtsp_url = %stream_uri.uri,
                    "ONVIF stream URI resolved"
                );

                // 5. Inject credentials into the RTSP URL if needed
                let mut rtsp_url = url::Url::parse(&stream_uri.uri).map_err(|e| {
                    DriverError::SessionError(format!("Invalid RTSP URL from ONVIF: {e}"))
                })?;
                if let (Some(u), Some(p)) = (username.as_deref(), password.as_deref()) {
                    let _ = rtsp_url.set_username(u);
                    let _ = rtsp_url.set_password(Some(p));
                }

                // 6. Connect via RTSP using the discovered URL
                let rtsp = RtspStream::connect(&rtsp_url, *transport, &ctx.cancel).await?;

                // 7. Set up PTZ controller if PTZ service is available
                let onvif_arc = Arc::new(onvif_client);
                if onvif_arc.ptz_url().is_some() {
                    let ptz =
                        PtzController::new(Arc::clone(&onvif_arc), selected_profile.token.clone());
                    self.handle.set_ptz_controller(ptz);
                    tracing::info!("ONVIF PTZ controller initialized");
                }

                Box::new(rtsp)
            }
            CameraProtocol::Mjpeg { url } => {
                tracing::info!(
                    channel_id = self.channel.id,
                    url = %url.host_str().unwrap_or("unknown"),
                    "Connecting to MJPEG camera"
                );
                let mjpeg = MjpegStream::connect(url, &ctx.cancel).await?;
                Box::new(mjpeg)
            }
        };

        Ok(CameraSession::new(
            Arc::clone(&self.handle),
            stream,
            ctx.cancel.clone(),
        ))
    }

    /// Classify errors to determine retry vs. fatal behavior.
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
