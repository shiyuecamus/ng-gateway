//! Inference worker pool — wraps ONNX Runtime sessions.
//!
//! Workers run on `tokio::task::spawn_blocking` to avoid blocking the
//! Tokio async runtime during GPU/CPU inference.

#[cfg(feature = "engine")]
mod inner {
    use crate::{
        decoded::DecodedFrame,
        model::registry::ModelRegistry,
        pipeline::{
            postprocess::RawInferenceOutput,
            preprocess::{CoordinateTransform, PreProcessor, PreprocessInput, PreprocessOutput},
        },
    };
    use dashmap::{mapref::entry::Entry, DashMap};
    use ndarray::{Array4, ArrayD};
    use ng_gateway_error::ai::AiEngineError;
    use ng_gateway_models::ai::model::{ModelInfo, TensorDType, TensorDesc};
    use ort::session::Session as OrtSession;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use tokio::sync::{mpsc, oneshot};
    use tracing::{debug, info, info_span};

    /// Inference worker pool with lazy model loading.
    pub struct InferencePool {
        /// Loaded ONNX Runtime session groups keyed by model id.
        session_groups: DashMap<String, Arc<SessionGroup>>,
        /// Model registry for resolving model paths.
        model_registry: Arc<ModelRegistry>,
        /// Number of intra-op threads per session.
        intra_op_threads: usize,
        /// Number of sessions to pre-warm per model.
        sessions_per_model: usize,
        /// Queue capacity per session worker.
        request_queue_capacity: usize,
    }

    /// One ONNX Runtime session wrapper.
    ///
    /// The inner `OrtSession` is behind a `parking_lot::Mutex` because
    /// `Session::run()` in ort v2 requires `&mut self`.
    pub struct InferenceSession {
        session: parking_lot::Mutex<OrtSession>,
        pub model_info: Arc<ModelInfo>,
    }

    /// A request sent to one session worker.
    struct InferRequest {
        tensor: Array4<f32>,
        response_tx: oneshot::Sender<Result<InferenceRunResult, AiEngineError>>,
    }

    /// One worker endpoint in a session group.
    struct SessionWorker {
        /// Request sender for this worker.
        tx: mpsc::Sender<InferRequest>,
        /// Approximate queued request count.
        queue_depth: Arc<AtomicUsize>,
    }

    /// Multi-session serving group for one model.
    struct SessionGroup {
        /// Session workers that run inference in parallel.
        workers: Vec<SessionWorker>,
        /// Round-robin cursor.
        rr_cursor: AtomicUsize,
    }

    /// Timing breakdown for one end-to-end inference request.
    #[derive(Debug, Clone, Copy)]
    pub struct InferTiming {
        /// Time spent in preprocessing.
        pub preprocess: Duration,
        /// Time spent waiting for session lock.
        pub infer_wait: Duration,
        /// Time spent inside ONNX Runtime `run`.
        pub infer_exec: Duration,
        /// Time spent in postprocess.
        pub postprocess: Duration,
    }

