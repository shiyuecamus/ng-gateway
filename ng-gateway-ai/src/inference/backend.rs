//! Model inference backend abstraction.
//!
//! Each model format (ONNX, RKNN, TensorRT, etc.) provides its own
//! [`ModelBackend`] implementation. The [`ModelRegistry`] routes inference
//! requests to the appropriate backend based on the model's format.
//!
//! # Design: Preprocessing Separated from Inference
//!
//! The `infer()` method receives a [`PreprocessOutput`] — the caller
//! (typically the Engine layer) is responsible for running the appropriate
//! preprocessor before handing off to the backend. This separation enables:
//!
//! - **Backend-specific input formats**: ONNX needs NCHW float32, RKNN
//!   needs NHWC uint8. The Engine picks the right preprocessor based on
//!   [`supports_dma_input()`] and the model's declared dtype.
//! - **Zero-copy DMA paths**: When `supports_dma_input()` is true and the
//!   input is `PreprocessOutput::DeviceMemory`, the backend can import the
//!   DMA-buf fd directly without any CPU copy.
//! - **Single Responsibility**: each backend only does inference; coordinate
//!   transforms and preprocessing are orthogonal concerns.

use super::RawInferenceOutput;
use crate::pipeline::preprocess::PreprocessOutput;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::enums::ai::ModelFormat;
use std::{path::Path, time::Duration};

/// Timing breakdown for one end-to-end inference request.
#[derive(Debug, Clone, Copy)]
pub struct InferTiming {
    /// Time spent waiting for session lock / worker queue.
    pub infer_wait: Duration,
    /// Time spent inside the inference runtime.
    pub infer_exec: Duration,
}

/// Runtime backend for loading and executing model inference.
///
/// Each backend manages its own session pool, memory lifecycle, and
/// concurrency model. Backends are format-specific and stateless
/// with respect to model metadata (that lives in the ModelRegistry).
///
/// # Preprocessing Contract
///
/// The caller is responsible for preprocessing frames into the format
/// expected by this backend. The backend's [`supports_dma_input()`]
/// method tells the caller whether `PreprocessOutput::DeviceMemory`
/// is accepted for zero-copy NPU/GPU inference.
#[async_trait::async_trait]
pub trait ModelBackend: Send + Sync {
    /// The model format this backend handles.
    fn format(&self) -> ModelFormat;

    /// Whether this backend can consume DMA-buf / device memory input directly.
    ///
    /// When `true`, the Engine will attempt to produce `PreprocessOutput::DeviceMemory`
    /// for frames that are already in device memory (e.g. GStreamer DMA-buf output
    /// on RK3588 after RGA resize). When `false`, only `PreprocessOutput::CpuTensor`
    /// is expected.
    fn supports_dma_input(&self) -> bool;

    /// Load a model into the inference runtime, creating one or more sessions.
    async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError>;

    /// Unload a model from the inference runtime, freeing all sessions.
    fn unload(&self, model_id: i32);

    /// Check if a model is currently loaded.
    fn is_loaded(&self, model_id: i32) -> bool;

    /// Run inference on already-preprocessed input.
    ///
    /// The backend dispatches on the [`PreprocessOutput`] variant:
    /// - `CpuTensor` → standard CPU/GPU inference (ONNX, fallback RKNN)
    /// - `DeviceMemory(DmaBuf)` → zero-copy NPU input (RKNN `create_mem_from_fd`)
    /// - `DeviceMemory(DeviceBuffer)` → zero-copy GPU input (TensorRT CUDA ptr)
    ///
    /// Returns raw output tensors and timing. Coordinate transforms and
    /// postprocessing are the caller's responsibility.
    async fn infer(
        &self,
        model_id: i32,
        input: PreprocessOutput,
    ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError>;

    /// Number of currently loaded models.
    fn loaded_count(&self) -> usize;

    /// Estimated total memory usage of loaded models in bytes.
    fn estimated_memory_bytes(&self) -> u64;
}
