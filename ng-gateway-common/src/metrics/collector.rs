//! Collector (data collection engine) metrics.
//!
//! This module provides low-cardinality Prometheus metrics and snapshot DTO generation
//! for the collection engine.
//!
//! # Cardinality rules
//! - Labels must remain bounded: `result` only.
//! - Do NOT include channel_id/device_id/point_id/error_message in labels.

use chrono::{DateTime, Utc};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::CollectorMetricsSnapshot;
use prometheus::{
    core::Collector, opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Registry,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    RwLock,
};
use tracing::warn;

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CollectorResult {
    Success,
    Fail,
    Timeout,
}

/// Collector metrics owned by `NGMetricsHub`.
#[derive(Debug)]
pub(crate) struct CollectorMetricsHub {
    active_tasks: IntGauge,
    permits_current: IntGauge,
    permits_available: IntGauge,

    // Pre-resolved counters/histograms to avoid label lookups on hot paths.
    cycles_success_total: IntCounter,
    cycles_fail_total: IntCounter,
    cycles_timeout_total: IntCounter,
    cycle_success_seconds: Histogram,
    cycle_fail_seconds: Histogram,
    cycle_timeout_seconds: Histogram,

    retries_timeout_total: IntCounter,
    retries_error_total: IntCounter,

    // snapshot-only state (single source of truth for REST/WS)
    avg_cycle_ns: AtomicU64,
    last_update: RwLock<Option<DateTime<Utc>>>,
}

