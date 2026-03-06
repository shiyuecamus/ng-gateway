//! WebRTC signaling types shared between the AI engine and web layer.
//!
//! These are pure data types (no GStreamer dependency) used for WebSocket
//! signaling between the browser and the gateway.

use serde::{Deserialize, Serialize};

/// Signaling message exchanged between client and server over WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebRtcSignaling {
    /// Client SDP offer.
    Offer {
        sdp: String,
        #[serde(default)]
        config: Option<WebRtcClientConfig>,
    },
    /// Server SDP answer.
    Answer { sdp: String },
    /// ICE candidate (bidirectional).
    Ice {
        candidate: String,
        #[serde(default)]
        sdp_mid: Option<String>,
        #[serde(default)]
        sdp_m_line_index: Option<u32>,
    },
    /// Connection established confirmation (server → client).
    Connected {
        channel_id: i32,
        video_codec: String,
        resolution: [u32; 2],
        fps: u32,
        hw_encoder: Option<String>,
    },
    /// Error (server → client).
    Error { message: String },
}

/// Client capability negotiation sent with the SDP offer.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebRtcClientConfig {
    /// Preferred video codec (e.g., "h264", "vp8").
    #[serde(default)]
    pub preferred_codec: Option<String>,
    /// Maximum resolution [width, height].
    #[serde(default)]
    pub max_resolution: Option<[u32; 2]>,
    /// Maximum frame rate.
    #[serde(default)]
    pub max_fps: Option<u32>,
    /// Whether to draw annotation overlays server-side (burned into video).
    #[serde(default)]
    pub server_side_annotation: Option<bool>,
}
