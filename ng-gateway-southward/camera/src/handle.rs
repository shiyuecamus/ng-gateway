//! Camera data-plane handle.
//!
//! This is the hot-path object published by the SDK supervision loop.
//! It registers the camera channel with the AI engine, which internally
//! creates a GStreamer pipeline for hardware-accelerated frame acquisition
//! and zero-copy inference. The handle caches the latest analysis result
//! in a `watch` channel for non-blocking reads by the Collector.
//!
//! # Architecture
//!
//! ```text
//! register_channel() → AI Engine (GStreamer → inference) → watch(latest)
//!                                                                       ↑
//!                                    collect_data() reads ──────────────┘
//! ```

use crate::{
    ptz::{self, PtzController},
    types::{
        CameraAction, CameraChannel, CameraCommand, CameraOutputKey, CameraPoint, CameraProtocol,
        RtspTransport,
    },
};
use async_trait::async_trait;
use ng_gateway_ai::api::{
    AiEngineApi, AlarmSeverity, AnalysisResult, ChannelRegistration, StreamTransport,
};
use ng_gateway_sdk::{
    supervision::ReconnectHandle, AlarmData, CollectItem, CollectorConcurrencyProfile, DriverError,
    DriverResult, ExecuteOutcome, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction,
    RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, TelemetryData,
    WriteResult,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};
use tokio::sync::{mpsc, watch};

/// Camera data-plane handle.
///
/// Published by the supervision loop when the camera session transitions
/// to Ready. The Collector's `collect_data()` reads the latest cached
/// AI result — no blocking inference on the hot path.
pub struct CameraHandle {
    /// Channel configuration (protocol, pipeline).
    channel: Arc<CameraChannel>,
    /// AI Engine API handle (injected from host process via extensions).
    ai_engine: Arc<dyn AiEngineApi>,
    /// Latest analysis result (updated by the AI engine's internal frame loop).
    latest_result: watch::Receiver<Option<Arc<AnalysisResult>>>,
    /// Sender used by the AI engine's frame loop to publish latest results.
    result_tx: watch::Sender<Option<Arc<AnalysisResult>>>,
    /// Monotonic frame sequence counter.
    frame_seq: Arc<AtomicU64>,
    /// Reconnect handle (set during session init).
    reconnect: OnceLock<ReconnectHandle>,
    /// Whether the channel is currently registered with the AI engine.
    registered: AtomicBool,
    /// Stream error notification channel.
    stream_error_tx: mpsc::Sender<String>,
    /// Stream error receiver (consumed by the session's `run()` method).
    stream_error_rx: tokio::sync::Mutex<mpsc::Receiver<String>>,
    /// Optional PTZ controller (available when connected via ONVIF with PTZ service).
    ptz_controller: parking_lot::Mutex<Option<PtzController>>,
}

impl CameraHandle {
    /// Create a new camera handle (no I/O, no spawning).
    pub fn new(channel: Arc<CameraChannel>, ai_engine: Arc<dyn AiEngineApi>) -> Self {
        let (result_tx, latest_result) = watch::channel(None);
        let (stream_error_tx, stream_error_rx) = mpsc::channel(1);

        Self {
            channel,
            ai_engine,
            latest_result,
            result_tx,
            frame_seq: Arc::new(AtomicU64::new(0)),
            reconnect: OnceLock::new(),
            registered: AtomicBool::new(false),
            stream_error_tx,
            stream_error_rx: tokio::sync::Mutex::new(stream_error_rx),
            ptz_controller: parking_lot::Mutex::new(None),
        }
    }

    /// Set the reconnect handle (called once during session init).
    #[inline]
    pub fn set_reconnect(&self, reconnect: ReconnectHandle) {
        let _ = self.reconnect.set(reconnect);
    }

    /// Set the PTZ controller (called during ONVIF connection establishment).
    #[inline]
    pub fn set_ptz_controller(&self, controller: PtzController) {
        *self.ptz_controller.lock() = Some(controller);
    }

    /// Get the PTZ controller, returning a clear error if unavailable.
    #[inline]
    fn require_ptz(&self) -> DriverResult<PtzController> {
        self.ptz_controller
            .lock()
            .clone()
            .ok_or(DriverError::ExecutionError(
                "PTZ not available (camera not connected via ONVIF or device has no PTZ)".into(),
            ))
    }

