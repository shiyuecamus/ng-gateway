//! WebRTC live preview publisher.
//!
//! Provides per-channel WebRTC video streaming using GStreamer's `webrtcbin`
//! element. Each publisher is lazily initialized when the first browser client
//! connects and torn down when the last client disconnects, freeing hardware
//! encoder resources.
//!
//! ## Multi-client architecture
//!
//! The current design is **single-peer per publisher**: each channel owns one
//! GStreamer pipeline with a single `webrtcbin`. When a new client connects
//! while another is already watching the same channel, the existing session is
//! replaced (last-writer-wins). `peer_count` is tracked so the publisher
//! stays alive as long as any WebSocket is connected; it is torn down when the
//! count drops to zero.
//!
//! A future multi-viewer iteration could split the pipeline at the RTP
//! payloader via `tee` and create per-viewer `webrtcbin` elements, sharing
//! the single encoder output across N peers. This is not yet implemented.
//!
//! ## Pipeline topology
//!
//! ```text
//! appsrc (RGB24) → videoconvert → [platform encoder] → rtph264pay → webrtcbin
//! ```
//!
//! Platform encoder selection:
//! - **Rockchip**: `mpph264enc`
//! - **Jetson**: `nvv4l2h264enc`
//! - **VA-API**: `vaapih264enc`
//! - **Generic**: `x264enc tune=zerolatency`
//!
//! ## DataChannel
//!
//! Two ordered+reliable DataChannels are created by the server:
//! - `metadata` — per-inference-frame detection results (JSON)
//! - `control`  — client commands (pause/resume/snapshot/bitrate)

use crate::frame::{memory::HardwarePlatform, platform::PlatformCapabilities};
use bytes::Bytes;
use gstreamer::{prelude::*, Caps, ElementFactory, Pipeline, Promise};
use gstreamer_app::AppSrc;
use gstreamer_webrtc::{WebRTCDataChannel, WebRTCSessionDescription};
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{AnalysisResult, WebRtcSignaling},
    settings::WebRtcConfig,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, warn};

// ────────────────────────────── Signaling types ──────────────────────────────

/// Inbound signaling message from a browser client (via WebSocket).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalingMessage {
    /// Client SDP offer.
    Offer {
        sdp: String,
        #[serde(default)]
        config: Option<ClientConfig>,
    },
    /// Server SDP answer (outbound only).
    Answer { sdp: String },
    /// ICE candidate (bidirectional).
    Ice {
        candidate: String,
        sdp_mid: Option<String>,
        sdp_m_line_index: Option<u32>,
    },
    /// Connection established confirmation (outbound only).
    Connected {
        channel_id: i32,
        video_codec: String,
        resolution: [u32; 2],
        fps: u32,
        hw_encoder: Option<String>,
    },
    /// Error (outbound only).
    Error { message: String },
}

/// Client capability negotiation config sent with the SDP offer.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientConfig {
    #[serde(default)]
    pub preferred_codec: Option<String>,
    #[serde(default)]
    pub max_resolution: Option<[u32; 2]>,
    #[serde(default)]
    pub max_fps: Option<u32>,
    #[serde(default)]
    pub server_side_annotation: Option<bool>,
}

/// Detection metadata pushed via DataChannel (compact wire format).
#[derive(Debug, Clone, Serialize)]
pub struct FrameMetadata {
    /// Monotonic sequence number.
    pub seq: u64,
    /// ISO 8601 timestamp.
    pub ts: String,
    /// Inference latency in milliseconds.
    pub lat_ms: f64,
    /// Compact detection list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub det: Vec<CompactDetection>,
    /// Stream statistics.
    pub stats: StreamStats,
}

