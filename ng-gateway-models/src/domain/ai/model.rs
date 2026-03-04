use crate::{
    domain::common::{PageParams, TimeRangeParams},
    entities::ai::{
        model::{
            ActiveModel, Entity as ModelEntity, Labels, Model as ModelModel, TensorDesc,
            TensorDescs,
        },
        pipeline::{PostProcessorConfig, PreProcessorConfig},
    },
    enums::{
        ai::{ModelFormat, ModelTask, PostProcessorType},
        common::Status,
    },
    initializer::SeedableTrait,
};
use sea_orm::{
    prelude::DateTimeUtc, DeriveIntoActiveModel, DerivePartialModel, FromQueryResult,
    IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use validator::Validate;

/// Model metadata stored in the registry.
#[derive(Debug, Clone, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::ModelModel as ModelTrait>::Entity")]
pub struct ModelInfo {
    /// Unique model identifier (derived from filename by default).
    pub id: i32,
    /// Model key.
    pub model_key: String,
    /// Human-readable name.
    pub name: String,
    /// Model version string.
    #[serde(default = "ModelInfo::default_model_version")]
    pub version: String,
    /// Model task type (detection, classification, etc.).
    pub task: ModelTask,
    /// Model format.
    pub format: ModelFormat,
    /// Model file path (relative to models directory).
    pub path: String,
    /// Input tensor descriptions.
    pub inputs: Option<TensorDescs>,
    /// Output tensor descriptions.
    pub outputs: Option<TensorDescs>,
    /// Class labels (if applicable).
    pub labels: Option<Labels>,
    /// Optional default preprocess override for this model.
    pub default_preprocess: Option<PreProcessorConfig>,
    /// Optional default postprocess override for this model.
    pub default_postprocess: Option<PostProcessorConfig>,
    /// File size in bytes.
    pub size: u64,
    /// Checksum of the model file.
    pub checksum: String,
    /// Created at timestamp.
    pub created_at: DateTimeUtc,
    /// Updated at timestamp.
    pub updated_at: DateTimeUtc,
}

impl From<ModelModel> for ModelInfo {
    fn from(model: ModelModel) -> Self {
        Self {
            id: model.id,
            model_key: model.model_key,
            name: model.name,
            version: model.version,
            task: model.task,
            format: model.format,
            path: model.path,
            inputs: model.inputs,
            outputs: model.outputs,
            labels: model.labels,
            default_preprocess: model.default_preprocess,
            default_postprocess: model.default_postprocess,
            size: model.size,
            checksum: model.checksum,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl ModelInfo {
    /// Infer the expected input size `(width, height)` from the first input tensor.
    ///
    /// Assumes NCHW layout: `[batch, channels, height, width]`.
    /// Returns `None` if the shape is invalid or has dynamic dimensions.
    pub fn input_size(&self) -> Option<(u32, u32)> {
        self.inputs.as_ref()?.0.first().and_then(|t| {
            if t.shape.len() == 4 && t.shape[2] > 0 && t.shape[3] > 0 {
                Some((t.shape[3] as u32, t.shape[2] as u32))
            } else {
                None
            }
        })
    }

    /// Check if the model output shape matches YOLOv8 format `[1, C, N]` where C < N.
    pub fn is_yolov8_output_format(&self) -> bool {
        self.outputs.as_ref().is_some_and(|outputs| {
            outputs.0.first().is_some_and(|t| {
                t.shape.len() == 3 && t.shape[1] > 0 && t.shape[2] > 0 && t.shape[1] < t.shape[2]
            })
        })
    }

    /// Check if the model output shape matches YOLOv5 format `[1, N, C]` where N > C.
    pub fn is_yolov5_output_format(&self) -> bool {
        self.outputs.as_ref().is_some_and(|outputs| {
            outputs.0.first().is_some_and(|t| {
                t.shape.len() == 3 && t.shape[1] > 0 && t.shape[2] > 0 && t.shape[1] > t.shape[2]
            })
        })
    }

    fn default_model_version() -> String {
        "1.0.0".to_string()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewModel {
    pub model_key: String,
    pub name: String,
    #[serde(default = "NewModel::default_version")]
    pub version: String,
    pub task: ModelTask,
    pub format: ModelFormat,
    pub path: String,
    pub labels: Option<Labels>,
    pub default_preprocess: Option<PreProcessorConfig>,
    pub default_postprocess: Option<PostProcessorConfig>,
    pub inputs: TensorDescs,
    pub outputs: TensorDescs,
    pub size: u64,
    pub checksum: String,
}

impl NewModel {
    fn default_version() -> String {
        "1.0.0".to_string()
    }
}

impl SeedableTrait for NewModel {
    type ActiveModel = ActiveModel;
    type Entity = ModelEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateModel {
    pub id: i32,
    pub model_key: String,
    pub name: String,
    pub version: String,
    pub task: ModelTask,
    pub format: ModelFormat,
    pub path: String,
    pub labels: Option<Option<Labels>>,
    pub default_preprocess: Option<Option<PreProcessorConfig>>,
    pub default_postprocess: Option<Option<PostProcessorConfig>>,
    pub inputs: Option<Option<TensorDescs>>,
    pub outputs: Option<Option<TensorDescs>>,
    pub size: u64,
    pub checksum: String,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
pub struct ChangeModelStatus {
    pub id: i32,
    pub status: Status,
}

/// Query parameters for paginating model records.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ModelPageParams {
    /// Fuzzy filter by model name.
    pub name: Option<String>,
    /// Exact filter by model task.
    pub task: Option<ModelTask>,
    /// Exact filter by model format.
    pub format: Option<ModelFormat>,
    /// Exact filter by model status.
    pub status: Option<Status>,
    /// Pagination controls.
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Created-at range filter.
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

// ── Probe + Install types ────────────────────────────────────────────

/// Probed metadata extracted from a model artifact via runtime session.
///
/// All fields are derived from the model binary itself — no caller-supplied
/// metadata is trusted. The prober loads a temporary runtime session to
/// extract precise tensor information including resolved dynamic dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProbeInfo {
    /// Detected model format (ONNX, RKNN, etc.).
    pub format: ModelFormat,
    /// Input tensor descriptions extracted from runtime session.
    pub inputs: Vec<TensorDesc>,
    /// Output tensor descriptions extracted from runtime session.
    pub outputs: Vec<TensorDesc>,
    /// Inferred model task type based on output tensor analysis.
    pub inferred_task: Option<ModelTask>,
    /// Detected output variant (YOLOv5/v8/Pose/Generic).
    pub inferred_variant: Option<ModelVariant>,
    /// Recommended postprocessor type based on output analysis.
    pub recommended_postprocessor: Option<PostProcessorType>,
    /// Recommended preprocessing config based on input analysis.
    pub recommended_preprocess: Option<PreProcessorConfig>,
    /// Model producer/framework information (ONNX-specific).
    pub producer: Option<ProducerInfo>,
    /// ONNX opset version (ONNX only).
    pub opset_version: Option<i64>,
    /// Target hardware platform (RKNN only: RK3588, RK3566, etc.).
    pub target_platform: Option<String>,
    /// Quantization type (RKNN: INT8/FP16; ONNX: from metadata).
    pub quantization: Option<String>,
    /// Custom metadata properties embedded in the model file.
    pub metadata_props: HashMap<String, String>,
    /// Class labels extracted from model metadata (if embedded).
    pub labels: Option<Vec<String>>,
    /// File size in bytes.
    pub size: u64,
    /// SHA-256 checksum (hex, lowercase).
    pub checksum: String,
}

/// Model output variant detected from tensor shape analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelVariant {
    /// YOLOv5 detection format: `[1, N, C]` where N > C.
    YoloV5,
    /// YOLOv8 detection format: `[1, C, N]` where C < N.
    YoloV8,
    /// YOLOv8-Pose format: detection + keypoint channels.
    YoloV8Pose,
    /// Generic / unrecognised output layout.
    Generic,
}

/// Model producer/framework information extracted from ONNX protobuf.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProducerInfo {
    /// Producer framework name (e.g. "pytorch", "onnxruntime").
    pub name: String,
    /// Producer framework version.
    pub version: Option<String>,
    /// Model version integer from ONNX ModelProto.
    pub model_version: Option<i64>,
    /// Documentation string from ONNX ModelProto.
    pub doc_string: Option<String>,
    /// Model domain from ONNX ModelProto.
    pub domain: Option<String>,
}

/// User-supplied metadata to complement probe results during install.
///
/// Fields here can override probe-inferred values. Unset fields fall
/// back to probe-extracted or default values.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ModelInstallRequest {
    /// Override the model name (default: filename stem).
    pub name: Option<String>,
    /// Override the inferred task type.
    pub task: Option<ModelTask>,
    /// Override the model version string.
    pub version: Option<String>,
    /// User-supplied labels (overrides embedded labels if any).
    pub labels: Option<Vec<String>>,
    /// Override default preprocessing config.
    pub default_preprocess: Option<PreProcessorConfig>,
    /// Override default postprocessing config.
    pub default_postprocess: Option<PostProcessorConfig>,
}
