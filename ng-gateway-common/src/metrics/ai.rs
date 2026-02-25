//! AI Processing Engine metrics.
//!
//! Provides low-cardinality Prometheus metrics for the AI vision subsystem.
//! All metrics are registered into the shared `NGMetricsHub` Prometheus registry,
//! ensuring they appear on the `GET /metrics` endpoint alongside other subsystems.
//!
//! # Cardinality rules
//! - `channel_id` labels are bounded by the number of configured channels.
//! - `model_id` / `module_id` labels are bounded by registry entries (typically < 10).
//! - `class` labels for detections are bounded by model label sets.
//! - Do NOT include frame_seq, detection_id, or other high-cardinality values.
//!
//! # Public API
//! External crates (e.g. `ng-gateway-ai`) interact exclusively through the
//! method API — no prometheus types leak across crate boundaries.

use ng_gateway_error::{NGError, NGResult};
use prometheus::{
    core::Collector, opts, Histogram, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts,
    Registry,
};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::warn;

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        warn!(metric_name = name, error = %e, "Failed to register AI Prometheus metric");
    }
}

/// AI engine metrics owned by `NGMetricsHub`.
///
/// All metric vectors are registered once at initialization. Individual metric
/// handles are resolved lazily via `with_label_values()` at the call site —
/// acceptable here because AI metrics are not on a per-packet hot path
/// (inference runs at 1–30 FPS, not millions of packets/sec).
///
/// External consumers should use the public method API rather than accessing
/// prometheus primitives directly — this keeps `prometheus` as an internal
/// implementation detail of `ng-gateway-common`.
#[derive(Debug)]
pub struct AiMetricsHub {
    frames_submitted: IntCounterVec,
    frames_dropped: IntCounterVec,
    inference_latency: HistogramVec,
    models_loaded: IntGauge,
    active_inferences: IntGauge,
    detections_total: IntCounterVec,
    alarms_triggered: IntCounterVec,
    model_load_latency: Histogram,
    frame_decode_latency: Histogram,
    wasm_execution_latency: HistogramVec,

    /// Cumulative inference count (lock-free, for status API).
    total_inference_count: AtomicU64,
    /// Cumulative latency sum in microseconds (lock-free, for status API).
    total_latency_us: AtomicU64,
}

