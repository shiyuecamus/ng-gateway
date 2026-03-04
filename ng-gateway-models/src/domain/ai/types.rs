use crate::enums::ai::{AlarmSeverity, FrameFormat};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{sync::Arc, time::Duration};

/// Frame analysis request from a camera driver to the AI engine.
#[derive(Debug, Clone)]
pub struct FrameAnalysisRequest {
    /// Source channel identifier (for pipeline routing).
    pub channel_id: i32,
    /// Source device identifier.
    pub device_id: i32,
    /// The video frame to analyze.
    pub frame: VideoFrame,
    /// Optional ROI override (if not set, uses pipeline default).
    pub roi: Option<BoundingBox>,
}

/// A video frame submitted for AI analysis.
///
/// Uses [`Bytes`] for zero-copy semantics — the frame data is reference-counted
/// and can be shared across pipeline stages without copying.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Raw frame data (encoded JPEG/H.264 NAL or decoded RGB/NV12).
    pub data: Bytes,
    /// Frame encoding format.
    pub format: FrameFormat,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Capture timestamp from the camera.
    pub timestamp: DateTime<Utc>,
    /// Monotonic frame sequence number (per-channel).
    pub seq: u64,
}

/// Axis-aligned bounding box in normalized coordinates.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

impl BoundingBox {
    #[inline]
    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }

    #[inline]
    pub fn height(&self) -> f32 {
        self.y_max - self.y_min
    }

    #[inline]
    pub fn area(&self) -> f32 {
        self.width().max(0.0) * self.height().max(0.0)
    }

    /// Compute the intersection area with another bounding box.
    #[inline]
    pub fn intersection_area(&self, other: &Self) -> f32 {
        let x1 = self.x_min.max(other.x_min);
        let y1 = self.y_min.max(other.y_min);
        let x2 = self.x_max.min(other.x_max);
        let y2 = self.y_max.min(other.y_max);
        (x2 - x1).max(0.0) * (y2 - y1).max(0.0)
    }

    /// Compute Intersection over Union with another bounding box.
    #[inline]
    pub fn iou(&self, other: &Self) -> f32 {
        let inter = self.intersection_area(other);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

/// A single object detection result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    /// Bounding box in normalized coordinates `[0.0, 1.0]`.
    pub bbox: BoundingBox,
    /// Detected class label.
    pub class: Arc<str>,
    /// Class index (model output index).
    pub class_id: u32,
    /// Detection confidence score `[0.0, 1.0]`.
    pub confidence: f32,
    /// Optional tracking ID (if tracker is enabled in pipeline).
    pub track_id: Option<u64>,
}

/// A classification result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    /// Top-K class labels with confidence scores, sorted descending.
    pub top_k: Vec<(Arc<str>, f32)>,
}

/// A single keypoint (joint) in a pose/skeleton detection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Keypoint {
    /// Normalized X coordinate `[0.0, 1.0]` relative to original frame.
    pub x: f32,
    /// Normalized Y coordinate `[0.0, 1.0]` relative to original frame.
    pub y: f32,
    /// Keypoint visibility/confidence `[0.0, 1.0]`.
    pub confidence: f32,
}

/// A pose/keypoint detection result (e.g., YOLOv8-Pose).
///
/// Combines a bounding box with a set of named keypoints forming a skeleton.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeypointDetection {
    /// Bounding box around the detected subject.
    pub bbox: BoundingBox,
    /// Detection confidence score `[0.0, 1.0]`.
    pub confidence: f32,
    /// Class label (typically "person" for pose models).
    pub class: Arc<str>,
    /// Class index.
    pub class_id: u32,
    /// Ordered keypoints (model-specific order, e.g., COCO 17-keypoint format).
    pub keypoints: Vec<Keypoint>,
    /// Optional tracking ID (if tracker is enabled).
    pub track_id: Option<u64>,
}

/// Semantic segmentation mask output.
///
/// Per-pixel class index map at the model's output resolution.
/// The mask dimensions may differ from the original frame; downstream
/// consumers should resize to match the source frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentationMask {
    /// Per-pixel class indices (row-major, `height × width`).
    pub mask: Vec<u8>,
    /// Mask width in pixels.
    pub width: u32,
    /// Mask height in pixels.
    pub height: u32,
    /// Ordered class labels (index → label).
    pub labels: Vec<Arc<str>>,
}

/// Anomaly detection result.
///
/// Combines a global anomaly score with a per-pixel heatmap indicating
/// the spatial distribution of anomalous regions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyMap {
    /// Global anomaly score `[0.0, 1.0]` (higher = more anomalous).
    pub score: f32,
    /// Per-pixel anomaly heatmap (row-major, `height × width`), values `[0.0, 1.0]`.
    /// `None` if the model does not output a spatial heatmap.
    #[serde(skip)]
    pub heatmap: Option<Vec<f32>>,
    /// Heatmap width (matches model output resolution).
    pub heatmap_width: u32,
    /// Heatmap height.
    pub heatmap_height: u32,
    /// Whether the anomaly score exceeds the configured threshold.
    pub is_anomalous: bool,
    /// Threshold used for anomaly determination.
    pub threshold: f32,
}

/// An alarm event generated by AI analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmEvent {
    /// Alarm type identifier (e.g., "intrusion", "fire", "ppe_violation").
    pub alarm_type: Arc<str>,
    /// Human-readable alarm description.
    pub description: Arc<str>,
    /// Alarm severity.
    pub severity: AlarmSeverity,
    /// Associated detections that triggered this alarm.
    pub related_detections: Vec<Detection>,
    /// Optional snapshot (JPEG) of the alarm moment.
    #[serde(skip)]
    pub snapshot: Option<Bytes>,
}