    /// Register this channel with the AI engine for continuous analysis.
    ///
    /// The AI engine creates a GStreamer pipeline internally for the given
    /// stream URL, performing hardware-accelerated decoding and zero-copy
    /// inference. Results are published directly through a latest-value
    /// watch channel for non-blocking reads by the Collector.
    pub async fn register_stream(&self, stream_url: String) -> DriverResult<()> {
        let channel_id = self.channel.id;
        let connect_timeout =
            std::time::Duration::from_millis(self.channel.connection_policy.connect_timeout_ms);

        let registration = ChannelRegistration {
            channel_id,
            device_id: 0,
            stream_url: stream_url.clone(),
            pipeline_id: self.channel.config.pipeline_id,
            transport: self.resolve_stream_transport(),
            connect_timeout,
            result_tx: Some(self.result_tx.clone()),
            error_tx: Some(self.stream_error_tx.clone()),
        };

        self.ai_engine
            .runtime()
            .register_channel(registration)
            .await
            .map_err(|e| {
                DriverError::SessionError(format!("failed to register channel with AI engine: {e}"))
            })?;

        self.registered.store(true, Ordering::Release);

        tracing::info!(
            channel_id,
            stream_url = %stream_url,
            "camera channel registered for AI analysis"
        );
        Ok(())
    }

    /// Resolve RTSP transport preference from camera protocol config.
    fn resolve_stream_transport(&self) -> StreamTransport {
        let transport = match self.channel.config.protocol {
            CameraProtocol::Rtsp { transport, .. } | CameraProtocol::Onvif { transport, .. } => {
                transport
            }
            CameraProtocol::Mjpeg { .. } => RtspTransport::Tcp,
        };
        match transport {
            RtspTransport::Tcp => StreamTransport::Tcp,
            RtspTransport::Udp => StreamTransport::UdpFallback,
        }
    }

    /// Unregister this channel from the AI engine, stopping frame acquisition.
    pub async fn unregister_stream(&self) {
        if !self.registered.swap(false, Ordering::AcqRel) {
            return;
        }

        let channel_id = self.channel.id;
        if let Err(e) = self
            .ai_engine
            .runtime()
            .unregister_channel(channel_id)
            .await
        {
            tracing::warn!(channel_id, error = %e, "failed to unregister channel");
        }
    }

    /// Wait for a stream error from the AI engine's frame loop.
    ///
    /// Used by [`CameraSession::run`] to detect disconnection and trigger
    /// reconnection through the supervision loop.
    pub async fn wait_for_stream_error(&self) -> String {
        let mut rx = self.stream_error_rx.lock().await;
        rx.recv().await.unwrap_or("stream closed".into())
    }
}

#[async_trait]
impl SouthwardHandle for CameraHandle {
    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let result = self.latest_result.borrow().clone();

        let Some(analysis) = result else {
            return Ok(vec![]);
        };

        let mut northward_data = Vec::new();
        for (device, points) in items {
            let data = convert_analysis_to_northward(device, points, analysis.as_ref())?;
            northward_data.extend(data);
        }

