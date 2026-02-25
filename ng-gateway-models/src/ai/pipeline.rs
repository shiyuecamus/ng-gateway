//! Pipeline configuration for AI analysis pipelines.

use super::types::{AlarmSeverity, PipelineId, RegionOfInterest};
use serde::{Deserialize, Serialize};

/// Pipeline configuration stored in database and managed via API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    /// Pipeline unique identifier.
    pub id: PipelineId,
    /// Human-readable name.
    pub name: String,
    /// Frame sampling strategy.
    pub sampling: SamplingStrategy,
    /// Optional ROI (applied before inference).
    pub roi: Option<RegionOfInterest>,
    /// Ordered list of processing stages.
    pub stages: Vec<StageConfig>,
    /// Alarm rules (post-processing triggers).
    pub alarm_rules: Vec<AlarmRule>,
    /// Annotation rendering configuration.
    #[serde(default)]
    pub annotation: AnnotationConfig,
}

/// Frame sampling strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SamplingStrategy {
    /// Process every N-th frame.
    FixedInterval { every_n_frames: u32 },
    /// Process at a target FPS (adaptive skip).
    TargetFps { fps: f32 },
    /// Process only key frames (I-frames from H.264/H.265).
    KeyFrameOnly,
    /// Process every frame (maximum load).
    EveryFrame,
}

impl Default for SamplingStrategy {
    fn default() -> Self {
        Self::TargetFps { fps: 5.0 }
    }
}

/// A single processing stage in the pipeline.
///
/// Stages are executed in order. The pipeline enforces:
/// - `FrameTransform` must come before `Inference`
/// - `Tracker` must follow an `Inference`
/// - `ResultProcessor` must come after `Inference`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageConfig {
    /// Custom WASM frame-level transform (before inference).
    FrameTransform {
        /// WASM module identifier.
        module_id: String,
        /// JSON configuration passed to the WASM module.
        #[serde(default)]
        config: serde_json::Value,
    },

    /// Built-in model inference stage.
    ///
    /// Internally chains: PreProcessor(config) → ONNX Runtime → PostProcessor(config).
    /// Users only configure parameters; processor selection is automatic based on
    /// model task + output shape, or manually overridden via config.
    Inference {
        /// Model identifier in the registry.
        model_id: String,
        /// Confidence threshold for filtering results.
        #[serde(default = "default_confidence_threshold")]
        confidence_threshold: f32,
        /// NMS IoU threshold (for detection models).
        nms_iou_threshold: Option<f32>,
        /// Target input size override (uses model metadata if not set).
        input_size: Option<(u32, u32)>,
        /// Preprocessing configuration override (optional).
        /// Boxed to keep enum variant size small (see clippy::large_enum_variant).
        preprocess: Option<Box<PreProcessorConfig>>,
        /// Postprocessing configuration override (optional).
        /// Boxed to keep enum variant size small (see clippy::large_enum_variant).
        postprocess: Option<Box<PostProcessorConfig>>,
    },

    /// Built-in object tracker stage (applied after detection).
    Tracker {
        /// Tracker algorithm.
        algorithm: TrackerAlgorithm,
        /// Maximum age (frames) before dropping a track.
        #[serde(default = "default_tracker_max_age")]
        max_age: u32,
    },

    /// Custom WASM result-level processor (after inference/tracker).
    ResultProcessor {
        /// WASM module identifier.
        module_id: String,
        /// JSON configuration passed to the WASM module.
        #[serde(default)]
        config: serde_json::Value,
    },
}

/// Tracker algorithm selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerAlgorithm {
    /// Simple IoU-based tracker (SORT variant).
    Sort,
    /// Deep SORT with appearance features.
    DeepSort { reid_model_id: String },
}

/// Preprocessing configuration — Inference stage internal, config-driven.
///
/// Users select resize mode and normalization preset in the Pipeline UI.
/// The engine maps these to the appropriate internal [`PreProcessor`] implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreProcessorConfig {
    /// Resize strategy: "letterbox", "center_crop", "direct_resize".
    pub resize_mode: Option<String>,
    /// Normalization preset or custom values.
    pub normalization: Option<NormalizationConfig>,
    /// Channel order: "rgb" (default) or "bgr".
    pub channel_order: Option<String>,
    /// Letterbox padding fill value (0-255, default 114).
    pub pad_value: Option<u8>,
}

/// Normalization configuration for preprocessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    /// Preset name: "yolo", "imagenet", "symmetric", or "custom".
    pub preset: Option<String>,
    /// Custom mean `[R, G, B]` (only when preset = "custom").
    pub mean: Option<[f32; 3]>,
    /// Custom std `[R, G, B]` (only when preset = "custom").
    pub std: Option<[f32; 3]>,
}

/// Postprocessing configuration — Inference stage internal, config-driven.
///
/// The engine auto-selects the postprocessor based on model task and output shape.
/// Users can override or fine-tune parameters here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostProcessorConfig {
    /// Force a specific postprocessor type (overrides auto-detection).
    /// Values: "yolov8_detection", "yolov5_detection", "classification",
    ///         "segmentation", "yolov8_pose", "anomaly_detection", "passthrough".
    pub r#type: Option<String>,
    /// Top-K for classification models.
    pub top_k: Option<usize>,
    /// Whether to apply softmax (classification).
    pub apply_softmax: Option<bool>,
    /// Max detections after NMS (detection models).
    pub max_detections: Option<usize>,
    /// Number of keypoints per detection (pose models, default: 17 for COCO).
    pub num_keypoints: Option<usize>,
    /// Anomaly score threshold (anomaly detection models).
    pub anomaly_threshold: Option<f32>,
    /// NMS algorithm variant: "classic" (default), "soft", "diou".
    pub nms_variant: Option<String>,
    /// Sigma parameter for Soft-NMS Gaussian decay (only when nms_variant = "soft").
    pub soft_nms_sigma: Option<f32>,
}

