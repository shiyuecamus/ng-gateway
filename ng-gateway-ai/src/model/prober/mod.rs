//! Model probing — extract metadata from model artifacts via runtime sessions.
//!
//! Each model format (ONNX, RKNN, TensorRT, etc.) provides its own
//! [`ModelProber`] implementation. The prober creates a temporary runtime
//! session to extract precise tensor metadata, then immediately destroys it.

use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::{
    domain::prelude::{ModelProbeInfo, ModelVariant},
    entities::ai::model::TensorDesc,
    enums::ai::{ModelFormat, ModelTask, PostProcessorType},
};
use std::path::Path;

#[cfg(feature = "engine")]
mod onnx;
#[cfg(feature = "engine")]
mod onnx_proto;
#[cfg(feature = "engine")]
mod rknn;

#[cfg(feature = "engine")]
pub use onnx::OnnxModelProber;
#[cfg(feature = "engine")]
pub use rknn::RknnModelProber;

/// Trait for probing model artifacts to extract metadata.
///
/// Each model format provides its own implementation. The prober creates
/// a temporary runtime session to extract precise tensor metadata (shapes,
/// dtypes, names), then immediately destroys the session.
///
/// # Contract
///
/// - `probe()` MUST NOT persist any state or register the model.
/// - `probe()` SHOULD be callable from `spawn_blocking` for large files.
/// - The temporary session MUST be dropped before returning.
pub trait ModelProber: Send + Sync {
    /// The model format this prober handles.
    fn format(&self) -> ModelFormat;

    /// File extensions this prober recognises (e.g. `["onnx"]`, `["rknn"]`).
    fn extensions(&self) -> &[&str];

    /// Probe a model file and extract metadata via a temporary runtime session.
    fn probe(&self, path: &Path) -> Result<ModelProbeInfo, AiEngineError>;
}

// ── Shared utilities ─────────────────────────────────────────────────