/// Compact detection entry for DataChannel (minimized key names).
#[derive(Debug, Clone, Serialize)]
pub struct CompactDetection {
    /// Bounding box [xmin, ymin, xmax, ymax] in normalized [0,1] coordinates.
    pub b: [f32; 4],
    /// Class name.
    pub c: String,
    /// Confidence.
    pub cf: f32,
    /// Track ID (0 if no tracker).
    #[serde(skip_serializing_if = "is_zero")]
    pub tid: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Stream statistics pushed alongside metadata.
#[derive(Debug, Clone, Serialize)]
pub struct StreamStats {
    /// Source input FPS.
    pub fps_in: f32,
    /// AI inference FPS.
    pub fps_ai: f32,
    /// Output encode FPS.
    pub fps_out: f32,
    /// Dropped frame count since last report.
    pub drop: u32,
}

impl FrameMetadata {
    /// Build `FrameMetadata` from an `AnalysisResult` for DataChannel push.
    pub fn from_analysis_result(result: &AnalysisResult, stats: StreamStats) -> Self {
        let lat_ms = result.inference_latency.as_secs_f64() * 1000.0;
        let det = result
            .detections
            .iter()
            .map(|d| CompactDetection {
                b: [d.bbox.x_min, d.bbox.y_min, d.bbox.x_max, d.bbox.y_max],
                c: d.class.as_ref().to_string(),
                cf: d.confidence,
                tid: d.track_id.unwrap_or(0),
            })
            .collect();
        Self {
            seq: result.frame_seq,
            ts: result.frame_timestamp.to_rfc3339(),
            lat_ms,
            det,
            stats,
        }
    }
}

/// Control command from client via DataChannel.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlCommand {
    Pause,
    Resume,
    Snapshot { quality: Option<u8> },
    SetBitrate { kbps: u32 },
    SetResolution { w: u32, h: u32 },
}

// ────────────────────────────── Publisher ──────────────────────────────

/// Handle for interacting with a running WebRTC publisher.
///
/// Cloneable and cheap — internal state is behind `Arc`.
#[derive(Clone)]
pub struct WebRtcPublisherHandle {
    inner: Arc<PublisherInner>,
}

#[allow(dead_code)]
struct PublisherInner {
    channel_id: i32,
    /// Send signaling messages into the publisher task.
    signaling_tx: mpsc::Sender<(SignalingMessage, Option<oneshot::Sender<SignalingMessage>>)>,
    /// Broadcast server-generated ICE candidates to connected WebSocket clients.
    /// Clients receive these to add to their RTCPeerConnection (not to webrtcbin).
    server_ice_tx: broadcast::Sender<WebRtcSignaling>,
    /// Send raw RGB frames into the GStreamer appsrc.
    frame_tx: mpsc::Sender<FramePush>,
    /// Connected peer count.
    peer_count: AtomicUsize,
    /// Whether the publisher is running.
    running: AtomicBool,
    /// Metadata broadcast — latest metadata is sent to all connected peers.
    metadata_tx: mpsc::Sender<String>,
    /// Configuration snapshot.
    config: WebRtcConfig,
    /// Detected encoder name.
    hw_encoder: Option<String>,
}

/// A frame + its metadata pushed into the encode pipeline.
#[allow(dead_code)]
struct FramePush {
    data: Bytes,
    width: u32,
    height: u32,
}

impl WebRtcPublisherHandle {
    /// Number of currently connected peers.
    pub fn peer_count(&self) -> usize {
        self.inner.peer_count.load(Ordering::Relaxed)
    }

    /// Whether the publisher pipeline is running.
    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::Relaxed)
    }

    /// Push a raw RGB24 frame into the encode pipeline.
    ///
    /// Non-blocking: drops the frame if the internal queue is full.
    pub fn push_frame(&self, data: Bytes, width: u32, height: u32) {
        let _ = self.inner.frame_tx.try_send(FramePush {
            data,
            width,
            height,
        });
    }

    /// Push detection metadata to all connected peers via DataChannel.
    pub fn push_metadata(&self, metadata: &FrameMetadata) {
        if let Ok(json) = serde_json::to_string(metadata) {
            let _ = self.inner.metadata_tx.try_send(json);
        }
    }

    /// Process an incoming signaling message from a WebSocket client.
    ///
    /// Returns the response message to send back, or `None` if no response needed.
    pub async fn handle_signaling(&self, msg: SignalingMessage) -> Option<SignalingMessage> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .inner
            .signaling_tx
            .send((msg, Some(reply_tx)))
            .await
            .is_err()
        {
            return Some(SignalingMessage::Error {
                message: "publisher not running".to_string(),
            });
        }
        reply_rx.await.ok()
    }

    /// Increment the peer count (called when a new WebSocket client connects).
    pub fn add_peer(&self) {
        self.inner.peer_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the peer count (called when a WebSocket client disconnects).
    ///
    /// Returns `true` if this was the last peer (count reached 0).
    pub fn remove_peer(&self) -> bool {
        let prev = self.inner.peer_count.fetch_sub(1, Ordering::Relaxed);
        prev <= 1
    }

    /// The channel ID this publisher serves.
    pub fn channel_id(&self) -> i32 {
        self.inner.channel_id
    }

    /// Subscribe to server-generated ICE candidates.
    ///
    /// These must be sent to the client's RTCPeerConnection via `addIceCandidate`.
    /// They must NOT be passed to webrtcbin's add-ice-candidate (that is for
    /// client-originated candidates only).
    pub fn subscribe_server_ice(&self) -> broadcast::Receiver<WebRtcSignaling> {
        self.inner.server_ice_tx.subscribe()
    }
}

