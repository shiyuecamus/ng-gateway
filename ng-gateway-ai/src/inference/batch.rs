//! Dynamic batching engine for multi-channel inference aggregation.
//!
//! The [`BatchRouter`] is the public entry point: it maintains one
//! collector loop (tokio task) per loaded model and transparently routes
//! `submit()` calls through the collector → backend pipeline.
//!
//! # Architecture
//!
//! ```text
//!   channel_1 ──┐
//!   channel_2 ──┤  submit(model_key, tensor)
//!   channel_N ──┘          │
//!                          ▼
//!              ┌─── BatchRouter ────────────────┐
//!              │  model_key → CollectorHandle    │
//!              │                                 │
//!              │  CollectorHandle (per model):    │
//!              │    mpsc::Sender<BatchItem>       │
//!              │    queue_depth: AtomicUsize      │
//!              │                                 │
//!              │  Collector Loop (tokio task):    │
//!              │    1. Accumulate up to B items   │
//!              │    2. Timeout T → partial flush  │
//!              │    3. Concat [B,C,H,W] tensor   │
//!              │    4. backend.infer()            │
//!              │    5. Scatter results via oneshot │
//!              │    6. Adaptive feedback loop     │
//!              └─────────────────────────────────┘
//! ```

use super::{backend::ModelBackend, RawInferenceOutput};
use crate::pipeline::preprocess::{CoordinateTransform, PreprocessOutput};
use dashmap::DashMap;
use ndarray::Array4;
use ng_gateway_error::ai::AiEngineError;
use ng_gateway_models::settings::BatchingConfig;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

// ── Types ─────────────────────────────────────────────────────────

/// A single inference request queued for batching.
struct BatchItem {
    /// Preprocessed tensor (batch dim = 1).
    tensor: Array4<f32>,
    /// Response channel — the caller awaits this.
    response_tx:
        oneshot::Sender<Result<(RawInferenceOutput, super::backend::InferTiming), AiEngineError>>,
    /// Submission timestamp (for queue-wait latency tracking).
    _submitted_at: Instant,
}

/// Per-model batch collector handle held by the router.
struct CollectorHandle {
    tx: mpsc::Sender<BatchItem>,
    queue_depth: Arc<AtomicUsize>,
    max_queue_depth: usize,
    /// Collector loop task handle (for graceful shutdown).
    _task: tokio::task::JoinHandle<()>,
}

/// Flush reason tag for metrics reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    /// Batch reached `max_batch_size`.
    Full,
    /// Collect timeout expired with a partial batch.
    Timeout,
    /// Only one item arrived (no batching benefit but we still flush).
    Single,
}

/// Snapshot of batch metrics for external reporting.
#[derive(Debug, Clone, Default)]
pub struct BatchMetricsSnapshot {
    /// Total batches flushed.
    pub total_flushes: u64,
    /// Flushes due to batch being full.
    pub flushes_full: u64,
    /// Flushes due to timeout.
    pub flushes_timeout: u64,
    /// Flushes with a single item (no batching).
    pub flushes_single: u64,
    /// Sum of all batch sizes (for average calculation).
    pub total_batch_items: u64,
}

/// Shared mutable metrics counters for one collector.
#[derive(Debug, Default)]
struct CollectorMetrics {
    total_flushes: AtomicUsize,
    flushes_full: AtomicUsize,
    flushes_timeout: AtomicUsize,
    flushes_single: AtomicUsize,
    total_batch_items: AtomicUsize,
}

impl CollectorMetrics {
    fn record_flush(&self, reason: FlushReason, batch_size: usize) {
        self.total_flushes.fetch_add(1, Ordering::Relaxed);
        self.total_batch_items
            .fetch_add(batch_size, Ordering::Relaxed);
        match reason {
            FlushReason::Full => self.flushes_full.fetch_add(1, Ordering::Relaxed),
            FlushReason::Timeout => self.flushes_timeout.fetch_add(1, Ordering::Relaxed),
            FlushReason::Single => self.flushes_single.fetch_add(1, Ordering::Relaxed),
        };
    }

