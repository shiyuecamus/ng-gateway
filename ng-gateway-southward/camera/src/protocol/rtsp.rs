//! RTSP video stream implementation using the `retina` crate.
//!
//! Provides [`RtspStream`] which connects to an RTSP server, negotiates
//! the video stream, and yields raw H.264/H.265 NAL units.

use super::{RawFrame, VideoStream};
use crate::types::RtspTransport;
use ng_gateway_ai::api::FrameFormat;
use ng_gateway_sdk::DriverError;
use retina::{
    client::{
        Credentials, Demuxed, PlayOptions, Session, SessionGroup, SessionOptions, SetupOptions,
        TcpTransportOptions, Transport, UdpTransportOptions,
    },
    codec::CodecItem,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// RTSP video stream backed by `retina`.
///
/// Wraps a demuxed `retina` session and presents a simple `next_frame()` API.
/// The stream is always single-consumer (one camera channel = one stream).
pub struct RtspStream {
    /// Demuxed video+audio session (we only consume video).
    session: Demuxed,
}

impl RtspStream {
    /// Establish an RTSP session and start playback.
    ///
    /// # Protocol steps
    /// 1. DESCRIBE — discover available media streams
    /// 2. SETUP — negotiate transport (TCP interleaved or UDP)
    /// 3. PLAY — start receiving RTP packets
    ///
    /// The cancellation token allows aborting the connection attempt if the
    /// supervision loop requests shutdown during a slow RTSP handshake.
    pub async fn connect(
        url: &url::Url,
        transport: RtspTransport,
        cancel: &CancellationToken,
    ) -> Result<Self, DriverError> {
        let session_group = Arc::new(SessionGroup::default());
        let creds = extract_credentials(url);
        let sanitized_url = sanitize_url(url);

        let mut session_opts = SessionOptions::default().session_group(session_group);

        if let Some((user, pass)) = creds {
            session_opts = session_opts.creds(Some(Credentials {
                username: user,
                password: pass,
            }));
        }

        let mut session = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            result = Session::describe(sanitized_url, session_opts) => {
                result.map_err(|e| DriverError::SessionError(
                    format!("RTSP DESCRIBE failed: {e}")
                ))?
            }
        };

        let video_stream_idx = session
            .streams()
            .iter()
            .position(|s| s.media() == "video")
            .ok_or(DriverError::SessionError(
                "No video stream found in RTSP session".into(),
            ))?;

        let setup_opts =
            match transport {
                RtspTransport::Tcp => SetupOptions::default()
                    .transport(Transport::Tcp(TcpTransportOptions::default())),
                RtspTransport::Udp => SetupOptions::default()
                    .transport(Transport::Udp(UdpTransportOptions::default())),
            };

        tokio::select! {
            _ = cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            result = session.setup(video_stream_idx, setup_opts) => {
                result.map_err(|e| DriverError::SessionError(
                    format!("RTSP SETUP failed: {e}")
                ))?;
            }
        }

        let playing = tokio::select! {
            _ = cancel.cancelled() => {
                return Err(DriverError::ServiceUnavailable);
            }
            result = session.play(PlayOptions::default()) => {
                result.map_err(|e| DriverError::SessionError(
                    format!("RTSP PLAY failed: {e}")
                ))?
            }
        };

        let demuxed = playing
            .demuxed()
            .map_err(|e| DriverError::SessionError(format!("RTSP demux failed: {e}")))?;

        tracing::info!(
            url = %sanitize_url_for_log(url),
            transport = ?transport,
            "RTSP session established"
        );

        Ok(Self { session: demuxed })
    }
}

#[async_trait::async_trait]
impl VideoStream for RtspStream {
    async fn next_frame(&mut self) -> Result<RawFrame, DriverError> {
        use futures_util::StreamExt;
        loop {
            match self.session.next().await {
                Some(Ok(CodecItem::VideoFrame(frame))) => {
                    let is_key = frame.is_random_access_point();

                    return Ok(RawFrame {
                        data: bytes::Bytes::from(frame.into_data()),
                        format: FrameFormat::H264Nal,
                        width: 0,
                        height: 0,
                        is_key,
                    });
                }
                Some(Ok(_)) => continue, // Skip audio / RTCP / other
                Some(Err(e)) => {
                    return Err(DriverError::SessionError(format!("RTSP stream error: {e}")));
                }
                None => {
                    return Err(DriverError::SessionError("RTSP stream ended".into()));
                }
            }
        }
    }
}

/// Extract username:password from the URL's userinfo component.
fn extract_credentials(url: &url::Url) -> Option<(String, String)> {
    let user = url.username();
    if user.is_empty() {
        return None;
    }
    let pass = url.password().unwrap_or("");
    Some((percent_decode(user), percent_decode(pass)))
}

/// Percent-decode a URL component.
fn percent_decode(input: &str) -> String {
    percent_encoding::percent_decode_str(input)
        .decode_utf8_lossy()
        .into_owned()
}

/// Remove credentials from the URL for use as the RTSP target
/// (retina takes creds separately).
fn sanitize_url(url: &url::Url) -> url::Url {
    let mut clean = url.clone();
    let _ = clean.set_username("");
    let _ = clean.set_password(None);
    clean
}

/// Sanitize URL for logging (mask password).
fn sanitize_url_for_log(url: &url::Url) -> String {
    let mut masked = url.clone();
    if masked.password().is_some() {
        let _ = masked.set_password(Some("***"));
    }
    masked.to_string()
}
