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
    /// Optional multiple ROI regions (applied before inference).
    ///
    /// When non-empty, each region is processed independently and results
    /// are merged back to full-frame coordinates.
    #[serde(default)]
    pub roi_regions: Vec<RegionOfInterest>,
    /// Ordered list of processing stages.
    pub stages: Vec<StageConfig>,
    /// Alarm rules (post-processing triggers).
    pub alarm_rules: Vec<AlarmRule>,
    /// Annotation rendering configuration.
    #[serde(default)]
    pub annotation: AnnotationConfig,
}

/// Create/replace pipeline binding request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineUpsertRequest {
    /// Target channel ID where pipeline is bound.
    pub channel_id: i32,
    /// Full pipeline configuration payload.
    pub config: PipelineConfig,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Resize strategy.
    pub resize_mode: Option<ResizeMode>,
    /// Normalization preset or custom values.
    pub normalization: Option<NormalizationConfig>,
    /// Channel order.
    pub channel_order: Option<ChannelOrder>,
    /// Letterbox padding fill value (0-255, default 114).
    pub pad_value: Option<u8>,
}

/// Normalization configuration for preprocessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizationConfig {
    /// Preset name.
    pub preset: Option<NormalizationPreset>,
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
    pub r#type: Option<PostProcessorType>,
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
    /// NMS algorithm variant.
    pub nms_variant: Option<NmsVariantConfig>,
    /// Sigma parameter for Soft-NMS Gaussian decay (only when nms_variant = "soft").
    pub soft_nms_sigma: Option<f32>,
    /// Minimum prediction count to enable parallel candidate generation.
    pub detection_parallel_threshold: Option<usize>,
    /// Candidate pre-screen multiplier before NMS.
    pub nms_prescreen_multiplier: Option<usize>,
    /// Class-count threshold for classification small-input fast path.
    pub classification_small_class_fast_path: Option<usize>,
    /// Minimum pixel count to enable segmentation argmax parallelism.
    pub segmentation_parallel_min_pixels: Option<usize>,
}

/// Supported preprocessing resize modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeMode {
    Letterbox,
    CenterCrop,
    DirectResize,
}

/// Input channel ordering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelOrder {
    Rgb,
    Bgr,
}

/// Supported postprocessor override types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostProcessorType {
    YoloV8Detection,
    YoloV5Detection,
    Classification,
    Segmentation,
    YoloV8Pose,
    AnomalyDetection,
    Passthrough,
}

/// Supported normalization presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalizationPreset {
    Yolo,
    Imagenet,
    Symmetric,
    Custom,
}

/// Configurable NMS variant type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NmsVariantConfig {
    Classic,
    Soft,
    Diou,
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
    /// Draw segmentation overlays.
    #[serde(default = "bool_true")]
    pub draw_segmentation: bool,
    /// Segmentation overlay alpha in range `[0.0, 1.0]`.
    #[serde(default = "default_segmentation_alpha")]
    pub segmentation_alpha: f32,
    /// Background class index to ignore for segmentation overlay.
    ///
    /// Set `None` to render all classes.
    #[serde(default = "default_segmentation_background_class")]
    pub segmentation_background_class: Option<u8>,
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
    /// Behavior when annotation queue is full.
    #[serde(default)]
    pub queue_overflow_strategy: AnnotationQueueOverflowStrategy,
    /// Max enqueue wait time in milliseconds when strategy is `wait_for_slot`.
    #[serde(default = "default_annotation_enqueue_timeout_ms")]
    pub enqueue_timeout_ms: u64,
}

impl Default for AnnotationConfig {
    fn default() -> Self {
        Self {
            draw_bboxes: true,
            draw_labels: true,
            draw_confidence: true,
            draw_track_ids: true,
            draw_segmentation: true,
            segmentation_alpha: 0.4,
            segmentation_background_class: Some(0),
            line_thickness: 2,
            font_scale: 0.6,
            jpeg_quality: 75,
            max_output_dimension: Some(1280),
            color_palette: default_color_palette(),
            queue_overflow_strategy: AnnotationQueueOverflowStrategy::DropNewest,
            enqueue_timeout_ms: default_annotation_enqueue_timeout_ms(),
        }
    }
}

