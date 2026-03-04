//! ONNX Runtime inference backend.
//!
//! Wraps ONNX Runtime sessions in a multi-worker pool with lazy loading.
//! Workers run on `tokio::task::spawn_blocking` to avoid blocking the
//! async runtime during CPU/GPU inference.

use super::backend::{InferTiming, ModelBackend};
use crate::{
    decoded::DecodedFrame,
    pipeline::{
        postprocess::RawInferenceOutput,
        preprocess::{CoordinateTransform, PreProcessor, PreprocessInput, PreprocessOutput},
    },
};
use dashmap::DashMap;
use ndarray::{Array4, ArrayD};
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::enums::ai::{ModelFormat, TensorDType};
use ort::session::Session as OrtSession;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, info_span};

/// ONNX Runtime inference backend with session pooling.
pub struct OnnxBackend {
    /// Loaded session groups keyed by model id (i32).
    session_groups: DashMap<i32, Arc<SessionGroup>>,
    /// Per-model async load guards to deduplicate concurrent first-load requests.
    load_guards: DashMap<i32, Arc<tokio::sync::Mutex<()>>>,
    /// Number of intra-op threads per session.
    intra_op_threads: usize,
    /// Number of sessions to pre-warm per model.
    sessions_per_model: usize,
    /// Queue capacity per session worker.
    request_queue_capacity: usize,
}

/// One ONNX Runtime session wrapper.
struct InferenceSession {
    session: parking_lot::Mutex<OrtSession>,
}

/// A request sent to one session worker.
struct InferRequest {
    tensor: Array4<f32>,
    response_tx: oneshot::Sender<Result<InferenceRunResult, AiEngineError>>,
}

/// One worker endpoint in a session group.
struct SessionWorker {
    tx: mpsc::Sender<InferRequest>,
    queue_depth: Arc<AtomicUsize>,
}

/// Multi-session serving group for one model.
struct SessionGroup {
    workers: Vec<SessionWorker>,
    rr_cursor: AtomicUsize,
}

#[derive(Debug)]
struct InferenceRunResult {
    output: RawInferenceOutput,
    infer_wait: Duration,
    infer_exec: Duration,
}

impl OnnxBackend {
    /// Create a new ONNX backend.
    pub fn new(
        intra_op_threads: usize,
        sessions_per_model: usize,
        request_queue_capacity: usize,
    ) -> Self {
        Self {
            session_groups: DashMap::new(),
            load_guards: DashMap::new(),
            intra_op_threads,
            sessions_per_model: sessions_per_model.max(1),
            request_queue_capacity: request_queue_capacity.max(1),
        }
    }

    /// Get an existing session group or lazily build a new one.
    async fn get_or_load(
        &self,
        model_id: i32,
        path: &Path,
    ) -> Result<Arc<SessionGroup>, AiEngineError> {
        if let Some(s) = self.session_groups.get(&model_id) {
            return Ok(Arc::clone(s.value()));
        }

        let load_guard = self
            .load_guards
            .entry(model_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _load_lock = load_guard.lock().await;

        // Re-check after acquiring the load lock.
        if let Some(s) = self.session_groups.get(&model_id) {
            return Ok(Arc::clone(s.value()));
        }

        info!(
            model_id,
            path = %path.display(),
            sessions = self.sessions_per_model,
            "loading ONNX model session group"
        );

        let mut workers = Vec::with_capacity(self.sessions_per_model);
        for worker_id in 0..self.sessions_per_model {
            let session = self.load_ort_session(path).await?;
            let session = Arc::new(InferenceSession {
                session: parking_lot::Mutex::new(session),
            });
            let (tx, rx) = mpsc::channel(self.request_queue_capacity);
            let queue_depth = Arc::new(AtomicUsize::new(0));

            spawn_session_worker(
                model_id,
                worker_id,
                Arc::clone(&session),
                rx,
                Arc::clone(&queue_depth),
            );
            workers.push(SessionWorker { tx, queue_depth });
        }

        let group = Arc::new(SessionGroup {
            workers,
            rr_cursor: AtomicUsize::new(0),
        });

        self.session_groups
            .entry(model_id)
            .or_insert_with(|| Arc::clone(&group));

        debug!(model_id, "ONNX session group loaded");
        self.session_groups
            .get(&model_id)
            .map(|existing| Arc::clone(existing.value()))
            .ok_or_else(|| {
                AiEngineError::InternalError(format!(
                    "session group missing right after insert for model {model_id}"
                ))
            })
    }

    /// Load one ONNX Runtime session.
    async fn load_ort_session(&self, path: &Path) -> Result<OrtSession, AiEngineError> {
        let intra_threads = self.intra_op_threads;
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            OrtSession::builder()
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                .with_intra_threads(intra_threads)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                .commit_from_file(&path)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))
        })
        .await
        .map_err(|e| AiEngineError::ModelLoadError(format!("join error: {e}")))?
    }
}

#[async_trait::async_trait]
impl ModelBackend for OnnxBackend {
    fn format(&self) -> ModelFormat {
        ModelFormat::Onnx
    }

    async fn load(&self, model_id: i32, path: &Path) -> Result<(), AiEngineError> {
        let _ = self.get_or_load(model_id, path).await?;
        Ok(())
    }