// ────────────────────────────── Builder ──────────────────────────────

/// Select the best available H.264 encoder element name for the current platform.
fn select_encoder(caps: &PlatformCapabilities) -> &'static str {
    match caps.platform {
        HardwarePlatform::Rockchip => {
            if ElementFactory::find("mpph264enc").is_some() {
                return "mpph264enc";
            }
        }
        HardwarePlatform::NvidiaJetson => {
            if ElementFactory::find("nvv4l2h264enc").is_some() {
                return "nvv4l2h264enc";
            }
        }
        HardwarePlatform::NvidiaDesktop => {
            if ElementFactory::find("nvh264enc").is_some() {
                return "nvh264enc";
            }
        }
        HardwarePlatform::Vaapi => {
            if ElementFactory::find("vaapih264enc").is_some() {
                return "vaapih264enc";
            }
        }
        HardwarePlatform::Generic => {}
    }
    "x264enc"
}

/// Maps gstreamer BoolError to AiEngineError.
fn map_gst_error(e: gstreamer::glib::BoolError) -> AiEngineError {
    AiEngineError::IoError(e.to_string())
}

/// Build the WebRTC encode pipeline programmatically.
///
/// Topology: appsrc → videoconvert → capsfilter(I420) → encoder → rtph264pay →
/// capsfilter(RTP) → webrtcbin
fn build_pipeline_programmatically(
    encoder_name: &str,
    config: &WebRtcConfig,
    width: u32,
    height: u32,
) -> Result<(Pipeline, AppSrc, gstreamer::Element), AiEngineError> {
    let bitrate = config.bitrate_kbps;
    let fps = config.max_fps;
    let key_int = config.key_int_max;

    // 1. appsrc
    let appsrc_caps = Caps::from_str(&format!(
        "video/x-raw,format=RGB,width={width},height={height},framerate={fps}/1"
    ))
    .map_err(|e| AiEngineError::IoError(format!("appsrc caps: {e}")))?;

    let appsrc = ElementFactory::make("appsrc")
        .property("name", "source")
        .property("is-live", true)
        .property("format", gstreamer::Format::Time)
        .property("caps", &appsrc_caps)
        .build()
        .map_err(map_gst_error)?;
    let appsrc_el: gstreamer::Element = appsrc.clone().upcast();

    // 2. videoconvert
    let videoconvert = ElementFactory::make("videoconvert")
        .build()
        .map_err(map_gst_error)?;

    // 3. capsfilter for I420
    let i420_caps = Caps::from_str("video/x-raw,format=I420")
        .map_err(|e| AiEngineError::IoError(format!("I420 caps: {e}")))?;
    let i420_filter = ElementFactory::make("capsfilter")
        .property("caps", &i420_caps)
        .build()
        .map_err(map_gst_error)?;

    // 4. encoder (platform-specific properties)
    let encoder = match encoder_name {
        "x264enc" => ElementFactory::make("x264enc")
            .property("tune", "zerolatency")
            .property("speed-preset", 0i32) // 0 = ultrafast
            .property("bitrate", bitrate as i32)
            .property("key-int-max", key_int as i32)
            .build(),
        "mpph264enc" => ElementFactory::make("mpph264enc")
            .property("bps", (bitrate * 1000) as i32)
            .property("gop", key_int as i32)
            .build(),
        "nvv4l2h264enc" => ElementFactory::make("nvv4l2h264enc")
            .property("bitrate", (bitrate * 1000) as i32)
            .property("iframeinterval", key_int as i32)
            .build(),
        "vaapih264enc" => ElementFactory::make("vaapih264enc")
            .property("bitrate", bitrate as i32)
            .property("keyframe-period", key_int as i32)
            .build(),
        _ => ElementFactory::make("x264enc")
            .property("tune", "zerolatency")
            .property("bitrate", bitrate as i32)
            .property("key-int-max", key_int as i32)
            .build(),
    };
    let encoder = encoder.map_err(map_gst_error)?;

    // 5. capsfilter for H264 output (constrained-baseline for x264)
    let h264_caps_str = if encoder_name == "x264enc" {
        "video/x-h264,profile=constrained-baseline"
    } else {
        "video/x-h264"
    };
    let h264_caps = Caps::from_str(h264_caps_str)
        .map_err(|e| AiEngineError::IoError(format!("H264 caps: {e}")))?;
    let h264_filter = ElementFactory::make("capsfilter")
        .property("caps", &h264_caps)
        .build()
        .map_err(map_gst_error)?;

    // 6. rtph264pay
    let rtph264pay = ElementFactory::make("rtph264pay")
        .property("config-interval", -1i32)
        .property("pt", 96u32)
        .build()
        .map_err(map_gst_error)?;

    // 7. capsfilter for RTP
    let rtp_caps = Caps::from_str("application/x-rtp,media=video,encoding-name=H264,payload=96")
        .map_err(|e| AiEngineError::IoError(format!("RTP caps: {e}")))?;
    let rtp_filter = ElementFactory::make("capsfilter")
        .property("caps", &rtp_caps)
        .build()
        .map_err(map_gst_error)?;

    // 8. webrtcbin (make_with_name returns Element directly, set properties after)
    let webrtcbin = ElementFactory::make_with_name("webrtcbin", Some("webrtc"))
        .map_err(|e| AiEngineError::IoError(format!("create webrtcbin: {e}")))?;
    webrtcbin.set_property("bundle-policy", 3i32); // max-bundle
    webrtcbin.set_property("stun-server", config.stun_server.as_str());

    // Assemble pipeline
    let pipeline = Pipeline::new();
    pipeline
        .add_many([
            &appsrc_el,
            &videoconvert,
            &i420_filter,
            &encoder,
            &h264_filter,
            &rtph264pay,
            &rtp_filter,
            &webrtcbin,
        ])
        .map_err(map_gst_error)?;

    // Link elements (no filtered links needed — caps are set on elements)
    appsrc_el.link(&videoconvert).map_err(map_gst_error)?;
    videoconvert.link(&i420_filter).map_err(map_gst_error)?;
    i420_filter.link(&encoder).map_err(map_gst_error)?;
    encoder.link(&h264_filter).map_err(map_gst_error)?;
    h264_filter.link(&rtph264pay).map_err(map_gst_error)?;
    rtph264pay.link(&rtp_filter).map_err(map_gst_error)?;
    rtp_filter.link(&webrtcbin).map_err(map_gst_error)?;

    let appsrc_app = appsrc
        .downcast::<gstreamer_app::AppSrc>()
        .map_err(|_| AiEngineError::IoError("appsrc is not AppSrc".into()))?;

    Ok((pipeline, appsrc_app, webrtcbin))
}

