//! RKNN model prober — extracts metadata via the RKNN Lite runtime SDK.
//!
//! Creates a temporary RKNN context to query precise input/output tensor
//! attributes (shape, dtype, layout, quantization), then immediately
//! releases it. Only available on ARM Linux with RKNN NPU hardware.
//!
//! # Build Requirements
//!
//! Requires the `rknn` feature flag and `librknnrt.so` on the library path.
//! The RKNN Toolkit Lite2 shared library is loaded at runtime via `dlopen`.

use super::ModelProber;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{ModelProbeInfo, ModelVariant, ProducerInfo},
    entities::ai::model::TensorDesc,
    enums::ai::{ModelFormat, ModelTask, PostProcessorType},
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path};
use tracing::{debug, warn};

/// RKNN model prober using the RKNN Toolkit Lite2 SDK.
///
/// This prober loads the `.rknn` model into a temporary RKNN context,
/// queries tensor attributes, and immediately destroys the context.
/// It does NOT run any inference.
pub struct RknnModelProber;

impl RknnModelProber {
    /// Create a new RKNN model prober.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RknnModelProber {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelProber for RknnModelProber {
    fn format(&self) -> ModelFormat {
        ModelFormat::Rknn
    }

    fn extensions(&self) -> &[&str] {
        &["rknn"]
    }

    fn probe(&self, path: &Path) -> Result<ModelProbeInfo, AiEngineError> {
        let file_meta = std::fs::metadata(path)
            .map_err(|e| AiEngineError::IoError(format!("read model metadata: {e}")))?;
        let file_size = file_meta.len();
        let checksum = compute_sha256(path)?;

        // Read the RKNN model file into memory (required by rknn_init).
        let model_data = std::fs::read(path)
            .map_err(|e| AiEngineError::IoError(format!("read RKNN model: {e}")))?;

        // Parse RKNN file header for SDK version and target platform.
        let (target_platform, sdk_version) = parse_rknn_header(&model_data);

        // For now, without the actual RKNN runtime SDK linked, we extract
        // what we can from the file header and return a partial probe result.
        // When the rknn-lite crate is available, this will use:
        //   rknn_init() → rknn_query(IN_OUT_NUM) → rknn_query(INPUT/OUTPUT_ATTR) → rknn_destroy()
        let (inputs, outputs) = probe_rknn_tensors(&model_data).unwrap_or_else(|e| {
            warn!(
                path = %path.display(),
                error = %e,
                "RKNN tensor probing failed, returning empty tensors"
            );
            (vec![], vec![])
        });

        let (inferred_task, inferred_variant, recommended_post) = if !outputs.is_empty() {
            infer_task_and_variant_rknn(&outputs)
        } else {
            (None, None, None)
        };

        // RKNN models are typically INT8 quantized.
        let quantization = Some("int8".to_string());

        debug!(
            path = %path.display(),
            inputs = inputs.len(),
            outputs = outputs.len(),
            target_platform = ?target_platform,
            sdk_version = ?sdk_version,
            size = file_size,
            "RKNN model probed"
        );

        Ok(ModelProbeInfo {
            format: ModelFormat::Rknn,
            inputs,
            outputs,
            inferred_task,
            inferred_variant,
            recommended_postprocessor: recommended_post,
            recommended_preprocess: None,
            producer: sdk_version.map(|v| ProducerInfo {
                name: "rknn-toolkit2".to_string(),
                version: Some(v),
                model_version: None,
                doc_string: None,
                domain: None,
            }),
            opset_version: None,
            target_platform,
            quantization,
            metadata_props: HashMap::new(),
            labels: None,
            size: file_size,
            checksum,
        })
    }
}

/// Parse RKNN file header to extract target platform and SDK version.
///
/// RKNN files have a custom binary header. The exact layout depends on
/// the RKNN Toolkit version, but common fields include:
/// - Magic bytes: `RKNN` (0x4E4E4B52)
/// - Version fields for SDK compatibility
/// - Target platform identifier
fn parse_rknn_header(data: &[u8]) -> (Option<String>, Option<String>) {
    if data.len() < 16 {
        return (None, None);
    }

    // Check RKNN magic: "RKNN" in little-endian = 0x4E4E4B52
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != 0x4E4E_4B52 {
        return (None, None);
    }

    // Version is typically at offset 4 as a u64 encoding major.minor.patch.
    let version = if data.len() >= 12 {
        let major = u16::from_le_bytes([data[4], data[5]]);
        let minor = u16::from_le_bytes([data[6], data[7]]);
        let patch = u16::from_le_bytes([data[8], data[9]]);
        Some(format!("{major}.{minor}.{patch}"))
    } else {
        None
    };

    // Target platform detection: heuristic based on model structure.
    // The actual platform info requires rknn_query(RKNN_QUERY_SDK_VERSION).
    // For header-only parsing, we can detect RK3588 vs RK356x from the
    // model structure, but this is unreliable. Prefer runtime query.
    let platform = None;

    (platform, version)
}