    fn snapshot(&self) -> BatchMetricsSnapshot {
        BatchMetricsSnapshot {
            total_flushes: self.total_flushes.load(Ordering::Relaxed) as u64,
            flushes_full: self.flushes_full.load(Ordering::Relaxed) as u64,
            flushes_timeout: self.flushes_timeout.load(Ordering::Relaxed) as u64,
            flushes_single: self.flushes_single.load(Ordering::Relaxed) as u64,
            total_batch_items: self.total_batch_items.load(Ordering::Relaxed) as u64,
        }
    }
}

// ── Adaptive controller ───────────────────────────────────────────

/// Adaptive batch parameter controller.
///
/// Monitors queue load and inference latency to dynamically tune
/// `collect_timeout` and `max_batch_size`:
///
/// - Queue sustained >80% full → increase timeout (more aggressive batching)
/// - Queue sustained <20% full → decrease timeout (lower latency)
/// - P95 inference latency above threshold → decrease max_batch_size
struct AdaptiveController {
    enabled: bool,
    _base_timeout: Duration,
    current_timeout: Duration,
    min_timeout: Duration,
    max_timeout: Duration,
    base_batch_size: usize,
    current_batch_size: usize,
    min_batch_size: usize,
    /// Rolling window of recent inference latencies (ms).
    latency_window: Vec<f64>,
    latency_window_capacity: usize,
    /// P95 latency threshold (ms). When exceeded, batch size is reduced.
    p95_threshold_ms: f64,
}

impl AdaptiveController {
    fn new(config: &BatchingConfig) -> Self {
        let base_timeout = Duration::from_millis(config.collect_timeout_ms);
        Self {
            enabled: config.adaptive,
            _base_timeout: base_timeout,
            current_timeout: base_timeout,
            min_timeout: Duration::from_millis(1),
            max_timeout: base_timeout * 4,
            base_batch_size: config.max_batch_size,
            current_batch_size: config.max_batch_size,
            min_batch_size: 1,
            latency_window: Vec::with_capacity(128),
            latency_window_capacity: 128,
            p95_threshold_ms: 100.0,
        }
    }

    /// Feed queue load ratio [0.0, 1.0] to adapt timeout.
    fn on_queue_load(&mut self, load_ratio: f64) {
        if !self.enabled {
            return;
        }
        if load_ratio > 0.8 {
            self.current_timeout = (self.current_timeout.mul_f64(1.15)).min(self.max_timeout);
        } else if load_ratio < 0.2 {
            self.current_timeout = (self.current_timeout.mul_f64(0.85)).max(self.min_timeout);
        }
    }

    /// Feed observed inference latency (ms) to adapt batch size.
    fn on_inference_latency(&mut self, latency_ms: f64) {
        if !self.enabled {
            return;
        }
        if self.latency_window.len() >= self.latency_window_capacity {
            self.latency_window.remove(0);
        }
        self.latency_window.push(latency_ms);

        if self.latency_window.len() >= 16 {
            let p95 = self.compute_p95();
            if p95 > self.p95_threshold_ms && self.current_batch_size > self.min_batch_size {
                self.current_batch_size = (self.current_batch_size - 1).max(self.min_batch_size);
                debug!(
                    p95_ms = p95,
                    new_batch_size = self.current_batch_size,
                    "adaptive: reducing batch size due to high P95 latency"
                );
            } else if p95 < self.p95_threshold_ms * 0.5
                && self.current_batch_size < self.base_batch_size
            {
                self.current_batch_size = (self.current_batch_size + 1).min(self.base_batch_size);
            }
        }
    }

    fn compute_p95(&self) -> f64 {
        if self.latency_window.is_empty() {
            return 0.0;
        }
        let mut sorted = self.latency_window.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn effective_timeout(&self) -> Duration {
        self.current_timeout
    }

    fn effective_batch_size(&self) -> usize {
        self.current_batch_size
    }
}

// ── BatchRouter ───────────────────────────────────────────────────

/// Routes inference requests to per-model batch collectors.
///
/// Thread-safe: all internal state is protected by `DashMap` / atomics.
pub struct BatchRouter {
    /// Per-model-key collector handles.
    collectors: DashMap<String, CollectorHandle>,
    /// Per-model-key metrics.
    metrics: Arc<DashMap<String, Arc<CollectorMetrics>>>,
    /// Shared batching configuration.
    config: BatchingConfig,
    /// Guards concurrent collector spawns for the same model_key.
    spawn_guards: DashMap<String, Arc<Mutex<()>>>,
}

impl BatchRouter {
    /// Create a new batch router.
    pub fn new(config: BatchingConfig) -> Self {
        Self {
            collectors: DashMap::new(),
            metrics: Arc::new(DashMap::new()),
            config,
            spawn_guards: DashMap::new(),
        }
    }