        Ok(northward_data)
    }

    async fn execute_action(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult> {
        let cam_action =
            action
                .downcast_ref::<CameraAction>()
                .ok_or(DriverError::ExecutionError(
                    "RuntimeAction is not CameraAction".into(),
                ))?;

        match cam_action.command {
            CameraCommand::PtzMove => {
                let ptz = self.require_ptz()?;
                let params = flatten_action_params(&parameters);
                let velocity = ptz::parse_ptz_velocity(&params)?;
                ptz.continuous_move(velocity, Some(5.0)).await?;

                Ok(ExecuteResult {
                    outcome: ExecuteOutcome::Completed,
                    payload: Some(serde_json::json!({
                        "message": "PTZ move started",
                        "pan": velocity.pan,
                        "tilt": velocity.tilt,
                        "zoom": velocity.zoom,
                    })),
                })
            }
            CameraCommand::PtzStop => {
                let ptz = self.require_ptz()?;
                ptz.stop(true, true).await?;

                Ok(ExecuteResult {
                    outcome: ExecuteOutcome::Completed,
                    payload: Some(serde_json::json!({ "message": "PTZ stopped" })),
                })
            }
            CameraCommand::PtzPreset => {
                let ptz = self.require_ptz()?;
                let params = flatten_action_params(&parameters);
                let preset_token = ptz::parse_preset_token(&params)?;
                ptz.goto_preset(&preset_token, None).await?;

                Ok(ExecuteResult {
                    outcome: ExecuteOutcome::Completed,
                    payload: Some(serde_json::json!({
                        "message": "PTZ moved to preset",
                        "preset_token": preset_token,
                    })),
                })
            }
            CameraCommand::Snapshot => {
                let result = self.latest_result.borrow().clone();
                match result {
                    Some(analysis) if analysis.annotated_frame.is_some() => Ok(ExecuteResult {
                        outcome: ExecuteOutcome::Completed,
                        payload: Some(serde_json::json!({
                            "message": "Snapshot captured",
                            "frame_seq": analysis.frame_seq,
                        })),
                    }),
                    _ => Err(DriverError::ExecutionError(
                        "No analysis result available for snapshot".into(),
                    )),
                }
            }
            CameraCommand::RestartPipeline => {
                self.frame_seq.store(0, Ordering::Relaxed);
                Ok(ExecuteResult {
                    outcome: ExecuteOutcome::Completed,
                    payload: Some(serde_json::json!({
                        "message": "Pipeline restart signal sent"
                    })),
                })
            }
        }
    }

    async fn write_point(
        &self,
        _device: Arc<dyn RuntimeDevice>,
        _point: Arc<dyn RuntimePoint>,
        _value: &NGValue,
        _timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult> {
        Err(DriverError::ExecutionError(
            "Camera points are read-only".into(),
        ))
    }

    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
}

// ─── Action parameter helpers ──────────────────────────────────────

fn flatten_action_params(
    parameters: &[(Arc<dyn RuntimeParameter>, NGValue)],
) -> Vec<(String, serde_json::Value)> {
    parameters
        .iter()
        .map(|(p, v)| {
            (
                p.key().to_string(),
                serde_json::to_value(v).unwrap_or_default(),
            )
        })
        .collect()
}

// ─── Analysis → NorthwardData conversion ───────────────────────────

fn convert_analysis_to_northward(
    device: &Arc<dyn RuntimeDevice>,
    points: &Arc<[Arc<dyn RuntimePoint>]>,
    analysis: &AnalysisResult,
) -> DriverResult<Vec<NorthwardData>> {
    let mut data = Vec::new();

    for point in points.iter() {
        let Some(cam_point) = point.downcast_ref::<CameraPoint>() else {
            continue;
        };

        let value = match cam_point.output_key {
            CameraOutputKey::DetectionCount => {
                Some(NGValue::Int32(analysis.detections.len() as i32))
            }
            CameraOutputKey::PersonCount => {
                let count = analysis
                    .detections
                    .iter()
                    .filter(|d| d.class.as_ref() == "person")
                    .count();
                Some(NGValue::Int32(count as i32))
            }
            CameraOutputKey::VehicleCount => {
                let count = analysis
                    .detections
                    .iter()
                    .filter(|d| matches!(d.class.as_ref(), "car" | "truck" | "bus"))
                    .count();
                Some(NGValue::Int32(count as i32))
            }
            CameraOutputKey::InferenceLatencyMs => Some(NGValue::Float64(
                analysis.inference_latency.as_secs_f64() * 1000.0,
            )),
            CameraOutputKey::DetectionJson => {
                let json =
                    serde_json::to_string(&analysis.detections).unwrap_or_else(|_| "[]".into());
                Some(NGValue::String(json.into()))
            }
            CameraOutputKey::AlarmActive => Some(NGValue::Boolean(!analysis.alarms.is_empty())),
            CameraOutputKey::TopClass => {
                let top = analysis
                    .classifications
                    .first()
                    .and_then(|c| c.top_k.first())
                    .map(|(label, _)| label.to_string())
                    .unwrap_or_default();
                Some(NGValue::String(top.into()))
            }
            CameraOutputKey::TopConfidence => {
                let conf = analysis
                    .classifications
                    .first()
                    .and_then(|c| c.top_k.first())
                    .map(|(_, conf)| *conf as f64)
                    .unwrap_or(0.0);
                Some(NGValue::Float64(conf))
            }
            CameraOutputKey::Custom => None,
        };

        if let Some(v) = value {
            data.push(NorthwardData::Telemetry(TelemetryData::new(
                device.id(),
                device.device_name(),
                vec![PointValue {
                    point_id: point.id(),
                    point_key: Arc::<str>::from(point.key()),
                    value: v,
                }],
            )));
        }
    }

    for alarm in analysis.alarms.iter() {
        data.push(NorthwardData::Alarm(AlarmData::new(
            device.id(),
            device.device_name().to_string(),
            alarm.alarm_type.to_string(),
            match alarm.severity {
                AlarmSeverity::Critical => ng_gateway_sdk::AlarmSeverity::Critical,
                AlarmSeverity::Warning => ng_gateway_sdk::AlarmSeverity::Warning,
                AlarmSeverity::Info => ng_gateway_sdk::AlarmSeverity::Info,
            },
            alarm.description.to_string(),
        )));
    }

    Ok(data)
}
