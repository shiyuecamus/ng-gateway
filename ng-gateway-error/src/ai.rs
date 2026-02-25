//! AI engine error types.
//!
//! Provides a unified error hierarchy for all AI engine operations including
//! model management, inference, pipeline orchestration, and algorithm execution.

use thiserror::Error;

/// Unified error type for AI engine operations.
#[derive(Debug, Error)]
pub enum AiEngineError {
    /// Engine inference queue is full — caller should drop the frame.
    #[error("inference queue at capacity (backpressure)")]
    Backpressure,

    /// Requested pipeline not found for the given channel.
    #[error("pipeline not found for channel {0}")]
    PipelineNotFound(i32),

    /// Requested model not found in registry.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// Model loading failed.
    #[error("model load error: {0}")]
    ModelLoadError(String),

    /// Inference execution failed.
    #[error("inference error: {0}")]
    InferenceError(String),

    /// Preprocessing failed (resize, normalize, etc.).
    #[error("preprocess error: {0}")]
    PreprocessError(String),

    /// Postprocessing failed (NMS, classification, etc.).
    #[error("postprocess error: {0}")]
    PostprocessError(String),

    /// Frame decode failed.
    #[error("decode error: {0}")]
    DecodeError(String),

    /// WASM algorithm execution failed.
    #[error("algorithm error: {0}")]
    AlgorithmError(String),

    /// Pipeline configuration is invalid.
    #[error("pipeline config error: {0}")]
    PipelineConfigError(String),

    /// I/O error (file system, network).
    #[error("io error: {0}")]
    IoError(String),

    /// Internal engine error (should not happen in normal operation).
    #[error("internal error: {0}")]
    InternalError(String),

    /// Engine is not initialized or has been shut down.
    #[error("engine not available")]
    EngineNotAvailable,
}

impl AiEngineError {
    /// Whether this error indicates a transient condition that may resolve on retry.
    #[inline]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            AiEngineError::Backpressure | AiEngineError::EngineNotAvailable
        )
    }
}

/// Convenience result alias for AI engine operations.
pub type AiResult<T> = Result<T, AiEngineError>;