/// Queue overflow strategy for async frame annotation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationQueueOverflowStrategy {
    /// Drop the newest annotation request when queue is full (hot path friendly).
    #[default]
    DropNewest,
    /// Wait for a free queue slot up to `enqueue_timeout_ms`.
    WaitForSlot,
}

/// Pipeline validation result for DAG/order constraints.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PipelineValidationReport {
    /// Whether the pipeline satisfies all mandatory constraints.
    pub valid: bool,
    /// Hard validation errors that must be fixed before execution.
    pub errors: Vec<String>,
    /// Non-blocking warnings that may impact behaviour or quality.
    pub warnings: Vec<String>,
}

impl PipelineValidationReport {
    /// Create a successful validation report.
    #[inline]
    pub fn ok() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[inline]
    fn push_error(&mut self, message: String) {
        self.valid = false;
        self.errors.push(message);
    }

    #[inline]
    fn push_warning(&mut self, message: String) {
        self.warnings.push(message);
    }
}

impl PipelineConfig {
    /// Validate pipeline stage ordering and DAG-like constraints.
    ///
    /// Rules:
    /// - At least one `Inference` stage is required.
    /// - `FrameTransform` must be before any `Inference`.
    /// - `Tracker` must appear after `Inference` and at most once.
    /// - `ResultProcessor` must appear after `Inference`.
    /// - `Inference` cannot appear after `Tracker`/`ResultProcessor`.
    /// - `AlarmCondition::LineCrossing` requires a `Tracker` stage.
    pub fn validate_dag(&self) -> PipelineValidationReport {
        let mut report = PipelineValidationReport::ok();

        if let Some(roi) = self.roi {
            if !roi.is_valid() {
                report.push_error(
                    "pipeline.roi is invalid (expected normalized [0,1] bounds with min < max)"
                        .to_string(),
                );
            }
        }
        for (idx, roi) in self.roi_regions.iter().enumerate() {
            if !roi.is_valid() {
                report.push_error(format!(
                    "pipeline.roi_regions[{idx}] is invalid (expected normalized [0,1] bounds with min < max)"
                ));
            }
        }

        if self.stages.is_empty() {
            report.push_warning(
                "pipeline has no stages; no AI inference will be executed".to_string(),
            );
        }

        let mut inference_count = 0usize;
        let mut has_seen_inference = false;
        let mut has_seen_tracker = false;
        let mut has_seen_result_processor = false;
        let mut tracker_count = 0usize;

        for (idx, stage) in self.stages.iter().enumerate() {
            let stage_no = idx + 1;
            match stage {
                StageConfig::FrameTransform { .. } => {
                    if has_seen_inference {
                        report.push_error(format!(
                            "stage #{stage_no}: frame_transform must appear before any inference stage"
                        ));
                    }
                    if has_seen_tracker {
                        report.push_error(format!(
                            "stage #{stage_no}: frame_transform cannot appear after tracker"
                        ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                            "stage #{stage_no}: frame_transform cannot appear after result_processor"
                        ));
                    }
                }
                StageConfig::Inference { .. } => {
                    if has_seen_tracker {
                        report.push_error(format!(
                            "stage #{stage_no}: inference cannot appear after tracker"
                        ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                            "stage #{stage_no}: inference cannot appear after result_processor"
                        ));
                    }
                    has_seen_inference = true;
                    inference_count += 1;
                }
                StageConfig::Tracker { .. } => {
                    if !has_seen_inference {
                        report.push_error(format!(
                            "stage #{stage_no}: tracker must appear after at least one inference stage"
                        ));
                    }
                    if has_seen_result_processor {
                        report.push_error(format!(
                            "stage #{stage_no}: tracker cannot appear after result_processor"
                        ));
                    }
                    tracker_count += 1;
                    if tracker_count > 1 {
                        report.push_error(format!(
                            "stage #{stage_no}: only one tracker stage is allowed per pipeline"
                        ));
                    }
                    has_seen_tracker = true;
                }
                StageConfig::ResultProcessor { .. } => {
                    if !has_seen_inference {
                        report.push_error(format!(
                            "stage #{stage_no}: result_processor must appear after at least one inference stage"
                        ));
                    }
                    has_seen_result_processor = true;
                }
            }
        }

        if inference_count == 0 {
            report.push_error("pipeline must contain at least one inference stage".to_string());
        }

        let has_line_crossing_alarm = self.alarm_rules.iter().any(|rule| {
            matches!(
                rule.condition,
                AlarmCondition::LineCrossing {
                    line: _,
                    class: _,
                    direction: _
                }
            )
        });
        if has_line_crossing_alarm && !has_seen_tracker {
            report.push_error(
                "line_crossing alarm requires a tracker stage in the pipeline".to_string(),
            );
        }

        report
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

fn default_segmentation_alpha() -> f32 {
    0.4
}

fn default_segmentation_background_class() -> Option<u8> {
    Some(0)
}

fn default_annotation_enqueue_timeout_ms() -> u64 {
    5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::types::{AlarmSeverity, PipelineId};

    fn make_pipeline(stages: Vec<StageConfig>, alarm_rules: Vec<AlarmRule>) -> PipelineConfig {
        PipelineConfig {
            id: PipelineId::new("p1"),
            name: "test".to_string(),
            sampling: SamplingStrategy::EveryFrame,
            roi: None,
            roi_regions: vec![],
            stages,
            alarm_rules,
            annotation: AnnotationConfig::default(),
        }
    }

    #[test]
    fn validate_dag_accepts_multi_stage_pipeline() {
        let pipeline = make_pipeline(
            vec![
                StageConfig::FrameTransform {
                    module_id: "ft_1".to_string(),
                    config: serde_json::json!({}),
                },
                StageConfig::Inference {
                    model_id: "detector_a".to_string(),
                    confidence_threshold: 0.5,
                    nms_iou_threshold: Some(0.45),
                    input_size: Some((640, 640)),
                    preprocess: None,
                    postprocess: None,
                },
                StageConfig::Inference {
                    model_id: "detector_b".to_string(),
                    confidence_threshold: 0.5,
                    nms_iou_threshold: Some(0.45),
                    input_size: Some((640, 640)),
                    preprocess: None,
                    postprocess: None,
                },
                StageConfig::Tracker {
                    algorithm: TrackerAlgorithm::Sort,
                    max_age: 30,
                },
                StageConfig::ResultProcessor {
                    module_id: "rp_1".to_string(),
                    config: serde_json::json!({}),
                },
            ],
            vec![],
        );

        let report = pipeline.validate_dag();
        assert!(report.valid, "expected valid DAG: {:?}", report.errors);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn validate_dag_rejects_frame_transform_after_inference() {
        let pipeline = make_pipeline(
            vec![
                StageConfig::Inference {
                    model_id: "detector_a".to_string(),
                    confidence_threshold: 0.5,
                    nms_iou_threshold: Some(0.45),
                    input_size: Some((640, 640)),
                    preprocess: None,
                    postprocess: None,
                },
                StageConfig::FrameTransform {
                    module_id: "ft_1".to_string(),
                    config: serde_json::json!({}),
                },
            ],
            vec![],
        );

        let report = pipeline.validate_dag();
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("frame_transform must appear before any inference")));
    }

    #[test]
    fn validate_dag_requires_tracker_for_line_crossing_alarm() {
        let pipeline = make_pipeline(
            vec![StageConfig::Inference {
                model_id: "detector_a".to_string(),
                confidence_threshold: 0.5,
                nms_iou_threshold: Some(0.45),
                input_size: Some((640, 640)),
                preprocess: None,
                postprocess: None,
            }],
            vec![AlarmRule {
                name: "line".to_string(),
                condition: AlarmCondition::LineCrossing {
                    line: [(0.1, 0.2), (0.8, 0.2)],
                    class: Some("person".to_string()),
                    direction: Some(CrossingDirection::Any),
                },
                severity: AlarmSeverity::Warning,
                cooldown_secs: 60,
                min_duration_secs: None,
            }],
        );

        let report = pipeline.validate_dag();
        assert!(!report.valid);
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("line_crossing alarm requires a tracker stage")));
    }
}