impl AiMetricsHub {
    /// Create and register AI metrics into the given Prometheus registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let frames_submitted = IntCounterVec::new(
            Opts::new(
                "ai_frames_submitted_total",
                "Total frames submitted for AI analysis",
            ),
            &["channel_id"],
        )
        .map_err(|e| NGError::from(format!("ai_frames_submitted_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(frames_submitted.clone()),
            "ai_frames_submitted_total",
        );

        let frames_dropped = IntCounterVec::new(
            Opts::new(
                "ai_frames_dropped_total",
                "Total frames dropped due to backpressure",
            ),
            &["channel_id"],
        )
        .map_err(|e| NGError::from(format!("ai_frames_dropped_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(frames_dropped.clone()),
            "ai_frames_dropped_total",
        );

        let inference_latency = HistogramVec::new(
            HistogramOpts::new(
                "ai_inference_latency_seconds",
                "End-to-end inference latency",
            )
            .buckets(vec![0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5]),
            &["model_id"],
        )
        .map_err(|e| NGError::from(format!("ai_inference_latency_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(inference_latency.clone()),
            "ai_inference_latency_seconds",
        );

        let models_loaded =
            IntGauge::new("ai_models_loaded", "Number of currently loaded AI models")
                .map_err(|e| NGError::from(format!("ai_models_loaded: {e}")))?;
        register_collector_into(
            registry,
            Box::new(models_loaded.clone()),
            "ai_models_loaded",
        );

        let active_inferences = IntGauge::new(
            "ai_active_inferences",
            "Number of currently running inferences",
        )
        .map_err(|e| NGError::from(format!("ai_active_inferences: {e}")))?;
        register_collector_into(
            registry,
            Box::new(active_inferences.clone()),
            "ai_active_inferences",
        );

        let detections_total = IntCounterVec::new(
            opts!("ai_detections_total", "Total detections produced"),
            &["class"],
        )
        .map_err(|e| NGError::from(format!("ai_detections_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(detections_total.clone()),
            "ai_detections_total",
        );

        let alarms_triggered = IntCounterVec::new(
            opts!("ai_alarms_triggered_total", "Total AI alarms triggered"),
            &["alarm_type", "severity"],
        )
        .map_err(|e| NGError::from(format!("ai_alarms_triggered_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(alarms_triggered.clone()),
            "ai_alarms_triggered_total",
        );

        let model_load_latency = Histogram::with_opts(
            HistogramOpts::new("ai_model_load_latency_seconds", "Model loading latency")
                .buckets(vec![0.1, 0.5, 1.0, 5.0, 10.0, 30.0]),
        )
        .map_err(|e| NGError::from(format!("ai_model_load_latency_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(model_load_latency.clone()),
            "ai_model_load_latency_seconds",
        );

        let frame_decode_latency = Histogram::with_opts(
            HistogramOpts::new("ai_frame_decode_latency_seconds", "Frame decode latency")
                .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05]),
        )
        .map_err(|e| NGError::from(format!("ai_frame_decode_latency_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(frame_decode_latency.clone()),
            "ai_frame_decode_latency_seconds",
        );

        let wasm_execution_latency = HistogramVec::new(
            HistogramOpts::new(
                "ai_wasm_execution_latency_seconds",
                "WASM algorithm execution latency",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5]),
            &["module_id"],
        )
        .map_err(|e| NGError::from(format!("ai_wasm_execution_latency_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(wasm_execution_latency.clone()),
            "ai_wasm_execution_latency_seconds",
        );

        Ok(Self {
            frames_submitted,
            frames_dropped,
            inference_latency,
            models_loaded,
            active_inferences,
            detections_total,
            alarms_triggered,
            model_load_latency,
            frame_decode_latency,
            wasm_execution_latency,
            total_inference_count: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
        })
    }

    // ── Frame counters ────────────────────────────────────────────────

    /// Increment frames submitted counter for the given channel.
    #[inline]
    pub fn inc_frames_submitted(&self, channel_id: &str) {
        self.frames_submitted.with_label_values(&[channel_id]).inc();
    }

    /// Increment frames dropped counter for the given channel.
    #[inline]
    pub fn inc_frames_dropped(&self, channel_id: &str) {
        self.frames_dropped.with_label_values(&[channel_id]).inc();
    }

    // ── Inference lifecycle ───────────────────────────────────────────

    /// Increment the active inference gauge (call when inference starts).
    #[inline]
    pub fn inc_active_inferences(&self) {
        self.active_inferences.inc();
    }

    /// Decrement the active inference gauge (call when inference completes).
    #[inline]
    pub fn dec_active_inferences(&self) {
        self.active_inferences.dec();
    }

    /// Current number of in-flight inferences.
    #[inline]
    pub fn active_inference_count(&self) -> i64 {
        self.active_inferences.get()
    }

    /// Record a completed inference with its latency (histogram + atomic accumulators).
    pub fn record_inference(&self, latency: std::time::Duration, model_id: &str) {
        self.inference_latency
            .with_label_values(&[model_id])
            .observe(latency.as_secs_f64());
        self.total_inference_count.fetch_add(1, Ordering::Relaxed);
        self.total_latency_us
            .fetch_add(latency.as_micros() as u64, Ordering::Relaxed);
    }

    /// Total number of inferences completed since engine start.
    #[inline]
    pub fn total_inferences(&self) -> u64 {
        self.total_inference_count.load(Ordering::Relaxed)
    }

    /// Average inference latency in milliseconds (0.0 if no inferences yet).
    pub fn avg_latency_ms(&self) -> f64 {
        let count = self.total_inference_count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        let total_us = self.total_latency_us.load(Ordering::Relaxed);
        (total_us as f64 / count as f64) / 1000.0
    }

    // ── Detection & alarm counters ────────────────────────────────────

    /// Increment detection counter for the given class label.
    #[inline]
    pub fn inc_detections(&self, class: &str) {
        self.detections_total.with_label_values(&[class]).inc();
    }

    /// Increment alarm triggered counter.
    #[inline]
    pub fn inc_alarms_triggered(&self, alarm_type: &str, severity: &str) {
        self.alarms_triggered
            .with_label_values(&[alarm_type, severity])
            .inc();
    }

    // ── Model lifecycle ───────────────────────────────────────────────

    /// Set the loaded models gauge.
    #[inline]
    pub fn set_models_loaded(&self, count: i64) {
        self.models_loaded.set(count);
    }

    /// Observe model loading latency.
    #[inline]
    pub fn observe_model_load_latency(&self, seconds: f64) {
        self.model_load_latency.observe(seconds);
    }

    // ── Frame decode ──────────────────────────────────────────────────

    /// Observe frame decode latency.
    #[inline]
    pub fn observe_frame_decode_latency(&self, seconds: f64) {
        self.frame_decode_latency.observe(seconds);
    }

    // ── WASM algorithm execution ──────────────────────────────────────

    /// Observe WASM algorithm execution latency for the given module.
    #[inline]
    pub fn observe_wasm_execution_latency(&self, module_id: &str, seconds: f64) {
        self.wasm_execution_latency
            .with_label_values(&[module_id])
            .observe(seconds);
    }
}
