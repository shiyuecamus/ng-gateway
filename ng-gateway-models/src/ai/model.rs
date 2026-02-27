//! Model metadata types.
//!
//! Describes AI model properties including format, task type, tensor
//! descriptions, and class labels. These types are always available
//! (no feature gating) so camera drivers and API handlers can inspect them.

use super::pipeline::{PostProcessorConfig, PreProcessorConfig};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Model metadata stored in the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Unique model identifier (derived from filename by default).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Model version string.
    pub version: String,
    /// Model format.
    pub format: ModelFormat,
    /// Model file path (relative to models directory).
    pub path: PathBuf,
    /// Input tensor descriptions.
    pub inputs: Vec<TensorDesc>,
    /// Output tensor descriptions.
    pub outputs: Vec<TensorDesc>,
    /// Model task type (detection, classification, etc.).
    pub task: ModelTask,
    /// Class labels (if applicable).
    pub labels: Vec<String>,
    /// Optional default preprocess override for this model.
    pub default_preprocess: Option<PreProcessorConfig>,
    /// Optional default postprocess override for this model.
    pub default_postprocess: Option<PostProcessorConfig>,
    /// Whether the model is currently loaded in memory.
    #[serde(default)]
    pub loaded: bool,
    /// File size in bytes.
    pub file_size: u64,
}

/// Metadata provided when uploading a new ONNX model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUploadMetadata {
    /// Model identifier (filename stem). Must be unique.
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Model version string.
    #[serde(default = "default_model_version")]
    pub version: String,
    /// Model task type.
    pub task: ModelTask,
    /// Optional class labels.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Optional default preprocess override.
    pub default_preprocess: Option<PreProcessorConfig>,
    /// Optional default postprocess override.
    pub default_postprocess: Option<PostProcessorConfig>,
}

/// Mutable model update request for PUT `/models/{id}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelUpdateRequest {
    /// Human-readable model name.
    pub name: Option<String>,
    /// Model version string.
    pub version: Option<String>,
    /// Model task type.
    pub task: Option<ModelTask>,
    /// Replace labels.
    pub labels: Option<Vec<String>>,
    /// Default preprocess override.
    pub default_preprocess: Option<PreProcessorConfig>,
    /// Default postprocess override.
    pub default_postprocess: Option<PostProcessorConfig>,
}

fn default_model_version() -> String {
    "1.0.0".to_string()
}

/// Supported model formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelFormat {
    Onnx,
    TensorRt,
    OpenVino,
}

/// Model task type — determines which pre/post processor profile to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTask {
    ObjectDetection,
    Classification,
    Segmentation,
    Ocr,
    AnomalyDetection,
    Custom,
}

/// Tensor shape and data type description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TensorDesc {
    /// Tensor name.
    pub name: String,
    /// Tensor shape (e.g., `[1, 3, 640, 640]` for YOLO input).
    /// Negative values indicate dynamic dimensions.
    pub shape: Vec<i64>,
    /// Element data type.
    pub dtype: TensorDType,
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

impl ModelInfo {
    /// Infer the expected input size `(width, height)` from the first input tensor.
    ///
    /// Assumes NCHW layout: `[batch, channels, height, width]`.
    /// Returns `None` if the shape is invalid or has dynamic dimensions.
    pub fn input_size(&self) -> Option<(u32, u32)> {
        self.inputs.first().and_then(|t| {
            if t.shape.len() == 4 && t.shape[2] > 0 && t.shape[3] > 0 {
                Some((t.shape[3] as u32, t.shape[2] as u32))
            } else {
                None
            }
        })
    }

    /// Check if the model output shape matches YOLOv8 format `[1, C, N]` where C < N.
    pub fn is_yolov8_output_format(&self) -> bool {
        self.outputs.first().is_some_and(|t| {
            t.shape.len() == 3 && t.shape[1] > 0 && t.shape[2] > 0 && t.shape[1] < t.shape[2]
        })
    }

    /// Check if the model output shape matches YOLOv5 format `[1, N, C]` where N > C.
    pub fn is_yolov5_output_format(&self) -> bool {
        self.outputs.first().is_some_and(|t| {
            t.shape.len() == 3 && t.shape[1] > 0 && t.shape[2] > 0 && t.shape[1] > t.shape[2]
        })
    }
}