    #[derive(Debug)]
    struct InferenceRunResult {
        output: RawInferenceOutput,
        infer_wait: Duration,
        infer_exec: Duration,
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
            sessions_per_model: usize,
            request_queue_capacity: usize,
        ) -> Result<Self, AiEngineError> {
            Ok(Self {
                session_groups: DashMap::new(),
                model_registry,
                intra_op_threads,
                sessions_per_model: sessions_per_model.max(1),
                request_queue_capacity: request_queue_capacity.max(1),
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
        ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError> {
            let model_info = self
                .model_registry
                .get(model_id)
                .await
                .ok_or(AiEngineError::ModelNotFound(model_id.to_string()))?;

            // Determine input shape.
            let input_shape = input_size
                .map(|(w, h)| vec![1, 3, h as i64, w as i64])
                .unwrap_or_else(|| {
                    model_info
                        .inputs
                        .first()
                        .map(|input| input.shape.clone())
                        .unwrap_or_else(|| vec![1, 3, 640, 640])
                });
            let input_dtype = model_info
                .inputs
                .first()
                .map(|input| input.dtype)
                .unwrap_or(TensorDType::Float32);
            self.infer_with_model_spec(model_id, frame, preprocessor, &input_shape, input_dtype)
                .await
        }

        /// Run preprocessing + inference using a pre-compiled model spec.
        ///
        /// The compiled engine path should prefer this API to avoid repeated
        /// model metadata reads and shape construction per frame.
        pub async fn infer_compiled(
            &self,
            model_id: &str,
            frame: &DecodedFrame,
            preprocessor: &dyn PreProcessor,
            input_shape: &[i64],
            input_dtype: TensorDType,
        ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError> {
            self.infer_with_model_spec(model_id, frame, preprocessor, input_shape, input_dtype)
                .await
        }

        async fn infer_with_model_spec(
            &self,
            model_id: &str,
            frame: &DecodedFrame,
            preprocessor: &dyn PreProcessor,
            input_shape: &[i64],
            input_dtype: TensorDType,
        ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError> {
            let session_group = self.get_or_load_session_group(model_id).await?;

            // Preprocess.
            let preprocess_input = PreprocessInput {
                frame,
                model_input_shape: input_shape,
                model_input_dtype: input_dtype,
            };
            let preprocess_span = info_span!("preprocess_exec", model_id = model_id);
            let preprocess_start = Instant::now();
            let _preprocess_guard = preprocess_span.enter();
            let PreprocessOutput {
                tensor,
                coord_transform,
            } = preprocessor.process(preprocess_input)?;
            let preprocess_elapsed = preprocess_start.elapsed();
            drop(_preprocess_guard);

            // Dispatch to one session worker and await response.
            let worker_index = select_worker_index(&session_group);
            let worker = &session_group.workers[worker_index];
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            worker.queue_depth.fetch_add(1, Ordering::Relaxed);
            if worker
                .tx
                .send(InferRequest {
                    tensor,
                    response_tx,
                })
                .await
                .is_err()
            {
                worker.queue_depth.fetch_sub(1, Ordering::Relaxed);
                return Err(AiEngineError::InferenceError(format!(
                    "inference worker closed for model {model_id}"
                )));
            }
            let infer_result = response_rx.await.map_err(|_| {
                AiEngineError::InferenceError(format!(
                    "inference response channel closed for model {model_id}"
                ))
            })??;

            Ok((
                infer_result.output,
                coord_transform,
                InferTiming {
                    preprocess: preprocess_elapsed,
                    infer_wait: infer_result.infer_wait,
                    infer_exec: infer_result.infer_exec,
                    postprocess: Duration::from_secs(0),
                },
            ))
        }

        /// Get an existing session group or lazily build a new one.
        async fn get_or_load_session_group(
            &self,
            model_id: &str,
        ) -> Result<Arc<SessionGroup>, AiEngineError> {
            if let Some(s) = self.session_groups.get(model_id) {
                return Ok(Arc::clone(s.value()));
            }

            let model_info = self
                .model_registry
                .get(model_id)
                .await
                .ok_or(AiEngineError::ModelNotFound(model_id.into()))?;

            info!(
                model_id = model_id,
                path = %model_info.path.display(),
                sessions = self.sessions_per_model,
                queue_capacity = self.request_queue_capacity,
                "loading ONNX model session group"
            );

            let mut workers = Vec::with_capacity(self.sessions_per_model);
            let mut first_inputs: Option<Vec<TensorDesc>> = None;
            let mut first_outputs: Option<Vec<TensorDesc>> = None;

            for worker_id in 0..self.sessions_per_model {
                let session = self.load_ort_session(&model_info.path).await?;
                if first_inputs.is_none() || first_outputs.is_none() {
                    first_inputs = Some(extract_input_descs(&session));
                    first_outputs = Some(extract_output_descs(&session));
                }

                let session = Arc::new(InferenceSession {
                    session: parking_lot::Mutex::new(session),
                    model_info: Arc::clone(&model_info),
                });
                let (tx, rx) = tokio::sync::mpsc::channel(self.request_queue_capacity);
                let queue_depth = Arc::new(AtomicUsize::new(0));

                spawn_session_worker(
                    model_id.to_string(),
                    worker_id,
                    Arc::clone(&session),
                    rx,
                    Arc::clone(&queue_depth),
                );

                workers.push(SessionWorker { tx, queue_depth });
            }

            if let (Some(inputs), Some(outputs)) = (first_inputs, first_outputs) {
                self.model_registry
                    .update_tensor_info(model_id, inputs, outputs);
            }
            self.model_registry.mark_loaded(model_id);

            let group = Arc::new(SessionGroup {
                workers,
                rr_cursor: AtomicUsize::new(0),
            });

            match self.session_groups.entry(model_id.to_string()) {
                Entry::Occupied(existing) => Ok(Arc::clone(existing.get())),
                Entry::Vacant(vacant) => {
                    vacant.insert(Arc::clone(&group));
                    debug!(
                        model_id = model_id,
                        "ONNX model session group loaded successfully"
                    );
                    Ok(group)
                }
            }
        }

        /// Load one ONNX Runtime session for the given model path.
        async fn load_ort_session(
            &self,
            path: &std::path::Path,
        ) -> Result<OrtSession, AiEngineError> {
            let intra_threads = self.intra_op_threads;
            let path = path.to_path_buf();
            tokio::task::spawn_blocking(move || -> Result<OrtSession, AiEngineError> {
                let session = OrtSession::builder()
                    .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                    .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                    .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                    .with_intra_threads(intra_threads)
                    .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                    .commit_from_file(&path)
                    .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?;
                Ok(session)
            })
            .await
            .map_err(|e| AiEngineError::ModelLoadError(format!("join error: {e}")))?
        }

        /// Unload a model session from the pool.
        pub fn unload(&self, model_id: &str) {
            self.session_groups.remove(model_id);
            self.model_registry.mark_unloaded(model_id);
        }

        /// Explicitly load model session into the pool.
        pub async fn load(&self, model_id: &str) -> Result<(), AiEngineError> {
            let _ = self.get_or_load_session_group(model_id).await?;
            Ok(())
        }

        /// Number of models currently loaded in memory.
        pub fn loaded_count(&self) -> usize {
            self.session_groups.len()
        }
    }

    /// Select one worker with round-robin fast path and shortest-queue fallback.
    fn select_worker_index(group: &SessionGroup) -> usize {
        let worker_count = group.workers.len();
        if worker_count <= 1 {
            return 0;
        }

        let rr_index = group.rr_cursor.fetch_add(1, Ordering::Relaxed) % worker_count;
        let rr_depth = group.workers[rr_index].queue_depth.load(Ordering::Relaxed);

        let mut min_index = rr_index;
        let mut min_depth = rr_depth;
        for (idx, worker) in group.workers.iter().enumerate() {
            let depth = worker.queue_depth.load(Ordering::Relaxed);
            if depth < min_depth {
                min_depth = depth;
                min_index = idx;
            }
        }

        if rr_depth <= min_depth.saturating_add(1) {
            rr_index
        } else {
            min_index
        }
    }

    /// Spawn one session worker loop.
    fn spawn_session_worker(
        model_id: String,
        worker_id: usize,
        session: Arc<InferenceSession>,
        mut rx: mpsc::Receiver<InferRequest>,
        queue_depth: Arc<AtomicUsize>,
    ) {
        tokio::spawn(async move {
            while let Some(request) = rx.recv().await {
                let InferRequest {
                    tensor,
                    response_tx,
                } = request;
                queue_depth.fetch_sub(1, Ordering::Relaxed);
                let session = Arc::clone(&session);
                let result =
                    tokio::task::spawn_blocking(move || run_ort_inference(&session, tensor))
                        .await
                        .map_err(|e| {
                            AiEngineError::InferenceError(format!(
                                "join error in worker {worker_id} for model {model_id}: {e}"
                            ))
                        })
                        .and_then(|inner| inner);

                let _ = response_tx.send(result);
            }
        });
    }

    /// Execute ONNX inference synchronously (called from spawn_blocking).
    fn run_ort_inference(
        session: &InferenceSession,
        tensor: Array4<f32>,
    ) -> Result<InferenceRunResult, AiEngineError> {
        let input_name = session
            .model_info
            .inputs
            .first()
            .map(|t| t.name.as_str())
            .unwrap_or("input");

        let input_value = ort::value::Tensor::from_array(tensor)
            .map_err(|e| AiEngineError::InferenceError(format!("input tensor error: {e}")))?;

        let infer_wait_span = info_span!("infer_wait");
        let infer_wait_start = Instant::now();
        let _infer_wait_guard = infer_wait_span.enter();
        let mut sess = session.session.lock();
        let infer_wait_elapsed = infer_wait_start.elapsed();
        drop(_infer_wait_guard);

        // Capture output names before the mutable borrow from run()
        let output_names: Vec<String> = sess
            .outputs()
            .iter()
            .map(|o| o.name().to_string())
            .collect();

        let infer_exec_span = info_span!("infer_exec");
        let infer_exec_start = Instant::now();
        let _infer_exec_guard = infer_exec_span.enter();
        let outputs = sess
            .run(ort::inputs![input_name => input_value])
            .map_err(|e| AiEngineError::InferenceError(e.to_string()))?;
        let infer_exec_elapsed = infer_exec_start.elapsed();
        drop(_infer_exec_guard);

        let mut result_tensors = Vec::with_capacity(output_names.len());
        for (i, name) in output_names.into_iter().enumerate() {
            let extracted: ArrayD<f32> = outputs[i]
                .try_extract_array::<f32>()
                .map_err(|e| AiEngineError::InferenceError(format!("output extract: {e}")))?
                .to_owned();
            result_tensors.push((name, extracted));
        }

        Ok(InferenceRunResult {
            output: RawInferenceOutput {
                tensors: result_tensors,
            },
            infer_wait: infer_wait_elapsed,
            infer_exec: infer_exec_elapsed,
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
