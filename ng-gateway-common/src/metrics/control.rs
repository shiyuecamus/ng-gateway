//! Control-plane metrics (write-point / execute) for NG Gateway.
//!
//! This module provides low-cardinality Prometheus metrics for control-plane write paths,
//! aggregated by `(channel_id, driver)`.
//!
//! # Cardinality rules
//! - Labels must remain bounded: `channel_id`, `driver`, `result`.
//! - Do NOT include device_id / point_id / error_message in labels.

use dashmap::{mapref::entry::Entry, DashMap};
use ng_gateway_error::{NGError, NGResult};
use prometheus::{
    core::Collector, opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    Registry,
};
use std::sync::Arc;
use tracing::warn;

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

/// Bounded result label for control-plane operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlResult {
    Success,
    Fail,
    Timeout,
}

impl ControlResult {
    #[inline]
    pub const fn as_label(self) -> &'static str {
        match self {
            ControlResult::Success => "success",
            ControlResult::Fail => "fail",
            ControlResult::Timeout => "timeout",
        }
    }
}

/// Pre-resolved metric handles for one `(channel_id, driver)` pair.
#[derive(Debug)]
pub struct ControlChannelMetricHandles {
    write_requests_success_total: IntCounter,
    write_requests_fail_total: IntCounter,
    write_requests_timeout_total: IntCounter,
    write_queue_wait_seconds: Histogram,
    write_execute_success_seconds: Histogram,
    write_execute_fail_seconds: Histogram,
    write_execute_timeout_seconds: Histogram,
}

impl ControlChannelMetricHandles {
    /// Increment write requests total by result.
    #[inline]
    pub fn inc_write_request(&self, result: ControlResult) {
        match result {
            ControlResult::Success => self.write_requests_success_total.inc(),
            ControlResult::Fail => self.write_requests_fail_total.inc(),
            ControlResult::Timeout => self.write_requests_timeout_total.inc(),
        }
    }

    /// Observe queue wait time (seconds) for per-channel write serialization.
    #[inline]
    pub fn observe_write_queue_wait_seconds(&self, seconds: f64) {
        self.write_queue_wait_seconds.observe(seconds);
    }

    /// Observe write execution time (seconds) by result.
    #[inline]
    pub fn observe_write_execute_seconds(&self, result: ControlResult, seconds: f64) {
        match result {
            ControlResult::Success => self.write_execute_success_seconds.observe(seconds),
            ControlResult::Fail => self.write_execute_fail_seconds.observe(seconds),
            ControlResult::Timeout => self.write_execute_timeout_seconds.observe(seconds),
        }
    }
}

/// Control-plane metrics owned by `NGMetricsHub`.
#[derive(Debug)]
pub(crate) struct ControlMetricsHub {
    write_requests_total: IntCounterVec,
    write_queue_wait_seconds: HistogramVec,
    write_execute_seconds: HistogramVec,
    channels: DashMap<(i32, String), Arc<ControlChannelMetricHandles>>,
}

impl ControlMetricsHub {
    /// Create and register control-plane metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let write_requests_total = IntCounterVec::new(
            opts!(
                "control_write_requests_total",
                "Total control-plane write requests labeled by result."
            ),
            &["channel_id", "driver", "result"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create control_write_requests_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(write_requests_total.clone()),
            "control_write_requests_total",
        );

        let wait_opts = HistogramOpts::new(
            "control_write_queue_wait_seconds",
            "Time spent waiting for per-channel write serialization (seconds).",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let write_queue_wait_seconds = HistogramVec::new(wait_opts, &["channel_id", "driver"])
            .map_err(|e| {
                NGError::from(format!(
                    "Failed to create control_write_queue_wait_seconds: {e}"
                ))
            })?;
        register_collector_into(
            registry,
            Box::new(write_queue_wait_seconds.clone()),
            "control_write_queue_wait_seconds",
        );

        let exec_opts = HistogramOpts::new(
            "control_write_execute_seconds",
            "Time spent executing driver write-point (seconds), labeled by result.",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let write_execute_seconds =
            HistogramVec::new(exec_opts, &["channel_id", "driver", "result"]).map_err(|e| {
                NGError::from(format!(
                    "Failed to create control_write_execute_seconds: {e}"
                ))
            })?;
        register_collector_into(
            registry,
            Box::new(write_execute_seconds.clone()),
            "control_write_execute_seconds",
        );

        Ok(Self {
            write_requests_total,
            write_queue_wait_seconds,
            write_execute_seconds,
            channels: DashMap::new(),
        })
    }

    /// Register a channel and return its pre-resolved control metric handles.
    pub(crate) fn register_channel(
        &self,
        channel_id: i32,
        driver: String,
    ) -> NGResult<Arc<ControlChannelMetricHandles>> {
        match self.channels.entry((channel_id, driver.clone())) {
            Entry::Occupied(e) => Ok(Arc::clone(e.get())),
            Entry::Vacant(v) => {
                let channel_id_s = channel_id.to_string();
                let base = [channel_id_s.as_str(), driver.as_str()];

                let success_labels = [base[0], base[1], ControlResult::Success.as_label()];
                let fail_labels = [base[0], base[1], ControlResult::Fail.as_label()];
                let timeout_labels = [base[0], base[1], ControlResult::Timeout.as_label()];

                let write_requests_success_total = self
                    .write_requests_total
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_requests_total(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let write_requests_fail_total = self
                    .write_requests_total
                    .get_metric_with_label_values(&fail_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_requests_total(fail) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let write_requests_timeout_total = self
                    .write_requests_total
                    .get_metric_with_label_values(&timeout_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_requests_total(timeout) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let write_queue_wait_seconds = self
                    .write_queue_wait_seconds
                    .get_metric_with_label_values(&base)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_queue_wait_seconds for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let write_execute_success_seconds = self
                    .write_execute_seconds
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_execute_seconds(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let write_execute_fail_seconds = self
                    .write_execute_seconds
                    .get_metric_with_label_values(&fail_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_execute_seconds(fail) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let write_execute_timeout_seconds = self
                    .write_execute_seconds
                    .get_metric_with_label_values(&timeout_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get write_execute_seconds(timeout) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let handles = Arc::new(ControlChannelMetricHandles {
                    write_requests_success_total,
                    write_requests_fail_total,
                    write_requests_timeout_total,
                    write_queue_wait_seconds,
                    write_execute_success_seconds,
                    write_execute_fail_seconds,
                    write_execute_timeout_seconds,
                });
                v.insert(Arc::clone(&handles));
                Ok(handles)
            }
        }
    }
}