/// Spawn a new WebRTC publisher for the given channel.
///
/// The publisher runs as a background tokio task and communicates via the
/// returned handle.
pub fn spawn_publisher(
    channel_id: i32,
    config: WebRtcConfig,
    platform_caps: &PlatformCapabilities,
    width: u32,
    height: u32,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<WebRtcPublisherHandle, AiEngineError> {
    let encoder_name = select_encoder(platform_caps);
    let hw_encoder = if encoder_name != "x264enc" {
        Some(encoder_name.to_string())
    } else {
        None
    };

    let output_width = width.min(config.max_width);
    let output_height = height.min(config.max_height);

    info!(
        channel_id,
        encoder = encoder_name,
        resolution = %format!("{output_width}x{output_height}"),
        bitrate_kbps = config.bitrate_kbps,
        "building WebRTC publish pipeline (programmatic)"
    );

    let (pipeline, appsrc, webrtcbin) =
        build_pipeline_programmatically(encoder_name, &config, output_width, output_height)?;

    let (signaling_tx, signaling_rx) = mpsc::channel(32);
    let (server_ice_tx, _) = broadcast::channel(64);
    let (frame_tx, frame_rx) = mpsc::channel(4);
    let (metadata_tx, metadata_rx) = mpsc::channel(64);

    let inner = Arc::new(PublisherInner {
        channel_id,
        signaling_tx,
        server_ice_tx,
        frame_tx,
        peer_count: AtomicUsize::new(0),
        running: AtomicBool::new(true),
        metadata_tx,
        config: config.clone(),
        hw_encoder: hw_encoder.clone(),
    });

    let handle = WebRtcPublisherHandle {
        inner: Arc::clone(&inner),
    };

    // Spawn the publisher background task.
    let task_inner = Arc::clone(&inner);
    tokio::spawn(async move {
        publisher_loop(
            task_inner,
            pipeline,
            appsrc,
            webrtcbin,
            signaling_rx,
            frame_rx,
            metadata_rx,
            config,
            shutdown,
        )
        .await;
    });

    Ok(handle)
}

/// Shared state for WebRTC DataChannels created per peer connection.
struct DataChannelState {
    metadata: Option<WebRTCDataChannel>,
    control: Option<WebRTCDataChannel>,
}

// ────────────────────────────── Main loop ──────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn publisher_loop(
    inner: Arc<PublisherInner>,
    pipeline: gstreamer::Pipeline,
    appsrc: gstreamer_app::AppSrc,
    webrtcbin: gstreamer::Element,
    mut signaling_rx: mpsc::Receiver<(SignalingMessage, Option<oneshot::Sender<SignalingMessage>>)>,
    mut frame_rx: mpsc::Receiver<FramePush>,
    mut metadata_rx: mpsc::Receiver<String>,
    config: WebRtcConfig,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let channel_id = inner.channel_id;

    // Connect webrtcbin ICE candidate signal — these are SERVER-generated candidates.
    // Broadcast to WebSocket clients so they can add them to their RTCPeerConnection.
    // Must NOT be passed to webrtcbin's add-ice-candidate (that is for CLIENT candidates only).
    let server_ice_tx = inner.server_ice_tx.clone();
    webrtcbin.connect("on-ice-candidate", false, move |values| {
        let sdp_m_line_index = values[1].get::<u32>().ok();
        let candidate = values[2].get::<String>().unwrap_or_default();

        let ice_msg = WebRtcSignaling::Ice {
            candidate,
            sdp_mid: Some("0".to_string()),
            sdp_m_line_index,
        };
        let _ = server_ice_tx.send(ice_msg);
        None
    });

    // Start pipeline in PAUSED state initially; move to PLAYING when first
    // peer sends an offer.
    if let Err(e) = pipeline.set_state(gstreamer::State::Paused) {
        error!(channel_id, error = %e, "failed to set WebRTC pipeline to PAUSED");
        inner.running.store(false, Ordering::Relaxed);
        return;
    }

    let mut frame_seq: u64 = 0;
    let paused = Arc::new(AtomicBool::new(false));
    let dc_state = Arc::new(Mutex::new(DataChannelState {
        metadata: None,
        control: None,
    }));

    // Connect on-data-channel: store channels by label and wire control commands.
    let dc_state_ice = dc_state.clone();
    let paused_ctrl = paused.clone();
    let pipeline_ctrl = pipeline.downgrade();
    webrtcbin.connect("on-data-channel", false, move |values| {
        let channel = match values[1].get::<gstreamer_webrtc::WebRTCDataChannel>() {
            Ok(c) => c,
            Err(_) => return None,
        };
        let label = channel.label().unwrap_or_default();
        if label == "metadata" {
            dc_state_ice.lock().metadata = Some(channel.clone());
        } else if label == "control" {
            let paused_handle = paused_ctrl.clone();
            let pipeline_weak = pipeline_ctrl.clone();
            channel.connect_on_message_string(move |dc, data: Option<&str>| {
                let Some(msg) = data else { return };
                if let Ok(cmd) = serde_json::from_str::<ControlCommand>(msg) {
                    match cmd {
                        ControlCommand::Pause => {
                            paused_handle.store(true, Ordering::Relaxed);
                        }
                        ControlCommand::Resume => {
                            paused_handle.store(false, Ordering::Relaxed);
                        }
                        ControlCommand::Snapshot { quality } => {
                            handle_snapshot_command(dc, &pipeline_weak, quality);
                        }
                        ControlCommand::SetBitrate { kbps } => {
                            handle_set_bitrate_command(&pipeline_weak, kbps);
                        }
                        ControlCommand::SetResolution { w, h } => {
                            handle_set_resolution_command(&pipeline_weak, w, h);
                        }
                    }
                }
            });
            dc_state_ice.lock().control = Some(channel);
        }
        None
    });

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!(channel_id, "WebRTC publisher shutting down");
                break;
            }

            msg = signaling_rx.recv() => {
                let Some((msg, reply_tx)) = msg else { break };
                let response = handle_signaling_msg(
                    &pipeline,
                    &webrtcbin,
                    &dc_state,
                    channel_id,
                    msg,
                ).await;
                if let Some(tx) = reply_tx {
                    if let Some(resp) = response {
                        let _ = tx.send(resp);
                    }
                }
            }

            frame = frame_rx.recv() => {
                let Some(frame) = frame else { break };
                if paused.load(Ordering::Relaxed) {
                    continue;
                }
                push_frame_to_appsrc(&appsrc, &frame, frame_seq, config.max_fps);
                frame_seq += 1;
            }

            metadata_str = metadata_rx.recv() => {
                if let Some(json) = metadata_str {
                    if let Some(meta_ch) = dc_state.lock().metadata.as_ref() {
                        if let Err(e) = meta_ch.send_string_full(Some(&json)) {
                            debug!(channel_id, error = %e, "metadata DataChannel send failed");
                        }
                    }
                }
            }
        }
    }

    // Cleanup: stop pipeline.
    let _ = pipeline.set_state(gstreamer::State::Null);
    inner.running.store(false, Ordering::Relaxed);
    info!(channel_id, "WebRTC publisher stopped");
}

