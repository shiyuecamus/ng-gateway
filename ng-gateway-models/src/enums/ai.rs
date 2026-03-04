use ng_gateway_macros::IntoActiveValue;
use sea_orm::{DeriveActiveEnum, EnumIter, FromJsonQueryResult};
use sea_query::StringLen;
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(32))",
    rename_all = "snake_case"
)]
pub enum ModelTask {
    ObjectDetection,
    Classification,
    Segmentation,
    Ocr,
    AnomalyDetection,
    Custom,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(24))",
    rename_all = "snake_case"
)]
pub enum ModelFormat {
    Onnx,
    Rknn,
    TensorRt,
    OpenVino,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(24))",
    rename_all = "snake_case"
)]
pub enum RuntimeState {
    Registered,
    Loaded,
    Failed,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(16))",
    rename_all = "lowercase"
)]
pub enum AlarmSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(32))",
    rename_all = "snake_case"
)]
pub enum AlarmType {
    ClassDetected,
    CountExceeds,
    ZoneIntrusion,
    LineCrossing,
    AnomalyDetected,
    CustomWasm,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(16))",
    rename_all = "snake_case"
)]
pub enum AlarmEventStatus {
    Open,
    Acked,
    Closed,
}

/// Tensor element data type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TensorDType {
    Float32,
    Float16,
    Int8,
    UInt8,
    Int32,
    Int64,
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

/// Frame encoding format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameFormat {
    /// Raw RGB24 (3 bytes per pixel, row-major).
    Rgb24,
    /// Raw NV12 (Y plane + interleaved UV, common in HW decoders).
    Nv12,
    /// JPEG compressed.
    Jpeg,
    /// H.264 NAL unit (requires decoding).
    H264Nal,
    /// H.265 NAL unit.
    H265Nal,
}

/// WASM module type — determines the host–guest interface contract.
///
/// Each type defines a distinct ABI and data flow:
/// - `FrameTransform` operates on raw pixel data (before inference)
/// - `ResultProcessor` operates on structured analysis results (after inference)
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(32))",
    rename_all = "snake_case"
)]
pub enum AlgorithmModuleType {
    /// Frame-level transform: receives RGB pixels, returns transformed RGB pixels.
    ///
    /// Guest exports:
    /// ```text
    /// fn transform(input_ptr: i32, input_len: i32) -> i32
    /// fn alloc(size: i32) -> i32
    /// fn get_output_len() -> i32
    /// ```
    ///
    /// Input JSON (written to WASM memory):
    /// ```json
    /// { "width": 1920, "height": 1080, "pixels_ptr": 65536, "pixels_len": 6220800, "config": {} }
    /// ```
    ///
    /// Output JSON (read from WASM memory):
    /// ```json
    /// { "width": 1920, "height": 1080, "pixels_ptr": 7000000, "pixels_len": 6220800 }
    /// ```
    ///
    /// The pixel data is passed via WASM shared memory (zero-copy within the sandbox).
    FrameTransform,

    /// Result-level processor: receives analysis results JSON, returns modified results.
    ///
    /// Guest exports:
    /// ```text
    /// fn process(input_ptr: i32, input_len: i32) -> i32
    /// fn alloc(size: i32) -> i32
    /// fn get_output_len() -> i32
    /// ```
    ///
    /// Input JSON:
    /// ```json
    /// {
    ///   "detections": [...],
    ///   "classifications": [...],
    ///   "frame_width": 1920,
    ///   "frame_height": 1080,
    ///   "config": {}
    /// }
    /// ```
    ///
    /// Output JSON:
    /// ```json
    /// {
    ///   "detections": [...],
    ///   "classifications": [...],
    ///   "custom_outputs": [["key", value]]
    /// }
    /// ```
    ResultProcessor,
}

/// Frame sampling strategy.
///
/// Stored as a JSON column in the database. SeaORM handles
/// serialization via [`FromJsonQueryResult`] + [`IntoActiveValue`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromJsonQueryResult, IntoActiveValue)]
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

/// Direction constraint for line-crossing detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrossingDirection {
    /// Only trigger when crossing from the left side of the line to the right.
    LeftToRight,
    /// Only trigger when crossing from the right side of the line to the left.
    RightToLeft,
    /// Trigger on any crossing direction.
    Any,
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

/// Tracker algorithm selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerAlgorithm {
    /// Simple IoU-based tracker (SORT variant).
    Sort,
    /// Deep SORT with appearance features.
    DeepSort { reid_model_id: String },
}
