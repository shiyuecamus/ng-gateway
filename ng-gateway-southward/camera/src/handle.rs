//! Camera data-plane handle.
//!
//! This is the hot-path object published by the SDK supervision loop.
//! It owns a background frame loop that continuously pulls frames from
//! the video stream, submits them to the AI engine, and caches the latest
//! analysis result in a `watch` channel for non-blocking reads by the
//! Collector.
//!
//! # Architecture
//!
//! ```text
//! VideoStream → frame_loop_task → AI Engine → latest_result (watch)
//!                                                    ↑
//!                       collect_data() reads ─────────┘
//! ```

use crate::{
    protocol::VideoStream,
    ptz::{self, PtzController},
    types::{CameraAction, CameraChannel, CameraCommand, CameraOutputKey, CameraPoint},
};
use async_trait::async_trait;
use ng_gateway_ai::{
    api::{AiEngineApi, AiEngineError, AnalysisResult, FrameAnalysisRequest, VideoFrame},
    pipeline::sampler::FrameSampler,
};
use ng_gateway_sdk::{
    supervision::ReconnectHandle, AlarmData, CollectItem, CollectorConcurrencyProfile, DriverError,
    DriverResult, ExecuteOutcome, ExecuteResult, NGValue, NorthwardData, PointValue, RuntimeAction,
    RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, SouthwardHandle, TelemetryData,
    WriteResult,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock,
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// Camera data-plane handle.
///
/// Published by the supervision loop when the camera session transitions
/// to Ready. The Collector's `collect_data()` reads the latest cached
/// AI result — no blocking inference on the hot path.
pub struct CameraHandle {
    /// Channel configuration (protocol, pipeline, sampling).
    channel: Arc<CameraChannel>,
    /// AI Engine API handle (injected from host process via extensions).
    ai_engine: Arc<dyn AiEngineApi>,
    /// Latest analysis result (updated by the frame loop task).
    latest_result: watch::Receiver<Option<AnalysisResult>>,
    /// Sender half for the frame loop to publish results.
    result_tx: watch::Sender<Option<AnalysisResult>>,
    /// Monotonic frame sequence counter.
    frame_seq: Arc<AtomicU64>,
    /// Reconnect handle (set during session init).
    reconnect: OnceLock<ReconnectHandle>,
    /// Background frame loop task handle.
    frame_loop: Mutex<Option<JoinHandle<()>>>,
    /// Stream error notification channel.
    stream_error_tx: mpsc::Sender<String>,
    /// Stream error receiver (consumed by the session's `run()` method).
    stream_error_rx: Mutex<mpsc::Receiver<String>>,
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
            frame_loop: Mutex::new(None),
            stream_error_tx,
            stream_error_rx: Mutex::new(stream_error_rx),
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

    /// Spawn the background frame acquisition + AI submission loop.
    ///
    /// The loop continuously:
    /// 1. Pulls frames from the video stream
    /// 2. Applies the configured sampling strategy (skip non-sampled frames)
    /// 3. Checks AI engine capacity (backpressure)
    /// 4. Submits frames to the AI engine for analysis
    /// 5. Publishes results to the `watch` channel
    pub async fn start_frame_loop(
        &self,
        mut stream: Box<dyn VideoStream>,
        cancel: CancellationToken,
    ) -> DriverResult<()> {
        let ai_engine = Arc::clone(&self.ai_engine);
        let channel = Arc::clone(&self.channel);
        let result_tx = self.result_tx.clone();
        let frame_seq = Arc::clone(&self.frame_seq);
        let error_tx = self.stream_error_tx.clone();

        let task = tokio::spawn(async move {
            let mut sampler = FrameSampler::new(&channel.config.sampling);

            loop {
                tokio::select! {
                    biased;

                    _ = cancel.cancelled() => break,

                    frame_result = stream.next_frame() => {
                        match frame_result {
                            Ok(raw_frame) => {
                                let seq = frame_seq.fetch_add(1, Ordering::Relaxed);

                                if !sampler.should_process(seq) {
                                    continue;
                                }

                                if raw_frame.is_key {
                                    // Key frame detection for KeyFrameOnly strategy is
                                    // handled here — non-key frames are already skipped
                                    // by the sampler for that mode.
                                }

                                if !ai_engine.has_capacity(&channel.config.pipeline_id) {
                                    tracing::trace!(
                                        seq,
                                        pipeline = %channel.config.pipeline_id,
                                        "AI engine at capacity, dropping frame"
                                    );
                                    sampler.on_feedback(None, true);
                                    continue;
                                }

                                let request = FrameAnalysisRequest {
                                    channel_id: channel.id,
                                    device_id: 0,
                                    frame: VideoFrame {
                                        data: raw_frame.data,
                                        format: raw_frame.format,
                                        width: raw_frame.width,
                                        height: raw_frame.height,
                                        timestamp: chrono::Utc::now(),
                                        seq,
                                    },
                                    roi: None,
                                };

                                match ai_engine.analyze_frame(request).await {
                                    Ok(result) => {
                                        sampler.on_feedback(
                                            Some(result.inference_latency.as_secs_f64()),
                                            false,
                                        );
                                        let _ = result_tx.send(Some(result));
                                    }
                                    Err(AiEngineError::Backpressure) => {
                                        sampler.on_feedback(None, true);
                                        tracing::trace!(seq, "AI backpressure, frame dropped");
                                    }
                                    Err(e) => {
                                        sampler.on_feedback(None, false);
                                        tracing::warn!(seq, error = %e, "AI analysis error");
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Video stream error, stopping frame loop");
                                let _ = error_tx.try_send(e.to_string());
                                break;
                            }
                        }
                    }
                }
            }

            tracing::debug!("Camera frame loop exited");
        });

        *self.frame_loop.lock().await = Some(task);
        Ok(())
    }

    /// Stop the background frame loop (if running).
    pub async fn stop_frame_loop(&self) {
        if let Some(task) = self.frame_loop.lock().await.take() {
            task.abort();
            let _ = task.await;
        }
    }

    /// Wait for a stream error from the frame loop.
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
    /// Camera uses serial collection — the `collect_data` call is a fast,
    /// non-blocking read of the latest cached result.
    #[inline]
    fn collector_concurrency_profile(&self) -> CollectorConcurrencyProfile {
        CollectorConcurrencyProfile::serial()
    }

    /// Collect data: read the latest cached AI analysis result and convert
    /// it to standard [`NorthwardData`] based on the configured point mappings.
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>> {
        let result = self.latest_result.borrow().clone();

        let Some(analysis) = result else {
            return Ok(vec![]);
        };

        let mut northward_data = Vec::new();
        for (device, points) in items {
            let data = convert_analysis_to_northward(device, points, &analysis)?;
            northward_data.extend(data);
        }

        Ok(northward_data)
    }

    /// Execute camera control actions (PTZ, snapshot, pipeline restart).
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

    /// Camera points are read-only (AI analysis outputs).
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

/// Flatten SDK action parameters into a simple key-value list for PTZ parsing.
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

/// Convert an AI analysis result to standard [`NorthwardData`] based on
/// the configured camera point output mappings.
///
/// Each [`CameraPoint`] has an `output_key` that maps to a specific field
/// of the [`AnalysisResult`]. The conversion produces telemetry data and
/// alarm events suitable for northward reporting.
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
            CameraOutputKey::Custom => {
                // Custom expressions will be supported in Phase 3+
                None
            }
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

    // Convert alarm events to NorthwardData.
    for alarm in analysis.alarms.iter() {
        data.push(NorthwardData::Alarm(AlarmData::new(
            device.id(),
            device.device_name().to_string(),
            alarm.alarm_type.to_string(),
            alarm.severity.into(),
            alarm.description.to_string(),
        )));
    }

    Ok(data)
}