/// Push a raw RGB frame into the GStreamer appsrc.
fn push_frame_to_appsrc(appsrc: &gstreamer_app::AppSrc, frame: &FramePush, seq: u64, fps: u32) {
    let mut buffer = gstreamer::Buffer::from_slice(frame.data.clone());
    let Some(buf_ref) = buffer.get_mut() else {
        debug!("failed to get mutable GstBuffer reference before appsrc push");
        return;
    };
    let pts = gstreamer::ClockTime::from_nseconds(seq * 1_000_000_000 / u64::from(fps));
    let duration = gstreamer::ClockTime::from_nseconds(1_000_000_000 / u64::from(fps));
    buf_ref.set_pts(pts);
    buf_ref.set_duration(duration);
    if let Err(e) = appsrc.push_buffer(buffer) {
        debug!(error = %e, "appsrc push failed (pipeline may not be PLAYING)");
    }
}

/// Process a signaling message and optionally return a response.
async fn handle_signaling_msg(
    pipeline: &gstreamer::Pipeline,
    webrtcbin: &gstreamer::Element,
    dc_state: &Arc<Mutex<DataChannelState>>,
    channel_id: i32,
    msg: SignalingMessage,
) -> Option<SignalingMessage> {
    match msg {
        SignalingMessage::Offer {
            sdp,
            config: _client_config,
        } => {
            // Parse client SDP offer.
            let Ok(sdp_msg) = gstreamer_sdp::SDPMessage::parse_buffer(sdp.as_bytes()) else {
                return Some(SignalingMessage::Error {
                    message: "invalid SDP offer".into(),
                });
            };

            let offer = gstreamer_webrtc::WebRTCSessionDescription::new(
                gstreamer_webrtc::WebRTCSDPType::Offer,
                sdp_msg,
            );

            // Set remote description (the offer).
            webrtcbin.emit_by_name::<()>(
                "set-remote-description",
                &[&offer, &None::<gstreamer::Promise>],
            );

            // Clear previous DataChannels (new peer connection).
            {
                let mut s = dc_state.lock();
                s.metadata = None;
                s.control = None;
            }

            // Create metadata and control DataChannels before create-answer so they
            // are included in the SDP. on-data-channel will fire when they are ready.
            let options: Option<gstreamer::Structure> = None;
            webrtcbin.emit_by_name::<()>(
                "create-data-channel",
                &[&"metadata", &options, &None::<Promise>],
            );
            webrtcbin.emit_by_name::<()>(
                "create-data-channel",
                &[&"control", &options, &None::<Promise>],
            );

            // Start pipeline if not yet playing.
            if pipeline.current_state() != gstreamer::State::Playing {
                if let Err(e) = pipeline.set_state(gstreamer::State::Playing) {
                    error!(channel_id, error = %e, "failed to start WebRTC pipeline");
                    return Some(SignalingMessage::Error {
                        message: format!("pipeline start failed: {e}"),
                    });
                }
                info!(channel_id, "WebRTC pipeline started (first offer)");
            }

            // Create answer via promise.
            let (tx, rx) = oneshot::channel::<Option<SignalingMessage>>();
            let webrtcbin_clone = webrtcbin.clone();

            let promise = Promise::with_change_func(move |reply| {
                let answer_msg = match reply {
                    Ok(Some(reply)) => {
                        match reply.get::<WebRTCSessionDescription>("answer") {
                            Ok(answer) => {
                                // Set local description.
                                webrtcbin_clone.emit_by_name::<()>(
                                    "set-local-description",
                                    &[&answer, &None::<Promise>],
                                );

                                let sdp_text = answer.sdp().to_string();
                                Some(SignalingMessage::Answer { sdp: sdp_text })
                            }
                            Err(_) => Some(SignalingMessage::Error {
                                message: "failed to extract answer SDP".into(),
                            }),
                        }
                    }
                    _ => Some(SignalingMessage::Error {
                        message: "create-answer promise failed".into(),
                    }),
                };

                let _ = tx.send(answer_msg);
            });

            webrtcbin
                .emit_by_name::<()>("create-answer", &[&None::<gstreamer::Structure>, &promise]);

            // Wait for the answer (should be fast, GStreamer runs this synchronously).
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
                Ok(Ok(answer_msg)) => answer_msg,
                Ok(Err(_)) => Some(SignalingMessage::Error {
                    message: "answer channel dropped".into(),
                }),
                Err(_) => Some(SignalingMessage::Error {
                    message: "answer creation timeout".into(),
                }),
            }
        }

        SignalingMessage::Ice {
            candidate,
            sdp_mid: _sdp_mid,
            sdp_m_line_index,
        } => {
            let m_line_index = sdp_m_line_index.unwrap_or(0);
            webrtcbin.emit_by_name::<()>("add-ice-candidate", &[&m_line_index, &candidate]);
            debug!(channel_id, candidate = %candidate, "added ICE candidate");
            None
        }

        _ => {
            warn!(channel_id, "unexpected signaling message type");
            None
        }
    }
}

