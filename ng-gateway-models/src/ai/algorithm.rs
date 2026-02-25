//! WASM algorithm types and ABI contracts.
//!
//! Defines the metadata, module types, and host–guest data exchange formats
//! for the dual-mode WASM algorithm system.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::types::{BoundingBox, Detection};

// ───────────────────────────────────────────────────────────────────
// Module type & metadata
// ───────────────────────────────────────────────────────────────────

/// WASM module type — determines the host–guest interface contract.
///
/// Each type defines a distinct ABI and data flow:
/// - `FrameTransform` operates on raw pixel data (before inference)
/// - `ResultProcessor` operates on structured analysis results (after inference)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WasmModuleType {
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

impl std::fmt::Display for WasmModuleType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FrameTransform => write!(f, "frame_transform"),
            Self::ResultProcessor => write!(f, "result_processor"),
        }
    }
}

/// Metadata for a registered WASM algorithm module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmAlgorithmInfo {
    /// Unique identifier (derived from filename or user-specified).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this algorithm does.
    pub description: String,
    /// Version string (semver).
    pub version: String,
    /// Module type — determines ABI and pipeline mount point.
    pub module_type: WasmModuleType,
    /// File size in bytes.
    pub file_size: u64,
    /// JSON Schema for the `config` parameter (for UI form generation).
    /// `None` means the algorithm accepts arbitrary config or no config.
    pub config_schema: Option<serde_json::Value>,
    /// Timestamp when the algorithm was registered.
    #[serde(default = "chrono::Utc::now")]
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Sidecar metadata file for a WASM algorithm (stored as `<name>.json`).
///
/// When scanning the algorithms directory, the host looks for a JSON sidecar
/// alongside each `.wasm` file. If absent, defaults are inferred from the filename.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmAlgorithmSidecar {
    /// Human-readable name (defaults to filename stem).
    pub name: Option<String>,
    /// Description.
    pub description: Option<String>,
    /// Version string.
    pub version: Option<String>,
    /// Module type (required — no sensible default).
    pub module_type: WasmModuleType,
    /// JSON Schema for config parameter.
    pub config_schema: Option<serde_json::Value>,
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

// ───────────────────────────────────────────────────────────────────
// Algorithm upload / test request types
// ───────────────────────────────────────────────────────────────────

/// Metadata provided when uploading a new WASM algorithm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlgorithmUploadMetadata {
    /// Human-readable name.
    pub name: String,
    /// Description of what this algorithm does.
    #[serde(default)]
    pub description: String,
    /// Version string (semver).
    #[serde(default = "default_version")]
    pub version: String,
    /// Module type (determines ABI).
    pub module_type: WasmModuleType,
    /// Optional JSON Schema for the config parameter.
    pub config_schema: Option<serde_json::Value>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

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
    #[serde(default = "default_frame_width")]
    pub frame_width: u32,
    /// Mock frame height.
    #[serde(default = "default_frame_height")]
    pub frame_height: u32,
    /// Configuration to pass to the algorithm.
    #[serde(default)]
    pub config: serde_json::Value,
}

fn default_frame_width() -> u32 {
    1920
}
fn default_frame_height() -> u32 {
    1080
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