    /// Submit a preprocessed tensor for batched inference.
    ///
    /// Lazily spawns a collector loop for the model if one does not exist.
    /// The caller awaits the batch result transparently.
    pub async fn submit(
        &self,
        model_key: &str,
        model_id: i32,
        input: PreprocessOutput,
        backend: Arc<dyn ModelBackend>,
        model_path: &std::path::Path,
    ) -> Result<(RawInferenceOutput, super::backend::InferTiming), AiEngineError> {
        let tensor = input.into_cpu_tensor().map_err(|_| {
            AiEngineError::InferenceError(
                "BatchRouter only supports CpuTensor input; DeviceMemory should bypass batching"
                    .into(),
            )
        })?;

        // Lazy collector spawn (double-check with guard).
        if !self.collectors.contains_key(model_key) {
            let guard = self
                .spawn_guards
                .entry(model_key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone();
            let _lock = guard.lock().await;

            if !self.collectors.contains_key(model_key) {
                self.spawn_collector(model_key, model_id, Arc::clone(&backend), model_path)
                    .await?;
            }
        }

        let handle = self.collectors.get(model_key).ok_or_else(|| {
            AiEngineError::InternalError(format!(
                "batch collector for model '{}' disappeared after spawn",
                model_key
            ))
        })?;

        // Backpressure: reject if queue is full.
        let depth = handle.queue_depth.load(Ordering::Relaxed);
        if depth >= handle.max_queue_depth {
            return Err(AiEngineError::Backpressure);
        }

        let (response_tx, response_rx) = oneshot::channel();
        let item = BatchItem {
            tensor,
            response_tx,
            _submitted_at: Instant::now(),
        };

        handle.queue_depth.fetch_add(1, Ordering::Relaxed);
        if handle.tx.send(item).await.is_err() {
            handle.queue_depth.fetch_sub(1, Ordering::Relaxed);
            return Err(AiEngineError::InferenceError(format!(
                "batch collector channel closed for model '{model_key}'"
            )));
        }

        response_rx.await.map_err(|_| {
            AiEngineError::InferenceError(format!(
                "batch response channel dropped for model '{model_key}'"
            ))
        })?
    }

    /// Spawn the collector loop for a model.
    async fn spawn_collector(
        &self,
        model_key: &str,
        model_id: i32,
        backend: Arc<dyn ModelBackend>,
        model_path: &std::path::Path,
    ) -> Result<(), AiEngineError> {
        if !backend.is_loaded(model_id) {
            backend.load(model_id, model_path).await?;
        }

        let (tx, rx) = mpsc::channel(self.config.max_queue_depth);
        let queue_depth = Arc::new(AtomicUsize::new(0));
        let metrics = Arc::new(CollectorMetrics::default());
        let model_key_owned = model_key.to_string();

        info!(
            model_key,
            max_batch_size = self.config.max_batch_size,
            timeout_ms = self.config.collect_timeout_ms,
            adaptive = self.config.adaptive,
            "spawning batch collector"
        );

        let task = spawn_collector_loop(
            model_key_owned.clone(),
            model_id,
            backend,
            rx,
            Arc::clone(&queue_depth),
            self.config.clone(),
            Arc::clone(&metrics),
        );

        self.metrics
            .insert(model_key_owned.clone(), Arc::clone(&metrics));
        self.collectors.insert(
            model_key_owned,
            CollectorHandle {
                tx,
                queue_depth,
                max_queue_depth: self.config.max_queue_depth,
                _task: task,
            },
        );

        Ok(())
    }

    /// Whether batching is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Number of active collector loops.
    pub fn collector_count(&self) -> usize {
        self.collectors.len()
    }

    /// Aggregate metrics snapshot across all models.
    pub fn metrics_snapshot(&self) -> BatchMetricsSnapshot {
        let mut agg = BatchMetricsSnapshot::default();
        for entry in self.metrics.iter() {
            let s = entry.value().snapshot();
            agg.total_flushes += s.total_flushes;
            agg.flushes_full += s.flushes_full;
            agg.flushes_timeout += s.flushes_timeout;
            agg.flushes_single += s.flushes_single;
            agg.total_batch_items += s.total_batch_items;
        }
        agg
    }
}

// ── Collector loop ────────────────────────────────────────────────

/// Spawn the batch collector loop as a dedicated tokio task.
fn spawn_collector_loop(
    model_key: String,
    model_id: i32,
    backend: Arc<dyn ModelBackend>,
    mut rx: mpsc::Receiver<BatchItem>,
    queue_depth: Arc<AtomicUsize>,
    config: BatchingConfig,
    metrics: Arc<CollectorMetrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut adaptive = AdaptiveController::new(&config);
        let mut pending: Vec<BatchItem> = Vec::with_capacity(config.max_batch_size);

        loop {
            let effective_timeout = adaptive.effective_timeout();
            let effective_batch_size = adaptive.effective_batch_size();

            // Phase 1: accumulate items.
            let deadline = tokio::time::Instant::now() + effective_timeout;

            while pending.len() < effective_batch_size {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, rx.recv()).await {
                    Ok(Some(item)) => {
                        queue_depth.fetch_sub(1, Ordering::Relaxed);
                        pending.push(item);
                    }
                    Ok(None) => {
                        debug!(model_key = %model_key, "batch collector channel closed");
                        return;
                    }
                    Err(_) => break,
                }
            }

            if pending.is_empty() {
                match rx.recv().await {
                    Some(item) => {
                        queue_depth.fetch_sub(1, Ordering::Relaxed);
                        pending.push(item);
                    }
                    None => {
                        debug!(model_key = %model_key, "batch collector channel closed");
                        return;
                    }
                }
                continue;
            }

            // Determine flush reason.
            let batch_size = pending.len();
            let flush_reason = if batch_size >= effective_batch_size {
                FlushReason::Full
            } else if batch_size == 1 {
                FlushReason::Single
            } else {
                FlushReason::Timeout
            };

            let batch = std::mem::replace(&mut pending, Vec::with_capacity(effective_batch_size));

            // Adaptive: feed queue load.
            let max_depth = config.max_queue_depth.max(1);
            let load = queue_depth.load(Ordering::Relaxed) as f64 / max_depth as f64;
            adaptive.on_queue_load(load);

            // Phase 2: concat and infer.
            let views: Vec<_> = batch.iter().map(|b| b.tensor.view()).collect();
            let batched_tensor = match ndarray::concatenate(ndarray::Axis(0), &views) {
                Ok(t) => t,
                Err(e) => {
                    let err_msg = format!("batch tensor concat failed: {e}");
                    warn!(model_key = %model_key, batch_size, "{}", err_msg);
                    for item in batch {
                        let _ = item
                            .response_tx
                            .send(Err(AiEngineError::InferenceError(err_msg.clone())));
                    }
                    continue;
                }
            };

            let preprocess_output = PreprocessOutput::CpuTensor {
                tensor: batched_tensor,
                coord_transform: CoordinateTransform {
                    scale_x: 1.0,
                    scale_y: 1.0,
                    pad_x: 0.0,
                    pad_y: 0.0,
                    orig_width: 0,
                    orig_height: 0,
                    input_width: 0,
                    input_height: 0,
                },
            };

            let infer_start = Instant::now();
            let result = backend.infer(model_id, preprocess_output).await;
            let infer_elapsed_ms = infer_start.elapsed().as_secs_f64() * 1000.0;

            // Adaptive: feed inference latency.
            adaptive.on_inference_latency(infer_elapsed_ms);

            // Record metrics.
            metrics.record_flush(flush_reason, batch_size);

            // Phase 3: scatter results.
            match result {
                Ok((batched_output, timing)) => {
                    if batch_size == 1 {
                        if let Some(item) = batch.into_iter().next() {
                            let _ = item.response_tx.send(Ok((batched_output, timing)));
                        } else {
                            warn!(
                                model_key = %model_key,
                                batch_size,
                                "batch unexpectedly empty during single-item scatter"
                            );
                        }
                    } else {
                        for (i, item) in batch.into_iter().enumerate() {
                            let single_output = slice_batch_output(&batched_output, i);
                            let _ = item.response_tx.send(Ok((single_output, timing)));
                        }
                    }
                }
                Err(e) => {
                    let err_msg = format!("batched inference failed: {e}");
                    for item in batch {
                        let _ = item
                            .response_tx
                            .send(Err(AiEngineError::InferenceError(err_msg.clone())));
                    }
                }
            }
        }
    })
}