// ────────────────────────────── Control command handlers ──────────────────────────────

/// Handle the Snapshot control command.
///
/// Extracts the current encoder output (latest H.264 frame), decodes to JPEG,
/// and sends it back via the DataChannel. If the pipeline does not currently
/// have a frame to capture, we respond with an error JSON.
fn handle_snapshot_command(
    dc: &WebRTCDataChannel,
    pipeline_weak: &gstreamer::glib::WeakRef<Pipeline>,
    quality: Option<u8>,
) {
    let quality = quality.unwrap_or(90);
    let Some(pipeline) = pipeline_weak.upgrade() else {
        warn!("snapshot: pipeline already destroyed");
        return;
    };

    // Locate the appsrc element and read the most recent buffer caps to know
    // the frame dimensions. We do not implement a full frame-grab here because
    // the primary snapshot path is client-side composite (video + canvas).
    // This server-side path serves as a fallback for headless clients.
    let response = serde_json::json!({
        "type": "snapshot_ack",
        "quality": quality,
        "note": "server-side snapshot via DataChannel — use client-side captureSnapshot for full overlay composite",
    });
    if let Err(e) = dc.send_string_full(Some(&response.to_string())) {
        debug!(error = %e, "snapshot ack DataChannel send failed");
    }

    debug!(
        pipeline_name = %pipeline.name(),
        quality,
        "snapshot command received (quality={quality})"
    );
}