/// Compute SHA-256 checksum of a file using 64 KiB buffered reads.
#[cfg(feature = "engine")]
pub(crate) fn compute_sha256(path: &Path) -> Result<String, AiEngineError> {
    use sha2::{Digest, Sha256};
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

/// Infer model task, variant, and recommended postprocessor from output
/// tensor shapes. Works for both ONNX (NCHW) and RKNN (NHWC) layouts —
/// the caller normalises the shape to `[batch, ...]` before calling.
///
/// Layout parameter controls 4-D interpretation:
/// - `nchw = true`:  `[1, C, H, W]` (ONNX default)
/// - `nchw = false`: `[1, H, W, C]` (RKNN default)
pub fn infer_task_and_variant(
    outputs: &[TensorDesc],
    nchw: bool,
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
        // [1, C, N] / [1, N, C] — detection (YOLOv5 or YOLOv8)
        3 => {
            let dim1 = first.shape[1];
            let dim2 = first.shape[2];
            if dim1 > 0 && dim2 > 0 {
                if dim1 < dim2 {
                    // YOLOv8: [1, C, N] where C < N (e.g. [1, 84, 8400])
                    let is_pose = dim1 > 10 && (dim1 - 5) % 3 == 0;
                    if is_pose {
                        (
                            Some(ModelTask::ObjectDetection),
                            Some(ModelVariant::YoloV8Pose),
                            Some(PostProcessorType::YoloV8Pose),
                        )
                    } else {
                        (
                            Some(ModelTask::ObjectDetection),
                            Some(ModelVariant::YoloV8),
                            Some(PostProcessorType::YoloV8Detection),
                        )
                    }
                } else {
                    // YOLOv5: [1, N, C] where N > C (e.g. [1, 25200, 85])
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
        // [1, C] — classification
        2 => (
            Some(ModelTask::Classification),
            Some(ModelVariant::Generic),
            Some(PostProcessorType::Classification),
        ),
        // 4-D: segmentation or anomaly. Layout-aware channel extraction.
        4 => {
            let channels = if nchw {
                first.shape[1] // NCHW: channels at index 1
            } else {
                first.shape[3] // NHWC: channels at index 3
            };
            if channels > 1 {
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

/// Registry of available model probers, keyed by format.
pub struct ProberRegistry {
    probers: Vec<Box<dyn ModelProber>>,
}

impl ProberRegistry {
    /// Create a new registry from a list of probers.
    pub fn new(probers: Vec<Box<dyn ModelProber>>) -> Self {
        Self { probers }
    }

    /// Find a prober by file extension.
    pub fn find_by_extension(&self, ext: &str) -> Option<&dyn ModelProber> {
        let ext_lower = ext.to_ascii_lowercase();
        self.probers
            .iter()
            .find(|p| p.extensions().iter().any(|e| *e == ext_lower))
            .map(|p| p.as_ref())
    }

    /// Find a prober by model format.
    pub fn find_by_format(&self, format: ModelFormat) -> Option<&dyn ModelProber> {
        self.probers
            .iter()
            .find(|p| p.format() == format)
            .map(|p| p.as_ref())
    }

    /// List all supported file extensions across all probers.
    pub fn supported_extensions(&self) -> Vec<&str> {
        self.probers
            .iter()
            .flat_map(|p| p.extensions().iter().copied())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ng_gateway_models::enums::ai::TensorDType;

    fn tensor_desc(name: &str, shape: Vec<i64>) -> TensorDesc {
        TensorDesc {
            name: name.to_string(),
            shape,
            dtype: TensorDType::Float32,
        }
    }

    #[test]
    fn infer_yolov8_detection_shape() {
        let outputs = vec![tensor_desc("output0", vec![1, 84, 8400])];
        let (task, variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::ObjectDetection));
        assert_eq!(variant, Some(ModelVariant::YoloV8));
        assert_eq!(post, Some(PostProcessorType::YoloV8Detection));
    }

    #[test]
    fn infer_yolov5_detection_shape() {
        let outputs = vec![tensor_desc("output0", vec![1, 25200, 85])];
        let (task, variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::ObjectDetection));
        assert_eq!(variant, Some(ModelVariant::YoloV5));
        assert_eq!(post, Some(PostProcessorType::YoloV5Detection));
    }

    #[test]
    fn infer_classification_shape() {
        let outputs = vec![tensor_desc("output0", vec![1, 1000])];
        let (task, variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::Classification));
        assert_eq!(variant, Some(ModelVariant::Generic));
        assert_eq!(post, Some(PostProcessorType::Classification));
    }

    #[test]
    fn infer_segmentation_nchw_shape() {
        let outputs = vec![tensor_desc("output0", vec![1, 21, 512, 512])];
        let (task, _variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::Segmentation));
        assert_eq!(post, Some(PostProcessorType::Segmentation));
    }

    #[test]
    fn infer_segmentation_nhwc_shape() {
        let outputs = vec![tensor_desc("output0", vec![1, 512, 512, 21])];
        let (task, _variant, post) = infer_task_and_variant(&outputs, false);
        assert_eq!(task, Some(ModelTask::Segmentation));
        assert_eq!(post, Some(PostProcessorType::Segmentation));
    }

    #[test]
    fn infer_anomaly_detection_single_channel() {
        let outputs = vec![tensor_desc("output0", vec![1, 1, 256, 256])];
        let (task, _variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::AnomalyDetection));
        assert_eq!(post, Some(PostProcessorType::AnomalyDetection));
    }

    #[test]
    fn infer_yolov8_pose_shape() {
        // 5 + 17*3 = 56 features → pose model
        let outputs = vec![tensor_desc("output0", vec![1, 56, 8400])];
        let (task, variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, Some(ModelTask::ObjectDetection));
        assert_eq!(variant, Some(ModelVariant::YoloV8Pose));
        assert_eq!(post, Some(PostProcessorType::YoloV8Pose));
    }

    #[test]
    fn infer_empty_outputs_returns_none() {
        let outputs: Vec<TensorDesc> = vec![];
        let (task, variant, post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, None);
        assert_eq!(variant, None);
        assert_eq!(post, None);
    }

    #[test]
    fn infer_unknown_5d_shape_returns_generic() {
        let outputs = vec![tensor_desc("output0", vec![1, 2, 3, 4, 5])];
        let (task, variant, _post) = infer_task_and_variant(&outputs, true);
        assert_eq!(task, None);
        assert_eq!(variant, Some(ModelVariant::Generic));
    }

    #[test]
    fn compute_sha256_on_temp_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&file_path).expect("create file");
        f.write_all(b"hello world").expect("write");
        drop(f);

        let hash = compute_sha256(&file_path).expect("compute sha256");
        // SHA-256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn prober_registry_find_by_extension() {
        struct DummyProber;
        impl ModelProber for DummyProber {
            fn format(&self) -> ModelFormat {
                ModelFormat::Onnx
            }
            fn extensions(&self) -> &[&str] {
                &["onnx"]
            }
            fn probe(&self, _path: &Path) -> Result<ModelProbeInfo, AiEngineError> {
                Err(AiEngineError::InternalError("not implemented".into()))
            }
        }

        let registry = ProberRegistry::new(vec![Box::new(DummyProber)]);
        assert!(registry.find_by_extension("onnx").is_some());
        assert!(registry.find_by_extension("ONNX").is_some());
        assert!(registry.find_by_extension("rknn").is_none());
        assert_eq!(registry.supported_extensions(), vec!["onnx"]);
    }
}