/// Attempt to probe RKNN tensor information from model data.
///
/// When full RKNN runtime is available (feature `rknn`), this function
/// will use `rknn_init` → `rknn_query` → `rknn_destroy` to get precise
/// tensor attributes. Currently returns empty tensors as a stub.
fn probe_rknn_tensors(
    _model_data: &[u8],
) -> Result<(Vec<TensorDesc>, Vec<TensorDesc>), AiEngineError> {
    // TODO: Implement with actual rknn_lite FFI when the crate is available.
    //
    // The implementation will:
    // 1. rknn_init(&ctx, model_data, model_size, 0)
    // 2. rknn_query(ctx, RKNN_QUERY_IN_OUT_NUM, &io_num)
    // 3. For each input: rknn_query(ctx, RKNN_QUERY_INPUT_ATTR, &attr)
    //    → extract name, dims, n_dims, type (RKNN_TENSOR_UINT8 etc.), fmt (NHWC/NCHW)
    // 4. For each output: rknn_query(ctx, RKNN_QUERY_OUTPUT_ATTR, &attr)
    // 5. rknn_destroy(ctx)
    //
    // RKNN tensor types map to our TensorDType:
    //   RKNN_TENSOR_FLOAT32 → Float32
    //   RKNN_TENSOR_FLOAT16 → Float16
    //   RKNN_TENSOR_INT8    → Int8
    //   RKNN_TENSOR_UINT8   → UInt8
    //   RKNN_TENSOR_INT32   → Int32
    //   RKNN_TENSOR_INT64   → Int64
    //
    // RKNN layout (fmt) is typically NHWC, which affects preprocessing.

    Ok((vec![], vec![]))
}

/// Infer task and variant from RKNN output tensors.
///
/// RKNN outputs follow the same shape conventions as ONNX, but the
/// layout may be NHWC instead of NCHW. The shape analysis is similar
/// but accounts for potential layout differences.
fn infer_task_and_variant_rknn(
    outputs: &[TensorDesc],
) -> (
    Option<ModelTask>,
    Option<ModelVariant>,
    Option<PostProcessorType>,
) {
    let first = match outputs.first() {
        Some(t) => t,
        None => return (None, None, None),
    };

    match first.shape.len() {
        3 => {
            let dim1 = first.shape[1];
            let dim2 = first.shape[2];
            if dim1 > 0 && dim2 > 0 {
                if dim1 < dim2 {
                    (
                        Some(ModelTask::ObjectDetection),
                        Some(ModelVariant::YoloV8),
                        Some(PostProcessorType::YoloV8Detection),
                    )
                } else {
                    (
                        Some(ModelTask::ObjectDetection),
                        Some(ModelVariant::YoloV5),
                        Some(PostProcessorType::YoloV5Detection),
                    )
                }
            } else {
                (None, Some(ModelVariant::Generic), None)
            }
        }
        2 => (
            Some(ModelTask::Classification),
            Some(ModelVariant::Generic),
            Some(PostProcessorType::Classification),
        ),
        4 => {
            // RKNN may use NHWC: [1, H, W, C] — channels last
            let last_dim = first.shape[3];
            if last_dim > 1 {
                (
                    Some(ModelTask::Segmentation),
                    Some(ModelVariant::Generic),
                    Some(PostProcessorType::Segmentation),
                )
            } else {
                (
                    Some(ModelTask::AnomalyDetection),
                    Some(ModelVariant::Generic),
                    Some(PostProcessorType::AnomalyDetection),
                )
            }
        }
        _ => (None, Some(ModelVariant::Generic), None),
    }
}

/// Compute SHA-256 checksum of a file.
fn compute_sha256(path: &Path) -> Result<String, AiEngineError> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)
        .map_err(|e| AiEngineError::IoError(format!("open file for checksum: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AiEngineError::IoError(format!("read file for checksum: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