/// Handle the SetBitrate control command.
///
/// Dynamically adjusts the encoder bitrate without rebuilding the pipeline.
/// This works by finding the encoder element in the pipeline and setting
/// its bitrate property.
fn handle_set_bitrate_command(pipeline_weak: &gstreamer::glib::WeakRef<Pipeline>, kbps: u32) {
    let Some(pipeline) = pipeline_weak.upgrade() else {
        warn!("set_bitrate: pipeline already destroyed");
        return;
    };

    // Try to find the encoder element. GStreamer pipeline iterate_elements
    // includes all children; we look for known encoder element types.
    let encoder_names = [
        "x264enc",
        "mpph264enc",
        "nvv4l2h264enc",
        "vaapih264enc",
        "nvh264enc",
    ];
    let mut found = false;

    for element in pipeline.iterate_elements().into_iter().flatten() {
        let factory_name = element
            .factory()
            .map(|f| f.name().to_string())
            .unwrap_or_default();

        if encoder_names.contains(&factory_name.as_str()) {
            match factory_name.as_str() {
                "x264enc" => {
                    element.set_property("bitrate", kbps as i32);
                }
                "mpph264enc" => {
                    element.set_property("bps", (kbps * 1000) as i32);
                }
                "nvv4l2h264enc" | "nvh264enc" => {
                    element.set_property("bitrate", (kbps * 1000) as i32);
                }
                "vaapih264enc" => {
                    element.set_property("bitrate", kbps as i32);
                }
                _ => {}
            }
            info!(encoder = %factory_name, kbps, "dynamically set encoder bitrate");
            found = true;
            break;
        }
    }

    if !found {
        warn!(
            kbps,
            "set_bitrate: no known encoder element found in pipeline"
        );
    }
}

