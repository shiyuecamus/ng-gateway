//! ONNX model prober — extracts metadata via a temporary `ort::Session`.
//!
//! Creates a lightweight CPU-only session to query precise input/output
//! tensor metadata, then immediately drops it. Also decodes the ONNX
//! protobuf for producer info, opset, and embedded metadata properties
//! via the `prost`-derived [`ModelProto`](super::onnx_proto::ModelProto).

use super::{compute_sha256, infer_task_and_variant, onnx_proto::ModelProto, ModelProber};
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{ModelProbeInfo, ProducerInfo},
    entities::ai::model::TensorDesc,
    enums::ai::{ModelFormat, TensorDType},
};
use ort::{session::Session, tensor::TensorElementType, value::ValueType};
use prost::Message;
use std::{collections::HashMap, path::Path};
use tracing::{debug, warn};

/// ONNX model prober using `ort` runtime for precise tensor metadata.
pub struct OnnxModelProber;

impl OnnxModelProber {
    /// Create a new ONNX model prober.
    pub fn new() -> Self {
        Self
    }
}

impl Default for OnnxModelProber {
    fn default() -> Self {
        Self::new()
    }
}

/// Parsed metadata tuple extracted from ONNX protobuf.
type OnnxParsedMetadata = (
    Option<ProducerInfo>,
    Option<i64>,
    HashMap<String, String>,
    Option<Vec<String>>,
);

impl ModelProber for OnnxModelProber {
    fn format(&self) -> ModelFormat {
        ModelFormat::Onnx
    }

    fn extensions(&self) -> &[&str] {
        &["onnx"]
    }

    fn probe(&self, path: &Path) -> Result<ModelProbeInfo, AiEngineError> {
        let file_meta = std::fs::metadata(path)
            .map_err(|e| AiEngineError::IoError(format!("read model metadata: {e}")))?;
        let file_size = file_meta.len();

        let checksum = compute_sha256(path)?;

        // Build a minimal CPU-only session for probing tensor metadata.
        let session = Session::builder()
            .map_err(|e| AiEngineError::ModelLoadError(format!("create session builder: {e}")))?
            .with_intra_threads(1)
            .map_err(|e| AiEngineError::ModelLoadError(format!("set intra threads: {e}")))?
            .commit_from_file(path)
            .map_err(|e| {
                AiEngineError::ModelLoadError(format!("load ONNX model '{}': {e}", path.display()))
            })?;

        let inputs = extract_inputs(&session);
        let outputs = extract_outputs(&session);

        // ONNX uses NCHW layout.
        let (inferred_task, inferred_variant, recommended_post) =
            infer_task_and_variant(&outputs, true);

        // Parse ONNX protobuf for producer info, opset, and metadata.
        let (producer, opset_version, metadata_props, labels) = parse_onnx_protobuf(path)
            .unwrap_or_else(|e| {
                warn!(path = %path.display(), error = %e, "failed to parse ONNX protobuf metadata");
                (None, None, HashMap::new(), None)
            });

        debug!(
            path = %path.display(),
            inputs = inputs.len(),
            outputs = outputs.len(),
            task = ?inferred_task,
            variant = ?inferred_variant,
            size = file_size,
            "ONNX model probed"
        );

        Ok(ModelProbeInfo {
            format: ModelFormat::Onnx,
            inputs,
            outputs,
            inferred_task,
            inferred_variant,
            recommended_postprocessor: recommended_post,
            recommended_preprocess: None,
            producer,
            opset_version,
            target_platform: None,
            quantization: None,
            metadata_props,
            labels,
            size: file_size,
            checksum,
        })
    }
}

/// Extract input tensor descriptions from an ort session.
fn extract_inputs(session: &Session) -> Vec<TensorDesc> {
    session
        .inputs()
        .iter()
        .filter_map(|input| {
            let name = input.name().to_string();
            tensor_desc_from_value_type(&name, input.dtype())
        })
        .collect()
}

/// Extract output tensor descriptions from an ort session.
fn extract_outputs(session: &Session) -> Vec<TensorDesc> {
    session
        .outputs()
        .iter()
        .filter_map(|output| {
            let name = output.name().to_string();
            tensor_desc_from_value_type(&name, output.dtype())
        })
        .collect()
}

/// Convert an ort `ValueType` to our `TensorDesc`.
fn tensor_desc_from_value_type(name: &str, vt: &ValueType) -> Option<TensorDesc> {
    match vt {
        ValueType::Tensor {
            ty,
            shape,
            dimension_symbols: _,
        } => {
            let dtype = ort_element_type_to_dtype(*ty);
            let dims: Vec<i64> = shape.iter().map(|&d| if d <= 0 { -1 } else { d }).collect();
            Some(TensorDesc {
                name: name.to_string(),
                shape: dims,
                dtype,
            })
        }
        _ => None,
    }
}

/// Map ort tensor element type to our `TensorDType`.
fn ort_element_type_to_dtype(ty: TensorElementType) -> TensorDType {
    match ty {
        TensorElementType::Float32 => TensorDType::Float32,
        TensorElementType::Float16 | TensorElementType::Bfloat16 => TensorDType::Float16,
        TensorElementType::Int8 => TensorDType::Int8,
        TensorElementType::Uint8 => TensorDType::UInt8,
        TensorElementType::Int32 => TensorDType::Int32,
        TensorElementType::Int64 => TensorDType::Int64,
        _ => TensorDType::Float32,
    }
}

/// Parse ONNX protobuf via `prost` to extract producer info, opset, and metadata.
///
/// Decodes only the top-level `ModelProto` fields. The heavy `GraphProto`
/// (field 7 in the full schema, but our field 7 is `doc_string`) is
/// automatically skipped by prost since our subset doesn't declare
/// the graph field (which is at tag 7 in the full proto but we remap
/// `doc_string` to tag 7 matching the ONNX spec where doc_string=7
/// and graph=8).
fn parse_onnx_protobuf(path: &Path) -> Result<OnnxParsedMetadata, AiEngineError> {
    let bytes =
        std::fs::read(path).map_err(|e| AiEngineError::IoError(format!("read ONNX file: {e}")))?;

    let model = ModelProto::decode(bytes.as_slice())
        .map_err(|e| AiEngineError::ModelLoadError(format!("ONNX protobuf decode: {e}")))?;

    let producer = if !model.producer_name.is_empty() {
        Some(ProducerInfo {
            name: model.producer_name,
            version: Some(model.producer_version).filter(|s| !s.is_empty()),
            model_version: if model.model_version != 0 {
                Some(model.model_version)
            } else {
                None
            },
            doc_string: Some(model.doc_string).filter(|s| !s.is_empty()),
            domain: Some(model.domain).filter(|s| !s.is_empty()),
        })
    } else {
        None
    };

    let opset_version = model.opset_import.iter().map(|o| o.version).max();

    let metadata_props: HashMap<String, String> = model
        .metadata_props
        .iter()
        .map(|p| (p.key.clone(), p.value.clone()))
        .collect();

    let labels = metadata_props
        .get("labels")
        .or_else(|| metadata_props.get("class_names"))
        .map(|v| {
            v.split(&[',', '\n'][..])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        });

    Ok((producer, opset_version, metadata_props, labels))
}
