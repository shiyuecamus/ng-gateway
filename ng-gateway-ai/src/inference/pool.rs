//! Inference worker pool — wraps ONNX Runtime sessions.
//!
//! Workers run on `tokio::task::spawn_blocking` to avoid blocking the
//! Tokio async runtime during GPU/CPU inference.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        decoded_frame::DecodedFrame,
        model::registry::ModelRegistry,
        pipeline::{
            postprocess::RawInferenceOutput,
            preprocess::{PreProcessor, PreprocessInput, PreprocessOutput},
        },
    };
    use dashmap::DashMap;
    use ndarray::{Array4, ArrayD};
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::model::{ModelInfo, TensorDType, TensorDesc};
    use ort::session::Session as OrtSession;
    use std::sync::Arc;
    use tracing::{debug, info};

    /// Inference worker pool with lazy model loading.
    pub struct InferencePool {
        /// Loaded ONNX Runtime sessions keyed by model_id.
        sessions: DashMap<String, Arc<InferenceSession>>,
        /// Model registry for resolving model paths.
        model_registry: Arc<ModelRegistry>,
        /// Number of intra-op threads per session.
        intra_op_threads: usize,
    }

    /// A single ONNX Runtime inference session wrapper.
    ///
    /// The inner `OrtSession` is behind a `parking_lot::Mutex` because
    /// `Session::run()` in ort v2 requires `&mut self`.
    pub struct InferenceSession {
        session: parking_lot::Mutex<OrtSession>,
        pub model_info: ModelInfo,
    }

    // `OrtSession` is `Send + Sync` natively in ort v2; `parking_lot::Mutex`
    // provides synchronized mutable access. We keep the explicit impls in case
    // `ModelInfo` ever contains a non-Send/Sync field.
    unsafe impl Send for InferenceSession {}
    unsafe impl Sync for InferenceSession {}

    impl InferencePool {
        /// Create a new inference pool.
        pub async fn new(
            model_registry: Arc<ModelRegistry>,
            intra_op_threads: usize,
        ) -> Result<Self, AiEngineError> {
            Ok(Self {
                sessions: DashMap::new(),
                model_registry,
                intra_op_threads,
            })
        }

        /// Run preprocessing + inference on a decoded frame.
        ///
        /// Returns the raw inference output tensors for postprocessing.
        pub async fn infer(
            &self,
            model_id: &str,
            frame: &DecodedFrame,
            preprocessor: &dyn PreProcessor,
            input_size: Option<(u32, u32)>,
        ) -> Result<
            (
                RawInferenceOutput,
                crate::pipeline::preprocess::CoordinateTransform,
            ),
            AiEngineError,
        > {
            let session = self.get_or_load_session(model_id).await?;

            // Determine input shape
            let input_shape: Vec<i64> = if let Some((w, h)) = input_size {
                vec![1, 3, h as i64, w as i64]
            } else if let Some(first_input) = session.model_info.inputs.first() {
                first_input.shape.clone()
            } else {
                vec![1, 3, 640, 640] // fallback
            };

            let input_dtype = session
                .model_info
                .inputs
                .first()
                .map(|t| t.dtype)
                .unwrap_or(TensorDType::Float32);

            // Preprocess
            let preprocess_input = PreprocessInput {
                frame,
                model_input_shape: &input_shape,
                model_input_dtype: input_dtype,
            };
            let PreprocessOutput {
                tensor,
                coord_transform,
            } = preprocessor.process(preprocess_input)?;

            // Run inference on blocking thread
            let session = Arc::clone(&session);
            let output = tokio::task::spawn_blocking(move || run_ort_inference(&session, tensor))
                .await
                .map_err(|e| AiEngineError::InferenceError(format!("join error: {e}")))??;

            Ok((output, coord_transform))
        }

        /// Get an existing session or lazily load the model.
        async fn get_or_load_session(
            &self,
            model_id: &str,
        ) -> Result<Arc<InferenceSession>, AiEngineError> {
            if let Some(s) = self.sessions.get(model_id) {
                return Ok(Arc::clone(s.value()));
            }

            let model_info = self
                .model_registry
                .get(model_id)
                .await
                .ok_or(AiEngineError::ModelNotFound(model_id.into()))?;

            info!(model_id = model_id, path = %model_info.path.display(), "loading ONNX model");

            let intra_threads = self.intra_op_threads;
            let path = model_info.path.clone();
            let session =
                tokio::task::spawn_blocking(move || -> Result<OrtSession, AiEngineError> {
                    let session = OrtSession::builder()
                        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                        .with_optimization_level(
                            ort::session::builder::GraphOptimizationLevel::Level3,
                        )
                        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                        .with_intra_threads(intra_threads)
                        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                        .commit_from_file(&path)
                        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?;
                    Ok(session)
                })
                .await
                .map_err(|e| AiEngineError::ModelLoadError(format!("join error: {e}")))??;

            // Extract tensor metadata from the loaded session
            let inputs = extract_input_descs(&session);
            let outputs = extract_output_descs(&session);

            // Update registry with probed shapes
            self.model_registry
                .update_tensor_info(model_id, inputs, outputs);
            self.model_registry.mark_loaded(model_id);

            let wrapper = Arc::new(InferenceSession {
                session: parking_lot::Mutex::new(session),
                model_info: self
                    .model_registry
                    .get(model_id)
                    .await
                    .unwrap_or(model_info),
            });

            self.sessions
                .insert(model_id.to_string(), Arc::clone(&wrapper));

            debug!(model_id = model_id, "ONNX model loaded successfully");
            Ok(wrapper)
        }

        /// Unload a model session from the pool.
        pub fn unload(&self, model_id: &str) {
            self.sessions.remove(model_id);
            self.model_registry.mark_unloaded(model_id);
        }

        /// Number of models currently loaded in memory.
        pub fn loaded_count(&self) -> usize {
            self.sessions.len()
        }
    }

    /// Execute ONNX inference synchronously (called from spawn_blocking).
    fn run_ort_inference(
        session: &InferenceSession,
        tensor: Array4<f32>,
    ) -> Result<RawInferenceOutput, AiEngineError> {
        let input_name = session
            .model_info
            .inputs
            .first()
            .map(|t| t.name.as_str())
            .unwrap_or("input");

        let input_value = ort::value::Tensor::from_array(tensor)
            .map_err(|e| AiEngineError::InferenceError(format!("input tensor error: {e}")))?;

        let mut sess = session.session.lock();

        // Capture output names before the mutable borrow from run()
        let output_names: Vec<String> = sess
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();

        let outputs = sess
            .run(ort::inputs![input_name => input_value])
            .map_err(|e| AiEngineError::InferenceError(e.to_string()))?;

        let mut result_tensors = Vec::with_capacity(output_names.len());
        for (i, name) in output_names.into_iter().enumerate() {
            let extracted: ArrayD<f32> = outputs[i]
                .try_extract_array::<f32>()
                .map_err(|e| AiEngineError::InferenceError(format!("output extract: {e}")))?
                .to_owned();
            result_tensors.push((name, extracted));
        }

        Ok(RawInferenceOutput {
            tensors: result_tensors,
        })
    }

    /// Extract input tensor descriptors from a loaded ONNX session.
    fn extract_input_descs(session: &OrtSession) -> Vec<TensorDesc> {
        session
            .inputs()
            .iter()
            .map(|input| {
                let shape = input
                    .dtype()
                    .tensor_shape()
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                TensorDesc {
                    name: input.name().to_string(),
                    shape,
                    dtype: TensorDType::Float32,
                }
            })
            .collect()
    }

    /// Extract output tensor descriptors from a loaded ONNX session.
    fn extract_output_descs(session: &OrtSession) -> Vec<TensorDesc> {
        session
            .outputs()
            .iter()
            .map(|output| {
                let shape = output
                    .dtype()
                    .tensor_shape()
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                TensorDesc {
                    name: output.name().to_string(),
                    shape,
                    dtype: TensorDType::Float32,
                }
            })
            .collect()
    }
}

#[cfg(feature = "engine")]
pub use inner::*;
