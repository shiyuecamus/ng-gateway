use std::sync::Arc;

use crate::{
    domain::ai::types::{BoundingBox, Classification, Detection},
    domain::common::{PageParams, TimeRangeParams},
    entities::ai::algorithm::{ActiveModel, Entity as AlgorithmEntity, Model as AlgorithmModel},
    enums::{ai::AlgorithmModuleType, common::Status},
    initializer::SeedableTrait,
};
use sea_orm::{
    entity::prelude::*, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Metadata for a registered WASM algorithm module.
#[derive(Debug, Serialize, Clone, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::AlgorithmModel as ModelTrait>::Entity")]
pub struct AlgorithmInfo {
    /// App id
    pub id: i32,
    /// Algorithm key
    pub key: String,
    /// Algorithm name
    pub name: String,
    /// Algorithm description
    pub description: Option<String>,
    /// Algorithm version
    pub version: String,
    /// Algorithm module type
    pub module_type: AlgorithmModuleType,
    /// Algorithm artifact path
    pub path: String,
    /// JSON Schema for the `config` parameter (for UI form generation).
    /// `None` means the algorithm accepts arbitrary config or no config.
    pub config_schema: Option<serde_json::Value>,
    /// File size in bytes
    pub size: u64,
    /// Algorithm status
    pub status: Status,
    /// Checksum of the algorithm file
    pub checksum: String,
    /// Created at timestamp
    pub created_at: DateTimeUtc,
    /// Updated at timestamp
    pub updated_at: DateTimeUtc,
}

impl From<AlgorithmModel> for AlgorithmInfo {
    fn from(model: AlgorithmModel) -> Self {
        Self {
            id: model.id,
            key: model.key,
            name: model.name,
            description: model.description,
            version: model.version,
            module_type: model.module_type,
            path: model.path,
            config_schema: model.config_schema,
            size: model.size,
            status: model.status,
            checksum: model.checksum,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewAlgorithm {
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "NewAlgorithm::default_version")]
    pub version: String,
    pub module_type: AlgorithmModuleType,
    pub path: String,
    pub config_schema: Option<serde_json::Value>,
    pub size: u64,
    pub checksum: String,
}

impl SeedableTrait for NewAlgorithm {
    type ActiveModel = ActiveModel;
    type Entity = AlgorithmEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

impl NewAlgorithm {
    fn default_version() -> String {
        "1.0.0".to_string()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAlgorithm {
    pub id: i32,
    pub key: String,
    pub name: String,
    pub description: Option<Option<String>>,
    pub version: String,
    pub module_type: AlgorithmModuleType,
    pub path: String,
    pub config_schema: Option<Option<serde_json::Value>>,
    pub size: u64,
    pub checksum: String,
}

/// Query parameters for paginating algorithm records.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct AlgorithmPageParams {
    /// Fuzzy filter by algorithm name.
    pub name: Option<String>,
    /// Exact filter by algorithm module type.
    pub module_type: Option<AlgorithmModuleType>,
    /// Exact filter by algorithm status.
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

// ───────────────────────────────────────────────────────────────────
// ABI data exchange types (host ↔ guest JSON serialization)
// ───────────────────────────────────────────────────────────────────

/// Input JSON for `FrameTransform` WASM modules.
///
/// The host writes pixel data to WASM linear memory first, then serializes
/// this struct (with the WASM-side pointer) as JSON input.
#[derive(Debug, Serialize, Deserialize)]
pub struct FrameTransformInput {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Pointer to RGB24 pixel data in WASM linear memory.
    pub pixels_ptr: u32,
    /// Length of pixel data in bytes (width × height × 3).
    pub pixels_len: u32,
    /// User-defined configuration JSON.
    pub config: serde_json::Value,
}

/// Output JSON from `FrameTransform` WASM modules.
///
/// The guest writes transformed pixel data to WASM memory and returns
/// this struct indicating where the output lives.
#[derive(Debug, Serialize, Deserialize)]
pub struct FrameTransformOutput {
    /// Output frame width in pixels (may differ from input if the transform crops/resizes).
    pub width: u32,
    /// Output frame height in pixels.
    pub height: u32,
    /// Pointer to output RGB24 pixel data in WASM linear memory.
    pub pixels_ptr: u32,
    /// Length of output pixel data in bytes.
    pub pixels_len: u32,
}

/// Input JSON for `ResultProcessor` WASM modules.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResultProcessorInput {
    /// Detection results from inference.
    pub detections: Vec<ResultDetection>,
    /// Classification results from inference.
    pub classifications: Vec<ResultClassification>,
    /// Original frame width.
    pub frame_width: u32,
    /// Original frame height.
    pub frame_height: u32,
    /// User-defined configuration JSON.
    pub config: serde_json::Value,
}

/// Output JSON from `ResultProcessor` WASM modules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultProcessorOutput {
    /// Filtered/modified detections.
    #[serde(default)]
    pub detections: Vec<ResultDetection>,
    /// Filtered/modified classifications.
    #[serde(default)]
    pub classifications: Vec<ResultClassification>,
    /// Custom key-value outputs (for business-specific data).
    #[serde(default)]
    pub custom_outputs: Vec<(String, serde_json::Value)>,
}

/// Simplified detection for WASM ABI serialization.
///
/// Uses plain `String` instead of `Arc<str>` for straightforward serde
/// across the WASM boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultDetection {
    pub bbox: BoundingBox,
    pub class: String,
    pub class_id: u32,
    pub confidence: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track_id: Option<u64>,
}

impl From<&Detection> for ResultDetection {
    fn from(det: &Detection) -> Self {
        Self {
            bbox: det.bbox,
            class: det.class.to_string(),
            class_id: det.class_id,
            confidence: det.confidence,
            track_id: det.track_id,
        }
    }
}

impl From<&ResultDetection> for Detection {
    fn from(det: &ResultDetection) -> Self {
        Self {
            bbox: det.bbox,
            class: Arc::from(det.class.as_str()),
            class_id: det.class_id,
            confidence: det.confidence,
            track_id: det.track_id,
        }
    }
}

/// Simplified classification for WASM ABI serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultClassification {
    pub top_k: Vec<(String, f32)>,
}

impl From<&Classification> for ResultClassification {
    fn from(cls: &Classification) -> Self {
        Self {
            top_k: cls
                .top_k
                .iter()
                .map(|(label, confidence)| (label.to_string(), *confidence))
                .collect(),
        }
    }
}

impl From<&ResultClassification> for Classification {
    fn from(cls: &ResultClassification) -> Self {
        Self {
            top_k: cls
                .top_k
                .iter()
                .map(|(label, confidence)| (Arc::from(label.as_str()), *confidence))
                .collect(),
        }
    }
}

// ───────────────────────────────────────────────────────────────────
// WASM manifest / probe domain types
// ───────────────────────────────────────────────────────────────────

/// The canonical custom section name used to embed algorithm manifest metadata.
///
/// The host scans this section directly from the uploaded WASM binary and does
/// not rely on sidecar files or caller-provided metadata.
pub const WASM_ALGORITHM_MANIFEST_SECTION: &str = "ng.ai.manifest.v1";

/// Versioned manifest payload embedded in WASM custom section.
///
/// This schema is intentionally strict to make installation deterministic:
/// host runtime uses these fields as the source of truth for algorithm identity,
/// semantic versioning, module type, and UI config schema generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WasmAlgorithmManifestV1 {
    /// Manifest schema version. Must be `1` for this structure.
    #[serde(default = "WasmAlgorithmManifestV1::default_manifest_version")]
    pub manifest_version: u32,
    /// Stable algorithm key for runtime and persistence layers.
    pub algorithm_key: String,
    /// Human-readable algorithm name.
    pub name: String,
    /// Optional algorithm description.
    pub description: Option<String>,
    /// Semantic version of this algorithm artifact.
    #[serde(default = "WasmAlgorithmManifestV1::default_artifact_version")]
    pub version: String,
    /// Module type used by host ABI validation and pipeline placement.
    pub module_type: AlgorithmModuleType,
    /// Optional JSON schema for algorithm `config`.
    pub config_schema: Option<serde_json::Value>,
    /// Target SDK API version required by this artifact.
    pub sdk_api_version: u32,
}

