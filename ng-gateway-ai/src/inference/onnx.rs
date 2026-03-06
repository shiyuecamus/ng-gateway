//! ONNX Runtime inference backend.
//!
//! Wraps ONNX Runtime sessions in a multi-worker pool with lazy loading.
//! Workers run on `tokio::task::spawn_blocking` to avoid blocking the
//! async runtime during CPU/GPU inference.
//!
//! # Preprocessing Contract
//!
//! This backend expects `PreprocessOutput::CpuTensor` — an `Array4<f32>`
//! in NCHW layout. Preprocessing is performed by the Engine layer before
//! calling `infer()`. Receiving `DeviceMemory` is a routing error.

use super::{backend::InferTiming, backend::ModelBackend, RawInferenceOutput};
use crate::pipeline::preprocess::PreprocessOutput;
use dashmap::DashMap;
use ndarray::{Array4, ArrayD};
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::enums::ai::ModelFormat;
use ort::session::{builder::SessionBuilder, Session as OrtSession};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

/// ONNX Runtime execution provider configuration.
///
/// Determines which hardware backend is used for inference. When the
/// requested provider is not available at runtime, the backend falls
/// back to CPU automatically and logs a warning.
#[derive(Debug, Clone)]
pub enum ExecutionProviderKind {
    /// Default CPU-only inference.
    Cpu,
    /// NVIDIA CUDA GPU acceleration.
    #[cfg(feature = "cuda-ep")]
    Cuda,
    /// NVIDIA TensorRT optimized inference (implies CUDA fallback).
    #[cfg(feature = "tensorrt-ep")]
    TensorRt,
    /// Intel OpenVINO acceleration (CPU / iGPU / VPU).
    #[cfg(feature = "openvino-ep")]
    OpenVino,
}

/// ONNX Runtime inference backend with session pooling and GPU EP support.
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
    /// Configured execution provider.
    execution_provider: String,
    /// The EP that was actually registered after runtime probing.
    /// May differ from `execution_provider` if the requested EP
    /// was unavailable and fell back to CPU.
    effective_provider: parking_lot::Mutex<String>,
}

/// One ONNX Runtime session wrapper.
struct InferenceSession {
    session: parking_lot::Mutex<OrtSession>,
    input_name: String,
    output_names: Arc<[String]>,
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
    /// Create a new ONNX backend with the specified execution provider.
    ///
    /// `execution_provider` accepts: `"cpu"`, `"cuda"`, `"tensorrt"`, `"openvino"`.
    /// If the requested EP is not available at runtime, the backend falls back
    /// to CPU automatically and logs a warning.
    pub fn new(
        intra_op_threads: usize,
        sessions_per_model: usize,
        request_queue_capacity: usize,
        execution_provider: &str,
    ) -> Self {
        Self {
            session_groups: DashMap::new(),
            load_guards: DashMap::new(),
            intra_op_threads,
            sessions_per_model: sessions_per_model.max(1),
            request_queue_capacity: request_queue_capacity.max(1),
            execution_provider: execution_provider.to_string(),
            effective_provider: parking_lot::Mutex::new("cpu".to_string()),
        }
    }

    /// Returns the execution provider that is actually in use after
    /// runtime probing and possible fallback.
    pub fn effective_provider(&self) -> String {
        self.effective_provider.lock().clone()
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
            ep = %self.execution_provider,
            "loading ONNX model session group"
        );

