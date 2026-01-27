//! Queue/backpressure metrics registry.
//!
//! This module is the backend for `channel` and any other bounded-queue
//! instrumentation in the gateway.

use dashmap::{mapref::entry::Entry, DashMap};
use ng_gateway_error::{NGError, NGResult};
use prometheus::{
    opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec,
    Registry,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tracing::warn;

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
fn register_collector_into(
    registry: &Registry,
    collector: Box<dyn prometheus::core::Collector>,
    name: &'static str,
) {
    if let Err(e) = registry.register(collector) {
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
    pub depth: Arc<AtomicU64>,
    pub metrics: QueueMetricHandles,
}

/// Queue/backpressure metrics owned by `NGMetricsHub`.
///
/// # Notes
/// - All metric vectors are created and registered during `new()`.
/// - Per-queue child handles are resolved once and cached in `QueueObserverInner`.
#[derive(Debug)]
pub(crate) struct QueueMetricsHub {
    depth_vec: IntGaugeVec,
    cap_vec: IntGaugeVec,
    drops_vec: IntCounterVec,
    blocked_vec: HistogramVec,
    queues: DashMap<String, Arc<QueueObserverInner>>,
}

impl QueueMetricsHub {
    /// Create and register queue/backpressure metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let depth_vec = IntGaugeVec::new(
            opts!(
                "queue_depth",
                "Current queue depth (best-effort, monotonic under backpressure)."
            ),
            &["queue"],
        )
        .map_err(|e| NGError::from(format!("Failed to create queue_depth: {e}")))?;
        register_collector_into(registry, Box::new(depth_vec.clone()), "queue_depth");

        let cap_vec = IntGaugeVec::new(
            opts!("queue_capacity", "Configured queue capacity."),
            &["queue"],
        )
        .map_err(|e| NGError::from(format!("Failed to create queue_capacity: {e}")))?;
        register_collector_into(registry, Box::new(cap_vec.clone()), "queue_capacity");

        let drops_vec = IntCounterVec::new(
            opts!(
                "queue_dropped_total",
                "Total dropped items due to backpressure or policy."
            ),
            &["queue", "reason"],
        )
        .map_err(|e| NGError::from(format!("Failed to create queue_dropped_total: {e}")))?;
        register_collector_into(registry, Box::new(drops_vec.clone()), "queue_dropped_total");

        let blocked_opts = HistogramOpts::new(
            "queue_blocked_seconds",
            "Time spent blocked waiting for queue capacity (send-side).",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let blocked_vec = HistogramVec::new(blocked_opts, &["queue"])
            .map_err(|e| NGError::from(format!("Failed to create queue_blocked_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(blocked_vec.clone()),
            "queue_blocked_seconds",
        );

        Ok(Self {
            depth_vec,
            cap_vec,
            drops_vec,
            blocked_vec,
            queues: DashMap::new(),
        })
    }

    /// Register a queue and return its observer.
    ///
    /// # Notes
    /// - Safe to call multiple times for the same `queue_name` and will return the existing entry.
    /// - `capacity` is not changed after registration.
    pub(crate) fn register_queue(
        &self,
        queue_name: String,
        capacity: u64,
    ) -> NGResult<Arc<QueueObserverInner>> {
        // Use DashMap entry API to ensure only one observer is created per queue name,
        // even under concurrent registration.
        match self.queues.entry(queue_name.clone()) {
            Entry::Occupied(existing) => {
                // Allow capacity gauge to reflect runtime tuning (queue rebuild).
                if let Ok(cap_gauge) = self
                    .cap_vec
                    .get_metric_with_label_values(&[queue_name.as_str()])
                {
                    cap_gauge.set(capacity as i64);
                }
                Ok(Arc::clone(existing.get()))
            }
            Entry::Vacant(vacant) => {
                // Resolve child metrics once to avoid label lookups on hot paths.
                let depth = self
                    .depth_vec
                    .get_metric_with_label_values(&[queue_name.as_str()])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get queue depth gauge for {queue_name}: {e}"
                        ))
                    })?;

                let cap_gauge = self
                    .cap_vec
                    .get_metric_with_label_values(&[queue_name.as_str()])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get queue capacity gauge for {queue_name}: {e}"
                        ))
                    })?;
                cap_gauge.set(capacity as i64);

                let dropped_full_total = self
                    .drops_vec
                    .get_metric_with_label_values(&[
                        queue_name.as_str(),
                        DropReason::Full.as_label(),
                    ])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get dropped_full_total for {queue_name}: {e}"
                        ))
                    })?;
                let dropped_timeout_total = self
                    .drops_vec
                    .get_metric_with_label_values(&[
                        queue_name.as_str(),
                        DropReason::Timeout.as_label(),
                    ])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get dropped_timeout_total for {queue_name}: {e}"
                        ))
                    })?;
                let dropped_closed_total = self
                    .drops_vec
                    .get_metric_with_label_values(&[
                        queue_name.as_str(),
                        DropReason::Closed.as_label(),
                    ])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get dropped_closed_total for {queue_name}: {e}"
                        ))
                    })?;
                let dropped_buffer_full_total = self
                    .drops_vec
                    .get_metric_with_label_values(&[
                        queue_name.as_str(),
                        DropReason::BufferFull.as_label(),
                    ])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get dropped_buffer_full_total for {queue_name}: {e}"
                        ))
                    })?;
                let dropped_expired_total = self
                    .drops_vec
                    .get_metric_with_label_values(&[
                        queue_name.as_str(),
                        DropReason::Expired.as_label(),
                    ])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get dropped_expired_total for {queue_name}: {e}"
                        ))
                    })?;

                let blocked_seconds = self
                    .blocked_vec
                    .get_metric_with_label_values(&[queue_name.as_str()])
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get blocked_seconds histogram for {queue_name}: {e}"
                        ))
                    })?;

                let inner = Arc::new(QueueObserverInner {
                    queue_name: queue_name.clone(),
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

                // Set capacity gauge once.
                inner.metrics.set_capacity(capacity as i64);

                vacant.insert(Arc::clone(&inner));
                Ok(inner)
            }
        }
    }

    /// Refresh all queue depth gauges from their atomic counters (scrape-time).
    pub(crate) fn refresh_all_queue_depths(&self) {
        for entry in self.queues.iter() {
            let depth = entry.value().depth.load(Ordering::Relaxed) as i64;
            entry.value().metrics.set_depth(depth);
        }
    }
}