impl WasmAlgorithmManifestV1 {
    fn default_manifest_version() -> u32 {
        1
    }

    fn default_artifact_version() -> String {
        "1.0.0".to_string()
    }
}

/// Probed information for one uploaded WASM algorithm artifact.
///
/// This structure is returned by the control-plane `probe` endpoint before
/// installation so UI/clients can preview metadata and compatibility gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlgorithmProbeInfo {
    /// Manifest extracted from `ng.ai.manifest.v1` custom section.
    pub manifest: WasmAlgorithmManifestV1,
    /// File size in bytes of uploaded artifact.
    pub size: u64,
    /// SHA-256 checksum (hex, lowercase).
    pub checksum: String,
}

// ───────────────────────────────────────────────────────────────────
// Algorithm test request types
// ───────────────────────────────────────────────────────────────────

/// Input for the algorithm test endpoint.
///
/// Simulates a pipeline context so users can verify their algorithm works
/// correctly before deploying it in a live pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmTestInput {
    /// Mock detections to feed the algorithm (for ResultProcessor).
    #[serde(default)]
    pub detections: Vec<ResultDetection>,
    /// Mock classifications (for ResultProcessor).
    #[serde(default)]
    pub classifications: Vec<ResultClassification>,
    /// Mock frame width (for both module types).
    #[serde(default = "AlgorithmTestInput::default_frame_width")]
    pub frame_width: u32,
    /// Mock frame height.
    #[serde(default = "AlgorithmTestInput::default_frame_height")]
    pub frame_height: u32,
    /// Configuration to pass to the algorithm.
    #[serde(default)]
    pub config: serde_json::Value,
}

impl AlgorithmTestInput {
    fn default_frame_width() -> u32 {
        1920
    }
    fn default_frame_height() -> u32 {
        1080
    }
}

/// Result of an algorithm test execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmTestResult {
    /// Whether the algorithm executed successfully.
    pub success: bool,
    /// Execution wall-clock time in milliseconds.
    pub execution_time_ms: f64,
    /// Fuel consumed by the WASM execution.
    pub fuel_consumed: u64,
    /// Algorithm output (if successful).
    pub output: Option<ResultProcessorOutput>,
    /// Error message (if failed).
    pub error: Option<String>,
}

/// Required WASM export function names for validation.
pub struct WasmExports;

impl WasmExports {
    /// Memory export name.
    pub const MEMORY: &'static str = "memory";
    /// Allocation function.
    pub const ALLOC: &'static str = "alloc";
    /// Output length query function.
    pub const GET_OUTPUT_LEN: &'static str = "get_output_len";
    /// FrameTransform entry point.
    pub const TRANSFORM: &'static str = "transform";
    /// ResultProcessor entry point.
    pub const PROCESS: &'static str = "process";
}
