//! Queue/backpressure metrics registry.
//!
//! This module is the backend for `instrumented_mpsc` and any other bounded-queue
//! instrumentation in the gateway.

use dashmap::DashMap;
use ng_gateway_error::{NGError, NGResult};
use once_cell::sync::{Lazy, OnceCell};
use prometheus::{
    opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tracing::warn;

use super::REGISTRY;

/// Drop reason for queue instrumentation.
///
/// # Prometheus mapping
/// Each variant maps to a **bounded** `reason` label value for:
/// `ng_gateway_queue_dropped_total{queue="...",reason="..."}`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// Queue is full (`try_send` fails with Full).
    Full,
    /// Producer waited but timed out (`send_timeout`).
    Timeout,
    /// Channel/queue was closed.
    Closed,
    /// Buffer is full and an item was evicted/dropped.
    BufferFull,
    /// Buffered item expired before it could be flushed.
    Expired,
}

impl DropReason {
    /// Convert to the Prometheus `reason` label value.
    #[inline]
    pub const fn as_label(self) -> &'static str {
        match self {
            DropReason::Full => "full",
            DropReason::Timeout => "timeout",
            DropReason::Closed => "closed",
            DropReason::BufferFull => "buffer_full",
            DropReason::Expired => "expired",
        }
    }
}

#[inline]
fn register_collector(collector: Box<dyn prometheus::core::Collector>, name: &'static str) {
    if let Err(e) = REGISTRY.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

/// A set of pre-resolved metric handles for a single queue.
///
/// This avoids repeated label lookups on the hot path.
#[derive(Debug)]
pub struct QueueMetricHandles {
    pub depth: IntGauge,
    pub capacity: IntGauge,
    pub dropped_full_total: IntCounter,
    pub dropped_timeout_total: IntCounter,
    pub dropped_closed_total: IntCounter,
    pub dropped_buffer_full_total: IntCounter,
    pub dropped_expired_total: IntCounter,
    pub blocked_seconds: Histogram,
}

impl QueueMetricHandles {
    /// Set the scrape-time depth gauge.
    #[inline]
    pub fn set_depth(&self, depth: i64) {
        self.depth.set(depth);
    }

    /// Set the fixed capacity gauge.
    #[inline]
    pub fn set_capacity(&self, capacity: i64) {
        self.capacity.set(capacity);
    }

    /// Observe blocked seconds histogram.
    #[inline]
    pub fn observe_blocked_seconds(&self, seconds: f64) {
        self.blocked_seconds.observe(seconds);
    }

    /// Increment a dropped counter by reason.
    #[inline]
    pub fn inc_dropped(&self, reason: DropReason) {
        match reason {
            DropReason::Full => {
                self.dropped_full_total.inc();
            }
            DropReason::Timeout => {
                self.dropped_timeout_total.inc();
            }
            DropReason::Closed => {
                self.dropped_closed_total.inc();
            }
            DropReason::BufferFull => {
                self.dropped_buffer_full_total.inc();
            }
            DropReason::Expired => {
                self.dropped_expired_total.inc();
            }
        }
    }
}

/// Internal state for a registered queue.
#[derive(Debug)]
pub struct QueueObserverInner {
    pub queue_name: String,
    pub capacity: u64,
    pub depth: Arc<AtomicU64>,
    pub metrics: QueueMetricHandles,
}

static QUEUE_DEPTH: OnceCell<IntGaugeVec> = OnceCell::new();
static QUEUE_CAPACITY: OnceCell<IntGaugeVec> = OnceCell::new();
static QUEUE_DROPPED_TOTAL: OnceCell<IntCounterVec> = OnceCell::new();
static QUEUE_BLOCKED_SECONDS: OnceCell<HistogramVec> = OnceCell::new();

static QUEUE_REGISTRY: Lazy<DashMap<String, Arc<QueueObserverInner>>> = Lazy::new(DashMap::new);

static QUEUE_METRICS_REGISTERED: Lazy<()> = Lazy::new(|| {
    // Metrics are registered via `init_queue_metrics()` only.
});

/// Initialize and register all queue/backpressure metric vectors.
///
/// # Notes
/// - This function is idempotent.
/// - It returns a `NGResult<()>` instead of panicking.
pub fn init_queue_metrics() -> NGResult<()> {
    *QUEUE_METRICS_REGISTERED;

    let _depth = QUEUE_DEPTH.get_or_try_init(|| -> NGResult<IntGaugeVec> {
        let v = IntGaugeVec::new(
            opts!(
                "ng_gateway_queue_depth",
                "Current queue depth (best-effort, monotonic under backpressure)."
            ),
            &["queue"],
        )
        .map_err(|e| NGError::from(format!("Failed to create ng_gateway_queue_depth: {e}")))?;
        register_collector(Box::new(v.clone()), "ng_gateway_queue_depth");
        Ok(v)
    })?;

    let _cap = QUEUE_CAPACITY.get_or_try_init(|| -> NGResult<IntGaugeVec> {
        let v = IntGaugeVec::new(
            opts!("ng_gateway_queue_capacity", "Configured queue capacity."),
            &["queue"],
        )
        .map_err(|e| NGError::from(format!("Failed to create ng_gateway_queue_capacity: {e}")))?;
        register_collector(Box::new(v.clone()), "ng_gateway_queue_capacity");
        Ok(v)
    })?;

    let _drops = QUEUE_DROPPED_TOTAL.get_or_try_init(|| -> NGResult<IntCounterVec> {
        let v = IntCounterVec::new(
            opts!(
                "ng_gateway_queue_dropped_total",
                "Total dropped items due to backpressure or policy."
            ),
            &["queue", "reason"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create ng_gateway_queue_dropped_total: {e}"
            ))
        })?;
        register_collector(Box::new(v.clone()), "ng_gateway_queue_dropped_total");
        Ok(v)
    })?;

    let _blocked = QUEUE_BLOCKED_SECONDS.get_or_try_init(|| -> NGResult<HistogramVec> {
        let opts = HistogramOpts::new(
            "ng_gateway_queue_blocked_seconds",
            "Time spent blocked waiting for queue capacity (send-side).",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let v = HistogramVec::new(opts, &["queue"]).map_err(|e| {
            NGError::from(format!(
                "Failed to create ng_gateway_queue_blocked_seconds: {e}"
            ))
        })?;
        register_collector(Box::new(v.clone()), "ng_gateway_queue_blocked_seconds");
        Ok(v)
    })?;

    Ok(())
}

