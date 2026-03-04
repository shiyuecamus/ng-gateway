//! Model inference backend abstraction.
//!
//! Each model format (ONNX, RKNN, TensorRT, etc.) provides its own
//! [`ModelBackend`] implementation. The [`ModelRegistry`] routes inference
//! requests to the appropriate backend based on the model's format.

use crate::{
    decoded::DecodedFrame,
    pipeline::{
        postprocess::RawInferenceOutput,
        preprocess::{CoordinateTransform, PreProcessor},
    },
};
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::enums::ai::{ModelFormat, TensorDType};
use std::{path::Path, time::Duration};

/// Timing breakdown for one end-to-end inference request.
#[derive(Debug, Clone, Copy)]
pub struct InferTiming {
    /// Time spent in preprocessing.
    pub preprocess: Duration,
    /// Time spent waiting for session lock / worker queue.
    pub infer_wait: Duration,
    /// Time spent inside the inference runtime.
    pub infer_exec: Duration,
    /// Time spent in postprocessing (filled by caller).
    pub postprocess: Duration,
}

/// Runtime backend for loading and executing model inference.
///
/// Each backend manages its own session pool, memory lifecycle, and
/// concurrency model. Backends are format-specific and stateless
/// with respect to model metadata (that lives in the ModelRegistry).
#[async_trait::async_trait]
pub trait ModelBackend: Send + Sync {
    /// The model format this backend handles.
    fn format(&self) -> ModelFormat;

    /// Load a model into the inference runtime, creating one or more sessions.
    async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError>;

    /// Unload a model from the inference runtime, freeing all sessions.
    fn unload(&self, model_id: i32);

    /// Check if a model is currently loaded.
    fn is_loaded(&self, model_id: i32) -> bool;

    /// Run preprocessing + inference on a decoded frame.
    ///
    /// Returns raw output tensors, coordinate transform, and timing breakdown.
    async fn infer(
        &self,
        model_id: i32,
        frame: &DecodedFrame,
        preprocessor: &dyn PreProcessor,
        input_shape: &[i64],
        input_dtype: TensorDType,
    ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError>;

    /// Number of currently loaded models.
    fn loaded_count(&self) -> usize;

    /// Estimated total memory usage of loaded models in bytes.
    fn estimated_memory_bytes(&self) -> u64;
}
