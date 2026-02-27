//! Public API trait for the AI Processing Engine.
//!
//! This trait is the sole contract between camera drivers and the engine.
//! It is injected into drivers via [`SouthwardInitContext`] extensions,
//! allowing zero-copy frame submission and async result retrieval.

use super::{
    algorithm::{
        AlgorithmTestInput, AlgorithmTestResult, AlgorithmUploadMetadata, WasmAlgorithmInfo,
    },
    model::{ModelInfo, ModelUpdateRequest, ModelUploadMetadata},
    pipeline::{PipelineConfig, PipelineUpsertRequest},
    types::{AnalysisResult, EngineStatus, FrameAnalysisRequest, PipelineId, ProcessorInfo},
};
use bytes::Bytes;
use downcast_rs::{impl_downcast, DowncastSync};
use ng_gateway_error::ai::AiEngineError;
use std::sync::Arc;

impl_downcast!(sync AiEngineApi);

/// The public API that the AI Processing Engine exposes to southward drivers.
///
/// # Thread Safety
///
/// All methods are `&self` and internally synchronized. Implementations must be
/// safe to call from multiple driver instances concurrently.
///
/// # Backpressure
///
/// When the engine cannot accept more frames, [`analyze_frame`] returns
/// [`AiEngineError::Backpressure`]. Callers should drop the frame and
/// continue (best-effort semantics for real-time video).
#[async_trait::async_trait]
pub trait AiEngineApi: DowncastSync + Send + Sync + 'static {
    /// Submit a video frame for AI analysis.
    async fn analyze_frame(
        &self,
        request: FrameAnalysisRequest,
    ) -> Result<AnalysisResult, AiEngineError>;

    /// Check if the engine has capacity to accept a new frame (non-blocking).
    fn has_capacity(&self, pipeline_id: &PipelineId) -> bool;

    /// Query available models and their status.
    async fn list_models(&self) -> Result<Vec<Arc<ModelInfo>>, AiEngineError>;

    /// Get the pipeline configuration for a given channel.
    async fn get_pipeline(
        &self,
        channel_id: i32,
    ) -> Result<Option<Arc<PipelineConfig>>, AiEngineError>;

    /// Register (or replace) a pipeline for a channel.
    ///
    /// Implementations must validate stage ordering and reject invalid DAGs.
    fn register_pipeline(
        &self,
        channel_id: i32,
        config: PipelineConfig,
    ) -> Result<(), AiEngineError>;

    /// Remove the pipeline for a channel.
    fn unregister_pipeline(&self, channel_id: i32);

    /// Get the latest analysis result for a channel (for snapshot API).
    async fn get_latest_result(
        &self,
        channel_id: i32,
    ) -> Result<Option<AnalysisResult>, AiEngineError>;

    /// Get model info by identifier.
    async fn get_model(&self, model_id: &str) -> Result<Option<Arc<ModelInfo>>, AiEngineError>;

    /// Upload and register an ONNX model with metadata.
    async fn upload_model(
        &self,
        onnx_bytes: Bytes,
        metadata: ModelUploadMetadata,
    ) -> Result<Arc<ModelInfo>, AiEngineError>;

    /// Update mutable model configuration.
    async fn update_model(
        &self,
        model_id: &str,
        request: ModelUpdateRequest,
    ) -> Result<Arc<ModelInfo>, AiEngineError>;

    /// Delete a model and unload runtime session.
    async fn delete_model(&self, model_id: &str) -> Result<(), AiEngineError>;

    /// Explicitly load a model into inference pool.
    async fn load_model(&self, model_id: &str) -> Result<(), AiEngineError>;

    /// Explicitly unload a model from inference pool.
    async fn unload_model(&self, model_id: &str) -> Result<(), AiEngineError>;

    /// List all registered pipeline configurations with their bound channel IDs.
    async fn list_pipelines(&self) -> Result<Vec<(i32, Arc<PipelineConfig>)>, AiEngineError>;

    /// Create or replace a pipeline binding.
    async fn upsert_pipeline(&self, request: PipelineUpsertRequest) -> Result<(), AiEngineError>;

    /// Delete a pipeline binding by channel ID.
    async fn delete_pipeline(&self, channel_id: i32) -> Result<(), AiEngineError>;

    /// Get an aggregated engine status snapshot for monitoring/API.
    async fn get_engine_status(&self) -> Result<EngineStatus, AiEngineError>;

    /// List built-in preprocessors with their metadata and parameters.
    fn list_preprocessors(&self) -> Vec<ProcessorInfo>;

    /// List built-in postprocessors with their metadata and parameters.
    fn list_postprocessors(&self) -> Vec<ProcessorInfo>;

    // ── Algorithm management (Phase 2) ───────────────────────────

    /// List all registered WASM algorithms.
    async fn list_algorithms(&self) -> Result<Vec<Arc<WasmAlgorithmInfo>>, AiEngineError>;

    /// Get a single algorithm by identifier.
    async fn get_algorithm(
        &self,
        algorithm_id: &str,
    ) -> Result<Option<Arc<WasmAlgorithmInfo>>, AiEngineError>;

    /// Upload and register a new WASM algorithm module.
    async fn upload_algorithm(
        &self,
        wasm_bytes: Bytes,
        metadata: AlgorithmUploadMetadata,
    ) -> Result<Arc<WasmAlgorithmInfo>, AiEngineError>;

    /// Delete a registered algorithm and remove its files.
    async fn delete_algorithm(&self, algorithm_id: &str) -> Result<(), AiEngineError>;

    /// Test an algorithm with mock data.
    async fn test_algorithm(
        &self,
        algorithm_id: &str,
        test_input: AlgorithmTestInput,
    ) -> Result<AlgorithmTestResult, AiEngineError>;
}