/// AI engine global status snapshot (returned by `GET /api/ai/engine/status`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    /// Whether the AI engine is enabled.
    pub enabled: bool,
    /// Active execution provider (e.g. "cpu", "cuda").
    pub execution_provider: String,
    /// Model-related status.
    pub models: EngineModelStatus,
    /// Inference-related status.
    pub inference: EngineInferenceStatus,
    /// Pipeline-related status.
    pub pipelines: EnginePipelineStatus,
    /// Algorithm-related status (WASM modules — Phase 2).
    pub algorithms: EngineAlgorithmStatus,
    /// Frame decoder status.
    pub decoder: EngineDecoderStatus,
    /// Engine uptime in seconds.
    pub uptime_secs: u64,
}

/// Model subsystem status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineModelStatus {
    /// Total models registered (discovered on disk).
    pub registered: usize,
    /// Models currently loaded into memory.
    pub loaded: usize,
    /// Approximate total memory used by loaded models (bytes).
    pub total_memory_bytes: u64,
}

/// Inference subsystem status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInferenceStatus {
    /// Currently running inferences.
    pub active_count: i64,
    /// Maximum allowed concurrent inferences.
    pub max_concurrent: usize,
    /// Remaining semaphore permits.
    pub available_permits: usize,
    /// Cumulative inference count since engine start.
    pub total_inferences: u64,
    /// Average inference latency in milliseconds.
    pub avg_latency_ms: f64,
}

/// Pipeline subsystem status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnginePipelineStatus {
    /// Total pipeline configurations registered.
    pub registered: usize,
    /// Number of channels with an active pipeline.
    pub active_channels: usize,
}

/// Algorithm subsystem status (Phase 2 — WASM modules).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineAlgorithmStatus {
    /// Total registered algorithms.
    pub registered: usize,
    /// Loaded WASM modules.
    pub wasm_modules: usize,
}

/// Frame decoder subsystem status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineDecoderStatus {
    /// Number of decoder worker threads.
    pub workers: usize,
    /// Current decode queue depth.
    pub queue_depth: usize,
}

/// Information about a built-in pre/post processor (for listing APIs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorInfo {
    /// Processor unique identifier (e.g. "letterbox", "yolov8_detection").
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this processor does.
    pub description: String,
    /// Model tasks this processor is applicable to.
    pub applicable_tasks: Vec<String>,
    /// Configurable parameters.
    pub parameters: Vec<ProcessorParameter>,
}

/// Descriptor for a single processor configuration parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorParameter {
    /// Parameter name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Data type.
    #[serde(rename = "type")]
    pub param_type: ParamType,
    /// Default value (JSON-encoded).
    pub default: Option<serde_json::Value>,
    /// Whether this parameter is required.
    pub required: bool,
}

/// Strongly-typed descriptor for processor parameter value types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    Bool,
    U8,
    U32,
    Usize,
    F32,
    F64,
    String,
    /// Fixed-size float array (e.g. normalization mean/std `[R, G, B]`).
    F32Array {
        /// Expected number of elements.
        len: usize,
    },
}

/// Aggregated analysis results from the AI pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// Source frame sequence number.
    pub frame_seq: u64,
    /// Source frame timestamp.
    pub frame_timestamp: DateTime<Utc>,
    /// Object detections (bounding boxes with class and confidence).
    pub detections: Arc<[Detection]>,
    /// Classification results (whole-frame or per-ROI).
    pub classifications: Arc<[Classification]>,
    /// Keypoint/pose detections (e.g., YOLOv8-Pose).
    pub keypoint_detections: Arc<[KeypointDetection]>,
    /// Segmentation masks (semantic segmentation output).
    pub segmentation_masks: Arc<[SegmentationMask]>,
    /// Anomaly detection results.
    pub anomaly_maps: Arc<[AnomalyMap]>,
    /// Triggered alarm events.
    pub alarms: Arc<[AlarmEvent]>,
    /// Inference latency (end-to-end pipeline) in milliseconds.
    #[serde(
        serialize_with = "serialize_duration_ms",
        deserialize_with = "deserialize_duration_ms"
    )]
    pub inference_latency: Duration,
    /// Optional annotated frame (JPEG with drawn bounding boxes).
    /// Excluded from JSON serialization (binary data).
    #[serde(skip)]
    pub annotated_frame: Option<Bytes>,
}

fn serialize_duration_ms<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64() * 1000.0)
}

fn deserialize_duration_ms<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let ms: f64 = Deserialize::deserialize(d)?;
    Ok(Duration::from_secs_f64(ms / 1000.0))
}

/// Structured analysis core — shared across readers via `Arc`.
///
/// Separates structured detection results from rendering artifacts
/// to allow independent caching and zero-copy sharing between the
/// inference hot path and async annotation/snapshot subsystems.
#[derive(Debug, Clone, Default)]
pub struct AnalysisCore {
    /// Source frame sequence number.
    pub frame_seq: u64,
    /// Object detections.
    pub detections: Arc<[Detection]>,
    /// Classification results.
    pub classifications: Arc<[Classification]>,
    /// Keypoint / pose detections.
    pub keypoint_detections: Arc<[KeypointDetection]>,
    /// Segmentation masks.
    pub segmentation_masks: Arc<[SegmentationMask]>,
    /// Anomaly maps.
    pub anomaly_maps: Arc<[AnomalyMap]>,
    /// Triggered alarms.
    pub alarms: Arc<[AlarmEvent]>,
}

/// Rendering payload stored independently from structured core.
///
/// Kept separate from [`AnalysisCore`] so that rendering can happen
/// asynchronously without blocking the inference hot path.
#[derive(Debug, Clone, Default)]
pub struct RenderArtifact {
    /// JPEG-encoded annotated frame (if annotation is enabled).
    pub annotated_frame: Option<Bytes>,
}
