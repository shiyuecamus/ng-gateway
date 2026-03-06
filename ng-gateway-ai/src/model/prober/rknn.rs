//! RKNN model prober — extracts metadata via the RKNN Lite runtime SDK.
//!
//! Creates a temporary RKNN context to query precise input/output tensor
//! attributes (shape, dtype, layout, quantization), then immediately
//! releases it. When the `rknn` feature is enabled and `librknnrt.so` is
//! available, this prober uses the actual RKNN runtime SDK for precise
//! tensor extraction. Otherwise it falls back to header-only parsing.

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
pub struct RknnModelProber;

impl RknnModelProber {
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

        let model_data = std::fs::read(path)
            .map_err(|e| AiEngineError::IoError(format!("read RKNN model: {e}")))?;

        let (target_platform, sdk_version) = parse_rknn_header(&model_data);

        let (inputs, outputs) = probe_rknn_tensors(model_data).unwrap_or_else(|e| {
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
fn parse_rknn_header(data: &[u8]) -> (Option<String>, Option<String>) {
    if data.len() < 16 {
        return (None, None);
    }

    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    if magic != 0x4E4E_4B52 {
        return (None, None);
    }

    let version = if data.len() >= 12 {
        let major = u16::from_le_bytes([data[4], data[5]]);
        let minor = u16::from_le_bytes([data[6], data[7]]);
        let patch = u16::from_le_bytes([data[8], data[9]]);
        Some(format!("{major}.{minor}.{patch}"))
    } else {
        None
    };

    (None, version)
}

/// Probe RKNN tensor information using the actual RKNN runtime SDK.
#[cfg(all(feature = "rknn", target_os = "linux", target_arch = "aarch64"))]
fn probe_rknn_tensors(
    mut model_data: Vec<u8>,
) -> Result<(Vec<TensorDesc>, Vec<TensorDesc>), AiEngineError> {
    use ng_gateway_models::enums::ai::TensorDType;
    use rknpu2::{
        api::RknnInitFlags,
        query::{
            input_attr::InputAttr, output_attr::OutputAttr, InputOutputNum, Query, QueryWithInput,
            TensorAttrView,
        },
        rknn::RKNN,
        tensor::DataTypeKind,
    };

    let ctx = RKNN::new_with_library("librknnrt.so", &mut model_data, RknnInitFlags::empty())
        .map_err(|e| {
            AiEngineError::IoError(format!(
                "cannot init RKNN context for probing (librknnrt.so not found?): {e}"
            ))
        })?;

    let io_num: InputOutputNum = ctx
        .query()
        .map_err(|e| AiEngineError::ModelLoadError(format!("rknn query IN_OUT_NUM: {e}")))?;

    let mut inputs = Vec::with_capacity(io_num.input_num() as usize);
    for i in 0..io_num.input_num() {
        let attr: InputAttr = ctx.query_with_input(i).map_err(|e| {
            AiEngineError::ModelLoadError(format!("rknn query INPUT_ATTR[{i}]: {e}"))
        })?;
        inputs.push(TensorDesc {
            name: attr.name(),
            shape: attr.dims().iter().map(|&d| d as i64).collect(),
            dtype: map_rknn_dtype(attr.dtype()),
        });
    }

    let mut outputs = Vec::with_capacity(io_num.output_num() as usize);
    for i in 0..io_num.output_num() {
        let attr: OutputAttr = ctx.query_with_input(i).map_err(|e| {
            AiEngineError::ModelLoadError(format!("rknn query OUTPUT_ATTR[{i}]: {e}"))
        })?;
        outputs.push(TensorDesc {
            name: attr.name(),
            shape: attr.dims().iter().map(|&d| d as i64).collect(),
            dtype: map_rknn_dtype(attr.dtype()),
        });
    }

    // Context is dropped here, calling rknn_destroy() via RAII.
    Ok((inputs, outputs))
}

/// Map rknpu2 DataTypeKind to our TensorDType.
#[cfg(all(feature = "rknn", target_os = "linux", target_arch = "aarch64"))]
fn map_rknn_dtype(
    dtype: rknpu2::tensor::DataTypeKind,
) -> ng_gateway_models::enums::ai::TensorDType {
    use ng_gateway_models::enums::ai::TensorDType;
    use rknpu2::tensor::DataTypeKind;

    match dtype {
        DataTypeKind::Float32(_) => TensorDType::Float32,
        DataTypeKind::Float16(_) => TensorDType::Float16,
        DataTypeKind::Int8(_) => TensorDType::Int8,
        DataTypeKind::UInt8(_) => TensorDType::UInt8,
        DataTypeKind::Int16(_) => TensorDType::Int16,
        DataTypeKind::Int32(_) => TensorDType::Int32,
        DataTypeKind::Int64(_) => TensorDType::Int64,
        _ => TensorDType::Float32,
    }
}

/// Fallback prober: returns empty tensors when RKNN runtime is not available.
#[cfg(not(all(feature = "rknn", target_os = "linux", target_arch = "aarch64")))]
fn probe_rknn_tensors(
    _model_data: Vec<u8>,
) -> Result<(Vec<TensorDesc>, Vec<TensorDesc>), AiEngineError> {
    Ok((vec![], vec![]))
}

/// Infer task and variant from RKNN output tensors.
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