impl CollectorMetricsHub {
    /// Create and register collector metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let cycles_total = IntCounterVec::new(
            opts!(
                "collector_cycles_total",
                "Total collection cycles (per group call) labeled by result."
            ),
            &["result"],
        )
        .map_err(|e| NGError::from(format!("Failed to create collector_cycles_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(cycles_total.clone()),
            "collector_cycles_total",
        );

        let cycle_opts = HistogramOpts::new(
            "collector_cycle_seconds",
            "Duration of a collection cycle (per group call) in seconds.",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let cycle_seconds = HistogramVec::new(cycle_opts, &["result"])
            .map_err(|e| NGError::from(format!("Failed to create collector_cycle_seconds: {e}")))?;
        register_collector_into(
            registry,
            Box::new(cycle_seconds.clone()),
            "collector_cycle_seconds",
        );

        let active_tasks = IntGauge::new(
            "collector_active_tasks",
            "Number of active channel collection tasks.",
        )
        .map_err(|e| NGError::from(format!("Failed to create collector_active_tasks: {e}")))?;
        register_collector_into(
            registry,
            Box::new(active_tasks.clone()),
            "collector_active_tasks",
        );

        let concurrency_permits = IntGaugeVec::new(
            opts!(
                "collector_concurrency_permits",
                "Collector concurrency permits (state=current|available)."
            ),
            &["state"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create collector_concurrency_permits: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(concurrency_permits.clone()),
            "collector_concurrency_permits",
        );

        let cycles_success_total = cycles_total
            .get_metric_with_label_values(&["success"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycles(success): {e}")))?;
        let cycles_fail_total = cycles_total
            .get_metric_with_label_values(&["fail"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycles(fail): {e}")))?;
        let cycles_timeout_total = cycles_total
            .get_metric_with_label_values(&["timeout"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycles(timeout): {e}")))?;

        let cycle_success_seconds = cycle_seconds
            .get_metric_with_label_values(&["success"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycle(success): {e}")))?;
        let cycle_fail_seconds = cycle_seconds
            .get_metric_with_label_values(&["fail"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycle(fail): {e}")))?;
        let cycle_timeout_seconds = cycle_seconds
            .get_metric_with_label_values(&["timeout"])
            .map_err(|e| NGError::from(format!("Failed to get collector cycle(timeout): {e}")))?;

        let permits_current = concurrency_permits
            .get_metric_with_label_values(&["current"])
            .map_err(|e| NGError::from(format!("Failed to get permits(current): {e}")))?;
        let permits_available = concurrency_permits
            .get_metric_with_label_values(&["available"])
            .map_err(|e| NGError::from(format!("Failed to get permits(available): {e}")))?;

        let retries_total = IntCounterVec::new(
            opts!(
                "collector_retries_total",
                "Total collector retries labeled by reason (timeout|error)."
            ),
            &["reason"],
        )
        .map_err(|e| NGError::from(format!("Failed to create collector_retries_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(retries_total.clone()),
            "collector_retries_total",
        );

        let retries_timeout_total = retries_total
            .get_metric_with_label_values(&["timeout"])
            .map_err(|e| NGError::from(format!("Failed to get retries(timeout): {e}")))?;
        let retries_error_total = retries_total
            .get_metric_with_label_values(&["error"])
            .map_err(|e| NGError::from(format!("Failed to get retries(error): {e}")))?;

        Ok(Self {
            active_tasks,
            permits_current,
            permits_available,
            cycles_success_total,
            cycles_fail_total,
            cycles_timeout_total,
            cycle_success_seconds,
            cycle_fail_seconds,
            cycle_timeout_seconds,
            retries_timeout_total,
            retries_error_total,
            avg_cycle_ns: AtomicU64::new(0),
            last_update: RwLock::new(None),
        })
    }

    #[inline]
    fn touch(&self) {
        if let Ok(mut g) = self.last_update.write() {
            *g = Some(Utc::now());
        }
    }

    /// Record one collection cycle with result and duration.
    ///
    /// # Notes
    /// - This is intended to be called on the hot path (per group call).
    /// - Snapshot average uses EWMA to avoid expensive histogram math.
    #[inline]
    pub(crate) fn record_cycle(
        &self,
        result: CollectorResult,
        elapsed_ns: u64,
        elapsed_seconds: f64,
    ) {
        match result {
            CollectorResult::Success => {
                self.cycles_success_total.inc_by(1);
                self.cycle_success_seconds.observe(elapsed_seconds);
            }
            CollectorResult::Fail => {
                self.cycles_fail_total.inc_by(1);
                self.cycle_fail_seconds.observe(elapsed_seconds);
            }
            CollectorResult::Timeout => {
                self.cycles_timeout_total.inc_by(1);
                self.cycle_timeout_seconds.observe(elapsed_seconds);
            }
        }

        // EWMA average (ns)
        let old = self.avg_cycle_ns.load(Ordering::Relaxed);
        let new = if old == 0 {
            elapsed_ns
        } else {
            (old * 9 + elapsed_ns) / 10
        };
        self.avg_cycle_ns.store(new, Ordering::Relaxed);
        self.touch();
    }

    /// Set number of active channel collection tasks.
    #[inline]
    pub(crate) fn set_active_tasks(&self, value: u64) {
        self.active_tasks.set(value as i64);
        self.touch();
    }

    /// Set current and available concurrency permits.
    #[inline]
    pub(crate) fn set_concurrency_permits(&self, current: u64, available: u64) {
        self.permits_current.set(current as i64);
        self.permits_available.set(available as i64);
        self.touch();
    }

    /// Increment retries counter for timeout-triggered retries.
    #[inline]
    pub(crate) fn inc_retries_timeout(&self) {
        self.retries_timeout_total.inc_by(1);
        self.touch();
    }

    /// Increment retries counter for error-triggered retries.
    #[inline]
    pub(crate) fn inc_retries_error(&self) {
        self.retries_error_total.inc_by(1);
        self.touch();
    }

    /// Snapshot collector metrics for REST/WS consumers.
    pub(crate) fn snapshot(&self) -> CollectorMetricsSnapshot {
        let success = self.cycles_success_total.get();
        let fail = self.cycles_fail_total.get();
        let timeout = self.cycles_timeout_total.get();
        let total = success + fail + timeout;

        CollectorMetricsSnapshot {
            total_collections: total,
            successful_collections: success,
            failed_collections: fail,
            timeout_collections: timeout,
            average_collection_time_ms: self.avg_cycle_ns.load(Ordering::Relaxed) as f64
                / 1_000_000.0,
            active_tasks: self.active_tasks.get().max(0) as usize,
            // This snapshot schema is historical and currently stores permit gauges as usize.
            current_permits: self.permits_current.get().max(0) as usize,
            available_permits: self.permits_available.get().max(0) as usize,
            // Not yet implemented; keep stable placeholder for now.
            batch_efficiency: 0.0,
            retries_total: self.retries_timeout_total.get() + self.retries_error_total.get(),
            retries_timeout_total: self.retries_timeout_total.get(),
            retries_error_total: self.retries_error_total.get(),
        }
    }
}