    fn unload(&self, model_id: i32) {
        self.session_groups.remove(&model_id);
        self.load_guards.remove(&model_id);
    }

    fn is_loaded(&self, model_id: i32) -> bool {
        self.session_groups.contains_key(&model_id)
    }

    async fn infer(
        &self,
        model_id: i32,
        frame: &DecodedFrame,
        preprocessor: &dyn PreProcessor,
        input_shape: &[i64],
        input_dtype: TensorDType,
    ) -> Result<(RawInferenceOutput, CoordinateTransform, InferTiming), AiEngineError> {
        // Lazy-load on first inference if not already loaded.
        // The actual path lookup happens upstream in AiEngine before calling backend.
        let session_group = self
            .session_groups
            .get(&model_id)
            .map(|s| Arc::clone(s.value()))
            .ok_or(AiEngineError::ModelNotFound(model_id.to_string()))?;

        // Preprocess
        let preprocess_input = PreprocessInput {
            frame,
            model_input_shape: input_shape,
            model_input_dtype: input_dtype,
        };
        let preprocess_span = info_span!("preprocess_exec", model_id);
        let preprocess_start = Instant::now();
        let _guard = preprocess_span.enter();
        let PreprocessOutput {
            tensor,
            coord_transform,
        } = preprocessor.process(preprocess_input)?;
        let preprocess_elapsed = preprocess_start.elapsed();
        drop(_guard);

        // Dispatch to worker
        let worker_index = select_worker_index(&session_group);
        let worker = &session_group.workers[worker_index];
        let (response_tx, response_rx) = oneshot::channel();
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

        let result = response_rx.await.map_err(|_| {
            AiEngineError::InferenceError(format!("response channel closed for model {model_id}"))
        })??;

        Ok((
            result.output,
            coord_transform,
            InferTiming {
                preprocess: preprocess_elapsed,
                infer_wait: result.infer_wait,
                infer_exec: result.infer_exec,
                postprocess: Duration::from_secs(0),
            },
        ))
    }

    fn loaded_count(&self) -> usize {
        self.session_groups.len()
    }

    fn estimated_memory_bytes(&self) -> u64 {
        0
    }
}

/// Select one worker with round-robin + shortest-queue fallback.
fn select_worker_index(group: &SessionGroup) -> usize {
    let n = group.workers.len();
    if n <= 1 {
        return 0;
    }
    let rr = group.rr_cursor.fetch_add(1, Ordering::Relaxed) % n;
    let rr_depth = group.workers[rr].queue_depth.load(Ordering::Relaxed);
    let mut best_idx = rr;
    let mut best_depth = rr_depth;
    for (idx, w) in group.workers.iter().enumerate() {
        let d = w.queue_depth.load(Ordering::Relaxed);
        if d < best_depth {
            best_depth = d;
            best_idx = idx;
        }
    }
    if rr_depth <= best_depth.saturating_add(1) {
        rr
    } else {
        best_idx
    }
}

/// Spawn one session worker loop.
fn spawn_session_worker(
    model_id: i32,
    worker_id: usize,
    session: Arc<InferenceSession>,
    mut rx: mpsc::Receiver<InferRequest>,
    queue_depth: Arc<AtomicUsize>,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            queue_depth.fetch_sub(1, Ordering::Relaxed);
            let sess = Arc::clone(&session);
            let result = tokio::task::spawn_blocking(move || run_ort_inference(&sess, req.tensor))
                .await
                .map_err(|e| {
                    AiEngineError::InferenceError(format!(
                        "join error in worker {worker_id} for model {model_id}: {e}"
                    ))
                })
                .and_then(|r| r);
            let _ = req.response_tx.send(result);
        }
    });
}

/// Execute ONNX inference synchronously (called from spawn_blocking).
fn run_ort_inference(
    session: &InferenceSession,
    tensor: Array4<f32>,
) -> Result<InferenceRunResult, AiEngineError> {
    let wait_start = Instant::now();
    let mut sess = session.session.lock();
    let infer_wait = wait_start.elapsed();

    let input_name = sess
        .inputs()
        .first()
        .map(|i| i.name().to_string())
        .unwrap_or("input".to_string());

    let output_names: Vec<String> = sess
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();

    let input_value = ort::value::Tensor::from_array(tensor)
        .map_err(|e| AiEngineError::InferenceError(format!("input tensor error: {e}")))?;

    let exec_start = Instant::now();
    let outputs = sess
        .run(ort::inputs![input_name.as_str() => input_value])
        .map_err(|e| AiEngineError::InferenceError(e.to_string()))?;
    let infer_exec = exec_start.elapsed();

    let mut tensors = Vec::with_capacity(output_names.len());
    for (i, name) in output_names.into_iter().enumerate() {
        let arr: ArrayD<f32> = outputs[i]
            .try_extract_array::<f32>()
            .map_err(|e| AiEngineError::InferenceError(format!("output extract: {e}")))?
            .to_owned();
        tensors.push((name, arr));
    }

    Ok(InferenceRunResult {
        output: RawInferenceOutput { tensors },
        infer_wait,
        infer_exec,
    })
}