/// Alarm rule applied to analysis results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlarmRule {
    /// Rule name.
    pub name: String,
    /// Condition type.
    pub condition: AlarmCondition,
    /// Alarm severity when triggered.
    pub severity: AlarmSeverity,
    /// Cooldown period (seconds) between consecutive alarms of this rule.
    #[serde(default = "default_alarm_cooldown")]
    pub cooldown_secs: u32,
    /// Minimum duration (seconds) the condition must persist before triggering.
    ///
    /// Reduces false positives from transient detections. The alarm evaluator
    /// tracks condition state over time and only fires after this duration
    /// has elapsed continuously.
    #[serde(default)]
    pub min_duration_secs: Option<u32>,
}

/// Alarm triggering condition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlarmCondition {
    /// Trigger when a specific class is detected.
    ClassDetected {
        class: String,
        #[serde(default = "default_confidence_threshold")]
        min_confidence: f32,
    },
    /// Trigger when object count exceeds threshold.
    CountExceeds {
        class: Option<String>,
        threshold: u32,
    },
    /// Trigger when an object enters a zone (polygon vertices).
    ZoneIntrusion {
        zone: Vec<(f32, f32)>,
        class: Option<String>,
    },
    /// Trigger when an object's track crosses a defined line segment.
    ///
    /// Requires a Tracker stage in the pipeline to provide trajectory data.
    /// The crossing direction can be constrained (e.g., only left-to-right).
    LineCrossing {
        /// Line segment: start point `(x1, y1)` and end point `(x2, y2)`.
        /// Coordinates are normalized `[0.0, 1.0]`.
        line: [(f32, f32); 2],
        /// Optional class filter.
        class: Option<String>,
        /// Crossing direction constraint (if `None`, any direction triggers).
        direction: Option<CrossingDirection>,
    },
    /// Trigger when an anomaly detection score exceeds a threshold.
    AnomalyDetected {
        /// Anomaly score threshold `[0.0, 1.0]`.
        #[serde(default = "default_confidence_threshold")]
        min_score: f32,
    },
    /// Trigger based on custom WASM evaluator.
    CustomWasm {
        module_id: String,
        #[serde(default)]
        config: serde_json::Value,
    },
}

/// Direction constraint for line-crossing detection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossingDirection {
    /// Only trigger when crossing from the left side of the line to the right.
    LeftToRight,
    /// Only trigger when crossing from the right side of the line to the left.
    RightToLeft,
    /// Trigger on any crossing direction.
    Any,
}

/// Annotation rendering configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationConfig {
    /// Draw bounding boxes.
    #[serde(default = "bool_true")]
    pub draw_bboxes: bool,
    /// Draw class labels on boxes.
    #[serde(default = "bool_true")]
    pub draw_labels: bool,
    /// Draw confidence scores.
    #[serde(default = "bool_true")]
    pub draw_confidence: bool,
    /// Draw tracking IDs.
    #[serde(default = "bool_true")]
    pub draw_track_ids: bool,
    /// Bounding box line thickness (pixels).
    #[serde(default = "default_line_thickness")]
    pub line_thickness: u32,
    /// Font scale for labels.
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    /// JPEG output quality (1-100).
    #[serde(default = "default_jpeg_quality")]
    pub jpeg_quality: u8,
    /// Output image max dimension (downscale if larger, for bandwidth).
    pub max_output_dimension: Option<u32>,
    /// Color palette for classes (hex colors, cycles if fewer than classes).
    #[serde(default = "default_color_palette")]
    pub color_palette: Vec<String>,
}

impl Default for AnnotationConfig {
    fn default() -> Self {
        Self {
            draw_bboxes: true,
            draw_labels: true,
            draw_confidence: true,
            draw_track_ids: true,
            line_thickness: 2,
            font_scale: 0.6,
            jpeg_quality: 75,
            max_output_dimension: Some(1280),
            color_palette: default_color_palette(),
        }
    }
}

// ── Serde default helpers ──────────────────────────────────────────

fn bool_true() -> bool {
    true
}
fn default_confidence_threshold() -> f32 {
    0.5
}
fn default_tracker_max_age() -> u32 {
    30
}
fn default_alarm_cooldown() -> u32 {
    60
}
fn default_line_thickness() -> u32 {
    2
}
fn default_font_scale() -> f32 {
    0.6
}
fn default_jpeg_quality() -> u8 {
    75
}
fn default_color_palette() -> Vec<String> {
    vec![
        "#FF3838".into(),
        "#FF9D97".into(),
        "#FF701F".into(),
        "#FFB21D".into(),
        "#CFD231".into(),
        "#48F90A".into(),
        "#92CC17".into(),
        "#3DDB86".into(),
        "#1A9334".into(),
        "#00D4BB".into(),
        "#2C99A8".into(),
        "#00C2FF".into(),
        "#344593".into(),
        "#6473FF".into(),
        "#0018EC".into(),
        "#8438FF".into(),
        "#520085".into(),
        "#CB38FF".into(),
        "#FF95C8".into(),
        "#FF37C7".into(),
    ]
}