        let mut workers = Vec::with_capacity(self.sessions_per_model);
        for worker_id in 0..self.sessions_per_model {
            let (ort_session, effective_ep) = self.load_ort_session(path).await?;

            // Record the effective provider from the first session.
            if worker_id == 0 {
                *self.effective_provider.lock() = effective_ep.clone();
                if effective_ep != self.execution_provider {
                    tracing::warn!(
                        model_id,
                        requested = %self.execution_provider,
                        effective = %effective_ep,
                        "execution provider fallback occurred"
                    );
                } else {
                    info!(model_id, ep = %effective_ep, "execution provider registered");
                }
            }

            let session = Arc::new(ort_session);
            let (tx, rx) = mpsc::channel(self.request_queue_capacity);
            let queue_depth = Arc::new(AtomicUsize::new(0));

            spawn_session_worker(
                model_id,
                worker_id,
                Arc::clone(&session),
                rx,
                Arc::clone(&queue_depth),
            )?;
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

    /// Load one ONNX Runtime session with the configured execution provider.
    ///
    /// EP registration is best-effort: if the requested provider fails to
    /// register (missing runtime library, unsupported operators, etc.),
    /// ONNX Runtime automatically falls back to CPU. The effective provider
    /// is logged and recorded.
    async fn load_ort_session(
        &self,
        path: &Path,
    ) -> Result<(InferenceSession, String), AiEngineError> {
        let intra_threads = self.intra_op_threads;
        let path = path.to_path_buf();
        // Use the effective provider: after OOM degradation this will be
        // "cpu" even if the original config was "cuda"/"tensorrt".
        let ep_name = self.effective_provider.lock().clone();
        tokio::task::spawn_blocking(move || {
            let builder = OrtSession::builder()
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
                .with_intra_threads(intra_threads)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?;

            let (builder, effective_ep) = apply_execution_provider(builder, &ep_name)?;

            let session = builder
                .commit_from_file(&path)
                .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?;

            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .unwrap_or_else(|| "input".to_string());
            let output_names: Arc<[String]> = session
                .outputs()
                .iter()
                .map(|o| o.name().to_string())
                .collect::<Vec<_>>()
                .into();

            Ok((
                InferenceSession {
                    session: parking_lot::Mutex::new(session),
                    input_name,
                    output_names,
                },
                effective_ep,
            ))
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

    fn supports_dma_input(&self) -> bool {
        false
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
        input: PreprocessOutput,
    ) -> Result<(RawInferenceOutput, InferTiming), AiEngineError> {
        let session_group = self
            .session_groups
            .get(&model_id)
            .map(|s| Arc::clone(s.value()))
            .ok_or(AiEngineError::ModelNotFound(model_id.to_string()))?;

        let tensor = input.into_cpu_tensor().map_err(|_| {
            AiEngineError::InferenceError(
                "OnnxBackend received DeviceMemory input — this is a routing error; \
                 the Engine should produce CpuTensor for ONNX models"
                    .into(),
            )
        })?;

        let result = dispatch_to_worker(&session_group, tensor, model_id).await;

        // GPU OOM detection: if inference failed with an OOM-like error
        // and we're running a GPU EP, unload the model so the next call
        // reloads with the effective provider (degraded to CPU).
        if let Err(ref e) = result {
            if is_gpu_oom_error(e) && self.execution_provider != "cpu" {
                tracing::warn!(
                    model_id,
                    ep = %self.execution_provider,
                    "GPU OOM detected, degrading model to CPU EP"
                );
                self.unload(model_id);
                *self.effective_provider.lock() = "cpu".to_string();
            }
        }

        let run_result = result?;
        Ok((
            run_result.output,
            InferTiming {
                infer_wait: run_result.infer_wait,
                infer_exec: run_result.infer_exec,
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

/// Dispatch a tensor to a session worker and await the result.
async fn dispatch_to_worker(
    session_group: &SessionGroup,
    tensor: Array4<f32>,
    model_id: i32,
) -> Result<InferenceRunResult, AiEngineError> {
    let worker_index = select_worker_index(session_group);
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

    response_rx.await.map_err(|_| {
        AiEngineError::InferenceError(format!("response channel closed for model {model_id}"))
    })?
}

/// Detect GPU out-of-memory errors from ONNX Runtime error messages.
///
/// CUDA and TensorRT OOM errors typically contain keywords like
/// "out of memory", "CUDA_ERROR_OUT_OF_MEMORY", or "cudaMalloc failed".
fn is_gpu_oom_error(err: &AiEngineError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("out of memory")
        || msg.contains("cuda_error_out_of_memory")
        || msg.contains("cudamalloc failed")
        || msg.contains("oom")
        || msg.contains("insufficient memory")
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
) -> Result<(), AiEngineError> {
    std::thread::Builder::new()
        .name(format!("onnx-worker-{model_id}-{worker_id}"))
        .spawn(move || {
            while let Some(req) = rx.blocking_recv() {
                queue_depth.fetch_sub(1, Ordering::Relaxed);
                let result = run_ort_inference(&session, req.tensor);
                let _ = req.response_tx.send(result);
            }
            debug!(model_id, worker_id, "ONNX worker thread exited");
        })
        .map(|_| ())
        .map_err(|e| {
            AiEngineError::InferenceError(format!(
                "failed to spawn ONNX worker thread model_id={model_id} worker_id={worker_id}: {e}"
            ))
        })
}

/// Execute ONNX inference synchronously (called from spawn_blocking).
fn run_ort_inference(
    session: &InferenceSession,
    tensor: Array4<f32>,
) -> Result<InferenceRunResult, AiEngineError> {
    let wait_start = Instant::now();
    let mut sess = session.session.lock();
    let infer_wait = wait_start.elapsed();

    let input_value = ort::value::Tensor::from_array(tensor)
        .map_err(|e| AiEngineError::InferenceError(format!("input tensor error: {e}")))?;

    let exec_start = Instant::now();
    let outputs = sess
        .run(ort::inputs![session.input_name.as_str() => input_value])
        .map_err(|e| AiEngineError::InferenceError(e.to_string()))?;
    let infer_exec = exec_start.elapsed();

    let mut tensors = Vec::with_capacity(session.output_names.len());
    for name in session.output_names.iter() {
        let output = outputs.get(name.as_str()).ok_or_else(|| {
            AiEngineError::InferenceError(format!(
                "ONNX output missing by name '{name}', available outputs={}",
                outputs.len()
            ))
        })?;
        let arr: ArrayD<f32> = output
            .try_extract_array::<f32>()
            .map_err(|e| AiEngineError::InferenceError(format!("output extract: {e}")))?
            .to_owned();
        tensors.push((name.clone(), arr));
    }

    Ok(InferenceRunResult {
        output: RawInferenceOutput { tensors },
        infer_wait,
        infer_exec,
    })
}

/// Apply the configured execution provider to a session builder.
///
/// Consumes and returns the builder because `with_execution_providers()`
/// takes `self` by value. Returns `(builder, effective_ep_name)`.
/// On failure the builder is returned unchanged (CPU fallback) with a
/// warning logged.
fn apply_execution_provider(
    builder: SessionBuilder,
    ep_name: &str,
) -> Result<(SessionBuilder, String), AiEngineError> {
    match ep_name {
        "cpu" => Ok((builder, "cpu".to_string())),

        #[cfg(feature = "cuda-ep")]
        "cuda" => match builder.with_execution_providers([ort::ep::CUDA::default().build()]) {
            Ok(b) => {
                info!("CUDA execution provider registered");
                Ok((b, "cuda".to_string()))
            }
            Err(e) => {
                tracing::warn!(error = %e, "CUDA EP registration failed, falling back to CPU");
                // `with_execution_providers` consumed the builder on error.
                // Rebuild a fresh one for CPU fallback.
                let fallback = rebuild_cpu_builder()?;
                Ok((fallback, "cpu".to_string()))
            }
        },
        #[cfg(not(feature = "cuda-ep"))]
        "cuda" => {
            tracing::warn!("CUDA EP requested but `cuda-ep` feature is not enabled, using CPU");
            Ok((builder, "cpu".to_string()))
        }

        #[cfg(feature = "tensorrt-ep")]
        "tensorrt" => {
            match builder.with_execution_providers([
                ort::ep::TensorRT::default().build(),
                ort::ep::CUDA::default().build(),
            ]) {
                Ok(b) => {
                    info!("TensorRT + CUDA execution providers registered");
                    Ok((b, "tensorrt".to_string()))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "TensorRT EP registration failed, falling back to CPU");
                    let fallback = rebuild_cpu_builder()?;
                    Ok((fallback, "cpu".to_string()))
                }
            }
        }
        #[cfg(not(feature = "tensorrt-ep"))]
        "tensorrt" => {
            tracing::warn!(
                "TensorRT EP requested but `tensorrt-ep` feature is not enabled, using CPU"
            );
            Ok((builder, "cpu".to_string()))
        }

        #[cfg(feature = "openvino-ep")]
        "openvino" => {
            match builder.with_execution_providers([ort::ep::OpenVINO::default().build()]) {
                Ok(b) => {
                    info!("OpenVINO execution provider registered");
                    Ok((b, "openvino".to_string()))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "OpenVINO EP registration failed, falling back to CPU");
                    let fallback = rebuild_cpu_builder()?;
                    Ok((fallback, "cpu".to_string()))
                }
            }
        }
        #[cfg(not(feature = "openvino-ep"))]
        "openvino" => {
            tracing::warn!(
                "OpenVINO EP requested but `openvino-ep` feature is not enabled, using CPU"
            );
            Ok((builder, "cpu".to_string()))
        }

        other => {
            tracing::warn!(
                provider = other,
                "unknown execution provider, falling back to CPU"
            );
            Ok((builder, "cpu".to_string()))
        }
    }
}

/// Rebuild a minimal session builder for CPU fallback after a GPU EP
/// registration failure consumed the original builder.
#[allow(dead_code)]
fn rebuild_cpu_builder() -> Result<SessionBuilder, AiEngineError> {
    OrtSession::builder()
        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .map_err(|e| AiEngineError::ModelLoadError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_group_with_depths(depths: &[usize]) -> SessionGroup {
        let workers = depths
            .iter()
            .map(|depth| {
                let (tx, _rx) = mpsc::channel(1);
                let queue_depth = Arc::new(AtomicUsize::new(*depth));
                SessionWorker { tx, queue_depth }
            })
            .collect();
        SessionGroup {
            workers,
            rr_cursor: AtomicUsize::new(0),
        }
    }

    #[test]
    fn select_worker_prefers_shorter_queue_when_gap_large() {
        let group = make_group_with_depths(&[8, 1, 2]);
        let idx = select_worker_index(&group);
        assert_eq!(idx, 1, "should choose worker with shortest queue");
    }

    #[test]
    fn select_worker_keeps_round_robin_when_depth_close() {
        let group = make_group_with_depths(&[2, 3, 2]);
        let idx = select_worker_index(&group);
        assert_eq!(idx, 0, "round-robin worker should be kept for close depths");
    }
}