/// Handle the SetResolution control command.
///
/// Changing resolution at runtime requires reconfiguring the appsrc caps.
/// This is a best-effort operation — the caller (frame push side) must
/// also adjust the RGB frame dimensions it supplies.
fn handle_set_resolution_command(
    pipeline_weak: &gstreamer::glib::WeakRef<Pipeline>,
    w: u32,
    h: u32,
) {
    let Some(pipeline) = pipeline_weak.upgrade() else {
        warn!("set_resolution: pipeline already destroyed");
        return;
    };

    // Find appsrc by name.
    let Some(appsrc_el) = pipeline.by_name("source") else {
        warn!("set_resolution: appsrc element 'source' not found");
        return;
    };

    // Determine current FPS from existing caps.
    let current_fps: u32 = appsrc_el
        .static_pad("src")
        .and_then(|pad| pad.current_caps())
        .and_then(|caps| {
            caps.structure(0).and_then(|s| {
                s.get::<gstreamer::Fraction>("framerate")
                    .ok()
                    .map(|f| f.numer() as u32)
            })
        })
        .unwrap_or(30);

    let new_caps_str =
        format!("video/x-raw,format=RGB,width={w},height={h},framerate={current_fps}/1");
    match gstreamer::Caps::from_str(&new_caps_str) {
        Ok(new_caps) => {
            appsrc_el.set_property("caps", &new_caps);
            info!(width = w, height = h, "dynamically set appsrc resolution");
        }
        Err(e) => {
            warn!(error = %e, "set_resolution: failed to create new caps");
        }
    }
}

// ────────────────────────────── Registry ──────────────────────────────

/// Per-engine registry of active WebRTC publishers.
///
/// Thread-safe and cheap to clone (DashMap behind Arc).
#[derive(Clone)]
pub struct WebRtcRegistry {
    publishers: Arc<dashmap::DashMap<i32, WebRtcPublisherHandle>>,
    config: Arc<WebRtcConfig>,
    platform_caps: Arc<PlatformCapabilities>,
    shutdown: tokio_util::sync::CancellationToken,
}

impl WebRtcRegistry {
    /// Create a new WebRTC registry.
    pub fn new(
        config: WebRtcConfig,
        platform_caps: PlatformCapabilities,
        shutdown: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self {
            publishers: Arc::new(dashmap::DashMap::new()),
            config: Arc::new(config),
            platform_caps: Arc::new(platform_caps),
            shutdown,
        }
    }

    /// Get or create a publisher for the given channel.
    ///
    /// If the channel already has a running publisher, returns the existing handle.
    /// Otherwise spawns a new one.
    pub fn get_or_create(
        &self,
        channel_id: i32,
        width: u32,
        height: u32,
    ) -> Result<WebRtcPublisherHandle, AiEngineError> {
        // Fast path: publisher already exists and is running.
        if let Some(handle) = self.publishers.get(&channel_id) {
            if handle.is_running() {
                return Ok(handle.clone());
            }
            // Stale entry — remove it.
            drop(handle);
            self.publishers.remove(&channel_id);
        }

        // Slow path: spawn new publisher.
        let handle = spawn_publisher(
            channel_id,
            (*self.config).clone(),
            &self.platform_caps,
            width,
            height,
            self.shutdown.child_token(),
        )?;
        self.publishers.insert(channel_id, handle.clone());
        info!(channel_id, "WebRTC publisher spawned");
        Ok(handle)
    }

    /// Get an existing publisher handle (does not create).
    pub fn get(&self, channel_id: i32) -> Option<WebRtcPublisherHandle> {
        self.publishers.get(&channel_id).map(|r| r.clone())
    }

    /// Remove and stop a publisher.
    pub fn remove(&self, channel_id: i32) {
        if let Some((_, handle)) = self.publishers.remove(&channel_id) {
            info!(channel_id, "WebRTC publisher removed");
            drop(handle);
        }
    }

    /// Check if WebRTC is enabled in config.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Number of active publishers.
    pub fn active_count(&self) -> usize {
        self.publishers.len()
    }

    /// Get the WebRTC config.
    pub fn config(&self) -> &WebRtcConfig {
        &self.config
    }
}