/// Register a queue into the global queue registry and return its observer.
///
/// # Notes
/// - Safe to call multiple times for the same `queue_name` and will return the existing entry.
/// - `capacity` is not changed after registration.
pub fn register_queue(queue_name: String, capacity: u64) -> NGResult<Arc<QueueObserverInner>> {
    init_queue_metrics()?;

    if let Some(existing) = QUEUE_REGISTRY.get(&queue_name) {
        return Ok(Arc::clone(existing.value()));
    }

    // Resolve child metrics once to avoid label lookups on hot paths.
    let depth_vec = QUEUE_DEPTH
        .get()
        .ok_or_else(|| NGError::from("Queue metrics not initialized: QUEUE_DEPTH"))?;
    let cap_vec = QUEUE_CAPACITY
        .get()
        .ok_or_else(|| NGError::from("Queue metrics not initialized: QUEUE_CAPACITY"))?;
    let drops_vec = QUEUE_DROPPED_TOTAL
        .get()
        .ok_or_else(|| NGError::from("Queue metrics not initialized: QUEUE_DROPPED_TOTAL"))?;
    let blocked_vec = QUEUE_BLOCKED_SECONDS
        .get()
        .ok_or_else(|| NGError::from("Queue metrics not initialized: QUEUE_BLOCKED_SECONDS"))?;

    let depth = depth_vec
        .get_metric_with_label_values(&[queue_name.as_str()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get queue depth gauge for {queue_name}: {e}"
            ))
        })?;
    let cap_gauge = cap_vec
        .get_metric_with_label_values(&[queue_name.as_str()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get queue capacity gauge for {queue_name}: {e}"
            ))
        })?;
    cap_gauge.set(capacity as i64);

    let dropped_full_total = drops_vec
        .get_metric_with_label_values(&[queue_name.as_str(), DropReason::Full.as_label()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get dropped_full_total for {queue_name}: {e}"
            ))
        })?;
    let dropped_timeout_total = drops_vec
        .get_metric_with_label_values(&[queue_name.as_str(), DropReason::Timeout.as_label()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get dropped_timeout_total for {queue_name}: {e}"
            ))
        })?;
    let dropped_closed_total = drops_vec
        .get_metric_with_label_values(&[queue_name.as_str(), DropReason::Closed.as_label()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get dropped_closed_total for {queue_name}: {e}"
            ))
        })?;
    let dropped_buffer_full_total = drops_vec
        .get_metric_with_label_values(&[queue_name.as_str(), DropReason::BufferFull.as_label()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get dropped_buffer_full_total for {queue_name}: {e}"
            ))
        })?;
    let dropped_expired_total = drops_vec
        .get_metric_with_label_values(&[queue_name.as_str(), DropReason::Expired.as_label()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get dropped_expired_total for {queue_name}: {e}"
            ))
        })?;

    let blocked_seconds = blocked_vec
        .get_metric_with_label_values(&[queue_name.as_str()])
        .map_err(|e| {
            NGError::from(format!(
                "Failed to get blocked_seconds histogram for {queue_name}: {e}"
            ))
        })?;

    let inner = Arc::new(QueueObserverInner {
        queue_name: queue_name.clone(),
        capacity,
        depth: Arc::new(AtomicU64::new(0)),
        metrics: QueueMetricHandles {
            depth,
            capacity: cap_gauge,
            dropped_full_total,
            dropped_timeout_total,
            dropped_closed_total,
            dropped_buffer_full_total,
            dropped_expired_total,
            blocked_seconds,
        },
    });

    // Set capacity gauge once (if enabled).
    inner.metrics.set_capacity(capacity as i64);

    QUEUE_REGISTRY.insert(queue_name, Arc::clone(&inner));
    Ok(inner)
}

/// Refresh all queue depth gauges from their atomic counters.
///
/// This should be called right before Prometheus encoding to keep scrape-time
/// accurate without adding extra cost to hot paths.
pub fn refresh_all_queue_depths() {
    for entry in QUEUE_REGISTRY.iter() {
        let depth = entry.value().depth.load(Ordering::Relaxed) as i64;
        entry.value().metrics.set_depth(depth);
    }
}