// ── Helpers ───────────────────────────────────────────────────────

/// Slice a batched inference output to extract one item's results.
fn slice_batch_output(batched: &RawInferenceOutput, index: usize) -> RawInferenceOutput {
    use ndarray::Axis;
    let tensors = batched
        .tensors
        .iter()
        .map(|(name, arr)| {
            let sliced = arr
                .index_axis(Axis(0), index)
                .insert_axis(Axis(0))
                .to_owned();
            (name.clone(), sliced)
        })
        .collect();
    RawInferenceOutput { tensors }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::ArrayD;

    #[test]
    fn slice_batch_output_single() {
        let tensor = ArrayD::from_shape_vec(vec![1, 3, 2, 2], (0..12).map(|x| x as f32).collect())
            .expect("valid shape");
        let output = RawInferenceOutput {
            tensors: vec![("out".to_string(), tensor)],
        };
        let sliced = slice_batch_output(&output, 0);
        assert_eq!(sliced.tensors[0].1.shape(), &[1, 3, 2, 2]);
    }

    #[test]
    fn slice_batch_output_multi() {
        let data: Vec<f32> = (0..24).map(|x| x as f32).collect();
        let tensor = ArrayD::from_shape_vec(vec![2, 3, 2, 2], data).expect("valid shape");
        let output = RawInferenceOutput {
            tensors: vec![("out".to_string(), tensor)],
        };
        let s0 = slice_batch_output(&output, 0);
        let s1 = slice_batch_output(&output, 1);
        assert_eq!(s0.tensors[0].1[[0, 0, 0, 0]], 0.0);
        assert_eq!(s1.tensors[0].1[[0, 0, 0, 0]], 12.0);
    }

    #[test]
    fn adaptive_controller_adjusts_timeout() {
        let config = BatchingConfig {
            enabled: false,
            max_batch_size: 4,
            collect_timeout_ms: 10,
            max_queue_depth: 32,
            adaptive: true,
        };
        let mut ctrl = AdaptiveController::new(&config);
        let base = ctrl.effective_timeout();

        // High load should increase timeout.
        for _ in 0..10 {
            ctrl.on_queue_load(0.9);
        }
        assert!(
            ctrl.effective_timeout() > base,
            "timeout should increase under high load"
        );

        // Low load should decrease timeout.
        for _ in 0..30 {
            ctrl.on_queue_load(0.1);
        }
        assert!(
            ctrl.effective_timeout() < ctrl.max_timeout,
            "timeout should decrease under low load"
        );
    }

    #[test]
    fn adaptive_controller_reduces_batch_size_on_high_latency() {
        let config = BatchingConfig {
            enabled: false,
            max_batch_size: 8,
            collect_timeout_ms: 10,
            max_queue_depth: 32,
            adaptive: true,
        };
        let mut ctrl = AdaptiveController::new(&config);
        assert_eq!(ctrl.effective_batch_size(), 8);

        // Feed high latencies.
        for _ in 0..20 {
            ctrl.on_inference_latency(200.0);
        }
        assert!(
            ctrl.effective_batch_size() < 8,
            "batch size should decrease with high P95"
        );
    }

    #[test]
    fn metrics_snapshot_aggregates() {
        let m = CollectorMetrics::default();
        m.record_flush(FlushReason::Full, 4);
        m.record_flush(FlushReason::Timeout, 2);
        m.record_flush(FlushReason::Single, 1);
        let snap = m.snapshot();
        assert_eq!(snap.total_flushes, 3);
        assert_eq!(snap.flushes_full, 1);
        assert_eq!(snap.flushes_timeout, 1);
        assert_eq!(snap.flushes_single, 1);
        assert_eq!(snap.total_batch_items, 7);
    }
}
