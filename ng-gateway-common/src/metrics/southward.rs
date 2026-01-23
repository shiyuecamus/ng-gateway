//! Southward (channel/driver) metrics.
//!
//! This module provides low-cardinality Prometheus metrics for the southward subsystem,
//! aggregated by `(channel_id, driver)`.
//!
//! # Cardinality rules
//! - Labels must remain bounded: `channel_id`, `driver`.
//! - Do NOT include device_id / point_id / error_message / endpoint in labels.

use chrono::{DateTime, Utc};
use dashmap::{mapref::entry::Entry, DashMap};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::{ChannelMetricsSnapshot, SouthwardManagerMetricsSnapshot};
use prometheus::{
    core::Collector, opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Registry,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use tracing::warn;

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

/// Pre-resolved metric handles for one `(channel_id, driver)` pair.
#[derive(Debug)]
pub struct SouthwardChannelMetricHandles {
    connected: IntGauge,
    state: IntGauge,
    reconnect_total: IntCounter,
    io_success_total: IntCounter,
    io_failed_total: IntCounter,
    io_latency_success_seconds: Histogram,
    io_latency_failed_seconds: Histogram,
    // southward collection (per channel)
    collect_cycle_success_seconds: Histogram,
    collect_cycle_fail_seconds: Histogram,
    collect_cycle_timeout_seconds: Histogram,
    point_read_success_total: IntCounter,
    point_read_fail_total: IntCounter,
    point_read_timeout_total: IntCounter,
    /// Snapshot-only latency state for REST/WS snapshots (EWMA + last).
    avg_latency_ns: AtomicU64,
    /// Snapshot-only latency state for REST/WS snapshots (EWMA + last).
    last_latency_ns: AtomicU64,
}

impl SouthwardChannelMetricHandles {
    /// Set channel connected gauge (0/1).
    #[inline]
    pub fn set_connected(&self, connected: bool) {
        self.connected.set(if connected { 1 } else { 0 });
    }

    /// Set channel state numeric gauge.
    #[inline]
    pub fn set_state_value(&self, value: i64) {
        self.state.set(value);
    }

    /// Increment reconnect counter.
    #[inline]
    pub fn inc_reconnect(&self) {
        self.reconnect_total.inc();
    }

    /// Record a successful southward I/O operation with latency.
    #[inline]
    pub fn record_io_success(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.io_success_total.inc();
        self.io_latency_success_seconds.observe(elapsed_seconds);
        self.record_latency_ns(elapsed_ns);
    }

    /// Record a failed southward I/O operation with latency.
    #[inline]
    pub fn record_io_failed(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.io_failed_total.inc();
        self.io_latency_failed_seconds.observe(elapsed_seconds);
        self.record_latency_ns(elapsed_ns);
    }

    /// Record one successful southward collection cycle.
    #[inline]
    pub fn record_collect_success(&self, points: u64, elapsed_seconds: f64) {
        self.collect_cycle_success_seconds.observe(elapsed_seconds);
        if points > 0 {
            self.point_read_success_total.inc_by(points);
        }
    }

    /// Record one failed southward collection cycle.
    #[inline]
    pub fn record_collect_fail(&self, points: u64, elapsed_seconds: f64) {
        self.collect_cycle_fail_seconds.observe(elapsed_seconds);
        if points > 0 {
            self.point_read_fail_total.inc_by(points);
        }
    }

    /// Record one timed-out southward collection cycle.
    #[inline]
    pub fn record_collect_timeout(&self, points: u64, elapsed_seconds: f64) {
        self.collect_cycle_timeout_seconds.observe(elapsed_seconds);
        if points > 0 {
            self.point_read_timeout_total.inc_by(points);
        }
    }

    /// Build a channel metrics snapshot for REST/WS.
    pub fn snapshot_metrics(&self) -> ChannelMetricsSnapshot {
        let successful = self.io_success_total.get();
        let failed = self.io_failed_total.get();
        let total = successful + failed;
        let (avg_ns, last_ns) = self.snapshot_latency_ns();

        ChannelMetricsSnapshot {
            total_operations: total,
            successful_operations: successful,
            failed_operations: failed,
            average_response_time: chrono::Duration::nanoseconds(
                (avg_ns.min(i64::MAX as u64)) as i64,
            ),
            last_operation_time: chrono::Duration::nanoseconds(
                (last_ns.min(i64::MAX as u64)) as i64,
            ),
            bytes_sent: 0,
            bytes_received: 0,
            reconnection_count: self.reconnect_total.get() as u32,
        }
    }

    /// Record latency (ns) into snapshot EWMA and last value.
    #[inline]
    fn record_latency_ns(&self, elapsed_ns: u64) {
        self.last_latency_ns.store(elapsed_ns, Ordering::Relaxed);

        // EWMA for cheap REST/WS snapshots.
        let old = self.avg_latency_ns.load(Ordering::Relaxed);
        let new = if old == 0 {
            elapsed_ns
        } else {
            (old * 9 + elapsed_ns) / 10
        };
        self.avg_latency_ns.store(new, Ordering::Relaxed);
    }

    #[inline]
    fn snapshot_latency_ns(&self) -> (u64, u64) {
        let avg = self.avg_latency_ns.load(Ordering::Relaxed);
        let last = self.last_latency_ns.load(Ordering::Relaxed);
        (avg, last)
    }
}

/// Southward metrics owned by `NGMetricsHub`.
#[derive(Debug)]
pub(crate) struct SouthwardMetricsHub {
    // manager-level (no labels)
    channels_total: IntGauge,
    channels_connected: IntGauge,
    // snapshot fields (single source of truth for REST/WS)
    total_channels: AtomicU64,
    connected_channels: AtomicU64,
    total_devices: AtomicU64,
    active_devices: AtomicU64,
    total_data_points: AtomicU64,
    total_actions: AtomicU64,
    last_update: RwLock<Option<DateTime<Utc>>>,

    // per-channel (labels: channel_id, driver)
    channel_connected: IntGaugeVec,
    channel_state: IntGaugeVec,
    channel_reconnect_total: IntCounterVec,
    channel_io_total: IntCounterVec,
    channel_collect_cycle_seconds: HistogramVec,
    channel_point_read_total: IntCounterVec,
    channel_io_latency_seconds: HistogramVec,
    channels: DashMap<(i32, String), Arc<SouthwardChannelMetricHandles>>,
}

impl SouthwardMetricsHub {
    /// Create and register southward metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let channels_total = IntGauge::new(
            "southward_channels_total",
            "Total number of southward channels.",
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_channels_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(channels_total.clone()),
            "southward_channels_total",
        );

        let channels_connected = IntGauge::new(
            "southward_channels_connected",
            "Number of currently connected southward channels.",
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_channels_connected: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channels_connected.clone()),
            "southward_channels_connected",
        );

        let channel_connected = IntGaugeVec::new(
            opts!(
                "southward_channel_connected",
                "Southward channel connected state (1=connected, 0=not connected)."
            ),
            &["channel_id", "driver"],
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_channel_connected: {e}")))?;
        register_collector_into(
            registry,
            Box::new(channel_connected.clone()),
            "southward_channel_connected",
        );

        let channel_state = IntGaugeVec::new(
            opts!(
                "southward_channel_state",
                "Southward channel state numeric value."
            ),
            &["channel_id", "driver"],
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_channel_state: {e}")))?;
        register_collector_into(
            registry,
            Box::new(channel_state.clone()),
            "southward_channel_state",
        );

        let channel_reconnect_total = IntCounterVec::new(
            opts!(
                "southward_channel_reconnect_total",
                "Total reconnect attempts for southward channel connection."
            ),
            &["channel_id", "driver"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_channel_reconnect_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channel_reconnect_total.clone()),
            "southward_channel_reconnect_total",
        );

        let channel_io_total = IntCounterVec::new(
            opts!(
                "southward_io_total",
                "Total southward I/O operations (collect/write/execute), labeled by result."
            ),
            &["channel_id", "driver", "result"],
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_io_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(channel_io_total.clone()),
            "southward_io_total",
        );

        let collect_opts = HistogramOpts::new(
            "southward_collect_cycle_seconds",
            "Duration of southward collection cycles (seconds), labeled by result.",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let channel_collect_cycle_seconds =
            HistogramVec::new(collect_opts, &["channel_id", "driver", "result"]).map_err(|e| {
                NGError::from(format!(
                    "Failed to create southward_collect_cycle_seconds: {e}"
                ))
            })?;
        register_collector_into(
            registry,
            Box::new(channel_collect_cycle_seconds.clone()),
            "southward_collect_cycle_seconds",
        );

        let channel_point_read_total = IntCounterVec::new(
            opts!(
                "southward_point_read_total",
                "Total points read by southward collection, labeled by result."
            ),
            &["channel_id", "driver", "result"],
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_point_read_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(channel_point_read_total.clone()),
            "southward_point_read_total",
        );

        let latency_opts = HistogramOpts::new(
            "southward_io_latency_seconds",
            "Latency of southward I/O operations (seconds), labeled by result.",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let channel_io_latency_seconds =
            HistogramVec::new(latency_opts, &["channel_id", "driver", "result"]).map_err(|e| {
                NGError::from(format!(
                    "Failed to create southward_io_latency_seconds: {e}"
                ))
            })?;
        register_collector_into(
            registry,
            Box::new(channel_io_latency_seconds.clone()),
            "southward_io_latency_seconds",
        );

        Ok(Self {
            channels_total,
            channels_connected,
            total_channels: AtomicU64::new(0),
            connected_channels: AtomicU64::new(0),
            total_devices: AtomicU64::new(0),
            active_devices: AtomicU64::new(0),
            total_data_points: AtomicU64::new(0),
            total_actions: AtomicU64::new(0),
            last_update: RwLock::new(None),
            channel_connected,
            channel_state,
            channel_reconnect_total,
            channel_io_total,
            channel_collect_cycle_seconds,
            channel_point_read_total,
            channel_io_latency_seconds,
            channels: DashMap::new(),
        })
    }

    /// Touch the manager snapshot timestamp.
    #[inline]
    pub(crate) fn touch_manager_snapshot(&self) {
        if let Ok(mut g) = self.last_update.write() {
            *g = Some(Utc::now());
        }
    }

    /// Set total channels for southward manager snapshot and Prometheus gauge.
    #[inline]
    pub(crate) fn set_total_channels(&self, value: u64) {
        self.total_channels.store(value, Ordering::Relaxed);
        self.channels_total.set(value as i64);
        self.touch_manager_snapshot();
    }

    /// Set connected channels for southward manager snapshot and Prometheus gauge.
    #[inline]
    pub(crate) fn set_connected_channels(&self, value: u64) {
        self.connected_channels.store(value, Ordering::Relaxed);
        self.channels_connected.set(value as i64);
        self.touch_manager_snapshot();
    }

    /// Increment connected channels counter and keep gauge in sync.
    ///
    /// # Notes
    /// This is intended to be used by the southward connection monitor on state transitions
    /// to avoid O(n) scans when generating gateway snapshots.
    #[inline]
    pub(crate) fn inc_connected_channels(&self) {
        let new = self.connected_channels.fetch_add(1, Ordering::Relaxed) + 1;
        self.channels_connected.set(new as i64);
        self.touch_manager_snapshot();
    }

    /// Decrement connected channels counter (saturating) and keep gauge in sync.
    ///
    /// # Notes
    /// Saturates at 0 on underflow (best-effort).
    #[inline]
    pub(crate) fn dec_connected_channels(&self) {
        let prev = self.connected_channels.fetch_sub(1, Ordering::Relaxed);
        let new = prev.saturating_sub(1);
        if prev == 0 {
            self.connected_channels.store(0, Ordering::Relaxed);
        }
        self.channels_connected.set(new as i64);
        self.touch_manager_snapshot();
    }

    /// Set total devices for southward manager snapshot.
    #[inline]
    pub(crate) fn set_total_devices(&self, value: u64) {
        self.total_devices.store(value, Ordering::Relaxed);
        self.touch_manager_snapshot();
    }

    /// Set active devices for southward manager snapshot.
    #[inline]
    pub(crate) fn set_active_devices(&self, value: u64) {
        self.active_devices.store(value, Ordering::Relaxed);
        self.touch_manager_snapshot();
    }

    /// Set total data points for southward manager snapshot.
    #[inline]
    pub(crate) fn set_total_data_points(&self, value: u64) {
        self.total_data_points.store(value, Ordering::Relaxed);
        self.touch_manager_snapshot();
    }

    /// Set total actions for southward manager snapshot.
    #[inline]
    pub(crate) fn set_total_actions(&self, value: u64) {
        self.total_actions.store(value, Ordering::Relaxed);
        self.touch_manager_snapshot();
    }

    /// Snapshot southward manager metrics for REST/WS consumers.
    pub(crate) fn snapshot_manager(&self) -> SouthwardManagerMetricsSnapshot {
        let total_channels = self.total_channels.load(Ordering::Relaxed);
        let connected_channels = self.connected_channels.load(Ordering::Relaxed);
        let total_devices = self.total_devices.load(Ordering::Relaxed);
        let total_data_points = self.total_data_points.load(Ordering::Relaxed);

        let average_points_per_device = if total_devices > 0 {
            total_data_points as f64 / total_devices as f64
        } else {
            0.0
        };

        SouthwardManagerMetricsSnapshot {
            total_channels: total_channels as usize,
            connected_channels: connected_channels as usize,
            total_devices: total_devices as usize,
            active_devices: self.active_devices.load(Ordering::Relaxed) as usize,
            total_data_points: total_data_points as usize,
            total_actions: self.total_actions.load(Ordering::Relaxed) as usize,
            average_points_per_device,
            last_update: self.last_update.read().ok().and_then(|g| *g),
        }
    }

    /// Get a channel metrics snapshot for REST/WS.
    #[inline]
    pub(crate) fn snapshot_channel_metrics(
        &self,
        channel_id: i32,
        driver: &str,
    ) -> ChannelMetricsSnapshot {
        self.channels
            .get(&(channel_id, driver.to_string()))
            .map(|h| h.snapshot_metrics())
            .unwrap_or_default()
    }

    /// Register a channel and return its pre-resolved metric handles.
    pub(crate) fn register_channel(
        &self,
        channel_id: i32,
        driver: String,
    ) -> NGResult<Arc<SouthwardChannelMetricHandles>> {
        match self.channels.entry((channel_id, driver.clone())) {
            Entry::Occupied(e) => Ok(Arc::clone(e.get())),
            Entry::Vacant(v) => {
                let channel_id_s = channel_id.to_string();
                let labels = [channel_id_s.as_str(), driver.as_str()];

                let connected = self
                    .channel_connected
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_connected for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let state = self
                    .channel_state
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_state for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let reconnect_total = self
                    .channel_reconnect_total
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_reconnect_total for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let success_labels = [labels[0], labels[1], "success"];
                let failed_labels = [labels[0], labels[1], "failed"];
                let timeout_labels = [labels[0], labels[1], "timeout"];

                let io_success_total = self
                    .channel_io_total
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_io_total(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let io_failed_total = self
                    .channel_io_total
                    .get_metric_with_label_values(&failed_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_io_total(failed) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let io_latency_success_seconds = self
                    .channel_io_latency_seconds
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_io_latency_seconds(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let io_latency_failed_seconds = self
                    .channel_io_latency_seconds
                    .get_metric_with_label_values(&failed_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_io_latency_seconds(failed) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let collect_cycle_success_seconds = self
                    .channel_collect_cycle_seconds
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get collect_cycle_seconds(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let collect_cycle_fail_seconds = self
                    .channel_collect_cycle_seconds
                    .get_metric_with_label_values(&failed_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get collect_cycle_seconds(fail) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let collect_cycle_timeout_seconds = self
                    .channel_collect_cycle_seconds
                    .get_metric_with_label_values(&timeout_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get collect_cycle_seconds(timeout) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let point_read_success_total = self
                    .channel_point_read_total
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get point_read_total(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let point_read_fail_total = self
                    .channel_point_read_total
                    .get_metric_with_label_values(&failed_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get point_read_total(fail) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let point_read_timeout_total = self
                    .channel_point_read_total
                    .get_metric_with_label_values(&timeout_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get point_read_total(timeout) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let handles = Arc::new(SouthwardChannelMetricHandles {
                    connected,
                    state,
                    reconnect_total,
                    io_success_total,
                    io_failed_total,
                    io_latency_success_seconds,
                    io_latency_failed_seconds,
                    collect_cycle_success_seconds,
                    collect_cycle_fail_seconds,
                    collect_cycle_timeout_seconds,
                    point_read_success_total,
                    point_read_fail_total,
                    point_read_timeout_total,
                    avg_latency_ns: AtomicU64::new(0),
                    last_latency_ns: AtomicU64::new(0),
                });
                v.insert(Arc::clone(&handles));
                Ok(handles)
            }
        }
    }
}
