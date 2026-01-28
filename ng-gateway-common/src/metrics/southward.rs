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
use ng_gateway_models::core::metrics::{
    ChannelMetricsSnapshot, DeviceMetricsSnapshot, SouthwardManagerMetricsSnapshot,
};
use prometheus::{
    core::Collector, opts, Histogram, HistogramOpts, HistogramVec, IntCounter, IntCounterVec,
    IntGauge, IntGaugeVec, Registry,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use tracing::warn;

#[derive(Debug, Default)]
struct SouthwardDeviceMetricEntry {
    collect_success_total: AtomicU64,
    collect_fail_total: AtomicU64,
    collect_timeout_total: AtomicU64,
    report_success_total: AtomicU64,
    report_dropped_total: AtomicU64,
    report_fail_total: AtomicU64,
    last_report_ms: AtomicU64,
    // EWMA + last collection latency (ns)
    avg_collect_latency_ns: AtomicU64,
    last_collect_latency_ns: AtomicU64,
    // unix millis for last activity (collection end)
    last_activity_ms: AtomicU64,
}

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CollectBatchOutcome {
    pub ok: bool,
    pub is_timeout: bool,
    pub points: u64,
    pub elapsed_ns: u64,
    pub elapsed_seconds: f64,
    pub now_ms: u64,
}

/// Pre-resolved metric handles for one `(channel_id, driver)` pair.
#[derive(Debug)]
pub struct SouthwardChannelMetricHandles {
    connected: IntGauge,
    state: IntGauge,
    reconnect_total: IntCounter,
    connect_failed_total: IntCounter,
    disconnect_total: IntCounter,
    io_success_total: IntCounter,
    io_failed_total: IntCounter,
    bytes_sent_total: IntCounter,
    bytes_received_total: IntCounter,
    io_latency_success_seconds: Histogram,
    io_latency_failed_seconds: Histogram,
    // southward collection (per channel)
    collect_cycle_success_seconds: Histogram,
    collect_cycle_fail_seconds: Histogram,
    collect_cycle_timeout_seconds: Histogram,
    point_read_success_total: IntCounter,
    point_read_fail_total: IntCounter,
    point_read_timeout_total: IntCounter,

    // report/push (publisher.try_publish)
    report_publish_success_total: IntCounter,
    report_publish_dropped_total: IntCounter,
    report_publish_fail_total: IntCounter,

    /// Snapshot-only latency state for REST/WS snapshots (EWMA + last).
    avg_latency_ns: AtomicU64,
    /// Snapshot-only latency state for REST/WS snapshots (EWMA + last).
    last_latency_ns: AtomicU64,
    /// Snapshot-only timestamps (unix millis) for REST/WS snapshots.
    last_state_change_ms: AtomicU64,
    /// Snapshot-only timestamps (unix millis) for REST/WS snapshots.
    last_report_ms: AtomicU64,

    // per-device (in-memory, non-Prometheus): owned by this channel handle
    device_metrics: DashMap<i32, SouthwardDeviceMetricEntry>,
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

    /// Increment connect failed counter.
    #[inline]
    pub fn inc_connect_failed(&self) {
        self.connect_failed_total.inc();
    }

    /// Increment disconnect counter.
    #[inline]
    pub fn inc_disconnect(&self) {
        self.disconnect_total.inc();
    }

    /// Record a state change timestamp (unix millis).
    #[inline]
    pub fn record_state_change_ms(&self, now_ms: u64) {
        if now_ms > 0 {
            self.last_state_change_ms.store(now_ms, Ordering::Relaxed);
        }
    }

    /// Record one publish attempt from a report/subscription driver.
    #[inline]
    pub fn record_report_success(&self, now_ms: u64) {
        self.report_publish_success_total.inc();
        if now_ms > 0 {
            self.last_report_ms.store(now_ms, Ordering::Relaxed);
        }
    }

    /// Record a dropped publish attempt due to backpressure.
    #[inline]
    pub fn record_report_dropped(&self) {
        self.report_publish_dropped_total.inc();
    }

    /// Record a failed publish attempt (closed/other errors).
    #[inline]
    pub fn record_report_fail(&self) {
        self.report_publish_fail_total.inc();
    }

    /// Best-practice: record a report publish outcome for a specific device.
    ///
    /// This is the **single entrypoint** for Report/Subscribe drivers to update:
    /// - channel-level report counters (`southward_report_publish_total{result=...}`)
    /// - per-device in-memory report counters + timestamps (WS/UI rows)
    #[inline]
    pub fn record_device_report_success(&self, device_id: i32, now_ms: u64) {
        self.record_report_success(now_ms);
        let entry = self
            .device_metrics
            .entry(device_id)
            .or_insert_with(SouthwardDeviceMetricEntry::default);
        entry.report_success_total.fetch_add(1, Ordering::Relaxed);
        entry.last_report_ms.store(now_ms, Ordering::Relaxed);
        entry.last_activity_ms.store(now_ms, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_device_report_dropped(&self, device_id: i32, now_ms: u64) {
        self.record_report_dropped();
        let entry = self
            .device_metrics
            .entry(device_id)
            .or_insert_with(SouthwardDeviceMetricEntry::default);
        entry.report_dropped_total.fetch_add(1, Ordering::Relaxed);
        entry.last_report_ms.store(now_ms, Ordering::Relaxed);
        entry.last_activity_ms.store(now_ms, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_device_report_fail(&self, device_id: i32, now_ms: u64) {
        self.record_report_fail();
        let entry = self
            .device_metrics
            .entry(device_id)
            .or_insert_with(SouthwardDeviceMetricEntry::default);
        entry.report_fail_total.fetch_add(1, Ordering::Relaxed);
        entry.last_report_ms.store(now_ms, Ordering::Relaxed);
        entry.last_activity_ms.store(now_ms, Ordering::Relaxed);
    }

    /// Add measured transport bytes sent (gateway -> field).
    #[inline]
    pub fn add_bytes_sent(&self, bytes: u64) {
        if bytes > 0 {
            self.bytes_sent_total.inc_by(bytes);
        }
    }

    /// Add measured transport bytes received (field -> gateway).
    #[inline]
    pub fn add_bytes_received(&self, bytes: u64) {
        if bytes > 0 {
            self.bytes_received_total.inc_by(bytes);
        }
    }

    /// Record one per-device collection result (in-memory).
    #[inline]
    pub fn record_device_collect_result(
        &self,
        device_id: i32,
        ok: bool,
        is_timeout: bool,
        elapsed_ns: u64,
        now_ms: u64,
    ) {
        let entry = self
            .device_metrics
            .entry(device_id)
            .or_insert_with(SouthwardDeviceMetricEntry::default);

        if ok {
            entry.collect_success_total.fetch_add(1, Ordering::Relaxed);
        } else if is_timeout {
            entry.collect_timeout_total.fetch_add(1, Ordering::Relaxed);
        } else {
            entry.collect_fail_total.fetch_add(1, Ordering::Relaxed);
        }

        entry
            .last_collect_latency_ns
            .store(elapsed_ns, Ordering::Relaxed);
        let old = entry.avg_collect_latency_ns.load(Ordering::Relaxed);
        let new = if old == 0 {
            elapsed_ns
        } else {
            (old * 9 + elapsed_ns) / 10
        };
        entry.avg_collect_latency_ns.store(new, Ordering::Relaxed);
        entry.last_activity_ms.store(now_ms, Ordering::Relaxed);
    }

    #[inline]
    fn ns_to_ms(ns: u64) -> u64 {
        // Best-effort: truncate sub-ms fractions.
        ns / 1_000_000
    }

    /// Snapshot per-device metrics for WS/UI. Returns `None` if never seen.
    #[inline]
    pub fn snapshot_device_metrics(&self, device_id: i32) -> Option<DeviceMetricsSnapshot> {
        self.device_metrics.get(&device_id).map(|e| {
            let v = e.value();

            DeviceMetricsSnapshot {
                collect_success_total: v.collect_success_total.load(Ordering::Relaxed),
                collect_fail_total: v.collect_fail_total.load(Ordering::Relaxed),
                collect_timeout_total: v.collect_timeout_total.load(Ordering::Relaxed),
                avg_collect_latency_ms: Self::ns_to_ms(
                    v.avg_collect_latency_ns.load(Ordering::Relaxed),
                ),
                last_collect_latency_ms: Self::ns_to_ms(
                    v.last_collect_latency_ns.load(Ordering::Relaxed),
                ),
                report_success_total: v.report_success_total.load(Ordering::Relaxed),
                report_dropped_total: v.report_dropped_total.load(Ordering::Relaxed),
                report_fail_total: v.report_fail_total.load(Ordering::Relaxed),
                last_report_ms: v.last_report_ms.load(Ordering::Relaxed),
                last_activity_ms: v.last_activity_ms.load(Ordering::Relaxed),
            }
        })
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

    /// Record one batched collection attempt and attribute the outcome to:
    /// - channel-level collect metrics (Prometheus, low-cardinality)
    /// - per-device collect metrics (in-memory, snapshot-only)
    ///
    /// # Semantics
    /// - This function treats a timeout as an **I/O failure** at the channel level, while
    ///   still recording a distinct *collect-timeout* outcome for UI friendliness.
    /// - The outcome is attributed to every `device_id` in the batch. This matches the
    ///   batching semantics of `driver.collect_data(&[(dev, pts)])` where per-device errors
    ///   are not currently surfaced.
    #[inline]
    pub fn record_collect_batch<DeviceIds>(
        &self,
        device_ids: DeviceIds,
        outcome: CollectBatchOutcome,
    ) where
        DeviceIds: IntoIterator<Item = i32>,
    {
        if outcome.ok {
            self.record_io_success(outcome.elapsed_ns, outcome.elapsed_seconds);
            self.record_collect_success(outcome.points, outcome.elapsed_seconds);
        } else {
            self.record_io_failed(outcome.elapsed_ns, outcome.elapsed_seconds);
            if outcome.is_timeout {
                self.record_collect_timeout(outcome.points, outcome.elapsed_seconds);
            } else {
                self.record_collect_fail(outcome.points, outcome.elapsed_seconds);
            }
        }

        for device_id in device_ids {
            self.record_device_collect_result(
                device_id,
                outcome.ok,
                outcome.is_timeout,
                outcome.elapsed_ns,
                outcome.now_ms,
            );
        }
    }

    #[inline]
    fn from_ms(ms: u64) -> Option<DateTime<Utc>> {
        if ms == 0 {
            return None;
        }
        DateTime::<Utc>::from_timestamp_millis(ms as i64)
    }

    /// Build a channel metrics snapshot for REST/WS.
    pub fn snapshot_metrics(&self) -> ChannelMetricsSnapshot {
        let successful = self.io_success_total.get();
        let failed = self.io_failed_total.get();
        let total = successful + failed;
        let (avg_ns, last_ns) = self.snapshot_latency_ns();
        let last_state_change_ms = self.last_state_change_ms.load(Ordering::Relaxed);
        let last_report_ms = self.last_report_ms.load(Ordering::Relaxed);

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
            bytes_sent: self.bytes_sent_total.get(),
            bytes_received: self.bytes_received_total.get(),
            reconnection_count: self.reconnect_total.get() as u32,

            point_read_success_total: self.point_read_success_total.get(),
            point_read_fail_total: self.point_read_fail_total.get(),
            point_read_timeout_total: self.point_read_timeout_total.get(),

            connect_failed_count: self.connect_failed_total.get(),
            disconnect_count: self.disconnect_total.get(),
            last_state_change_at: Self::from_ms(last_state_change_ms),

            report_publish_success_total: self.report_publish_success_total.get(),
            report_publish_dropped_total: self.report_publish_dropped_total.get(),
            report_publish_fail_total: self.report_publish_fail_total.get(),
            last_report_at: Self::from_ms(last_report_ms),
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
    devices_total: IntGauge,
    data_points_total: IntGauge,
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
    channel_connect_failed_total: IntCounterVec,
    channel_disconnect_total: IntCounterVec,
    channel_io_total: IntCounterVec,
    channel_bytes_total: IntCounterVec,
    channel_report_publish_total: IntCounterVec,
    channel_collect_cycle_seconds: HistogramVec,
    channel_point_read_total: IntCounterVec,
    channel_io_latency_seconds: HistogramVec,
    channels: DashMap<(i32, Arc<str>), Arc<SouthwardChannelMetricHandles>>,
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

        let devices_total = IntGauge::new(
            "southward_devices_total",
            "Total number of devices across all southward channels.",
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_devices_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(devices_total.clone()),
            "southward_devices_total",
        );

        let data_points_total = IntGauge::new(
            "southward_data_points_total",
            "Total number of data points across all devices across all southward channels.",
        )
        .map_err(|e| NGError::from(format!("Failed to create southward_data_points_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(data_points_total.clone()),
            "southward_data_points_total",
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

        let channel_connect_failed_total = IntCounterVec::new(
            opts!(
                "southward_channel_connect_failed_total",
                "Total times a southward channel entered Failed state."
            ),
            &["channel_id", "driver"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_channel_connect_failed_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channel_connect_failed_total.clone()),
            "southward_channel_connect_failed_total",
        );

        let channel_disconnect_total = IntCounterVec::new(
            opts!(
                "southward_channel_disconnect_total",
                "Total disconnect transitions from Connected for southward channel."
            ),
            &["channel_id", "driver"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_channel_disconnect_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channel_disconnect_total.clone()),
            "southward_channel_disconnect_total",
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

        let channel_report_publish_total = IntCounterVec::new(
            opts!(
                "southward_report_publish_total",
                "Total report/push publish attempts from drivers, labeled by result."
            ),
            &["channel_id", "driver", "result"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_report_publish_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channel_report_publish_total.clone()),
            "southward_report_publish_total",
        );

        let channel_bytes_total = IntCounterVec::new(
            opts!(
                "southward_channel_bytes_total",
                "Measured transport bytes for southward channels (direction=in|out)."
            ),
            &["channel_id", "driver", "direction"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create southward_channel_bytes_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(channel_bytes_total.clone()),
            "southward_channel_bytes_total",
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
            devices_total,
            data_points_total,
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
            channel_connect_failed_total,
            channel_disconnect_total,
            channel_io_total,
            channel_bytes_total,
            channel_report_publish_total,
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
        self.devices_total.set(value as i64);
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
        self.data_points_total.set(value as i64);
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
            .get(&(channel_id, Arc::<str>::from(driver)))
            .map(|h| h.snapshot_metrics())
            .unwrap_or_default()
    }

    /// Register a channel and return its pre-resolved metric handles.
    pub(crate) fn register_channel(
        &self,
        channel_id: i32,
        driver: Arc<str>,
    ) -> NGResult<Arc<SouthwardChannelMetricHandles>> {
        match self.channels.entry((channel_id, Arc::clone(&driver))) {
            Entry::Occupied(e) => Ok(Arc::clone(e.get())),
            Entry::Vacant(v) => {
                let channel_id_s = channel_id.to_string();
                let labels = [channel_id_s.as_str(), driver.as_ref()];

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
                let connect_failed_total = self
                    .channel_connect_failed_total
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_connect_failed_total for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let disconnect_total = self
                    .channel_disconnect_total
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_disconnect_total for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let success_labels = [labels[0], labels[1], "success"];
                let failed_labels = [labels[0], labels[1], "failed"];
                let timeout_labels = [labels[0], labels[1], "timeout"];
                let dropped_labels = [labels[0], labels[1], "dropped"];

                let bytes_out_labels = [labels[0], labels[1], "out"];
                let bytes_in_labels = [labels[0], labels[1], "in"];

                let bytes_sent_total = self
                    .channel_bytes_total
                    .get_metric_with_label_values(&bytes_out_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_bytes_total(out) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let bytes_received_total = self
                    .channel_bytes_total
                    .get_metric_with_label_values(&bytes_in_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get channel_bytes_total(in) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

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

                let report_publish_success_total = self
                    .channel_report_publish_total
                    .get_metric_with_label_values(&success_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get report_publish_total(success) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let report_publish_dropped_total = self
                    .channel_report_publish_total
                    .get_metric_with_label_values(&dropped_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get report_publish_total(dropped) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;
                let report_publish_fail_total = self
                    .channel_report_publish_total
                    .get_metric_with_label_values(&failed_labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get report_publish_total(fail) for channel_id={channel_id}, driver={driver}: {e}"
                        ))
                    })?;

                let handles = Arc::new(SouthwardChannelMetricHandles {
                    connected,
                    state,
                    reconnect_total,
                    connect_failed_total,
                    disconnect_total,
                    io_success_total,
                    io_failed_total,
                    bytes_sent_total,
                    bytes_received_total,
                    io_latency_success_seconds,
                    io_latency_failed_seconds,
                    collect_cycle_success_seconds,
                    collect_cycle_fail_seconds,
                    collect_cycle_timeout_seconds,
                    point_read_success_total,
                    point_read_fail_total,
                    point_read_timeout_total,
                    report_publish_success_total,
                    report_publish_dropped_total,
                    report_publish_fail_total,
                    avg_latency_ns: AtomicU64::new(0),
                    last_latency_ns: AtomicU64::new(0),
                    last_state_change_ms: AtomicU64::new(0),
                    last_report_ms: AtomicU64::new(0),
                    device_metrics: DashMap::new(),
                });
                v.insert(Arc::clone(&handles));
                Ok(handles)
            }
        }
    }

    /// Unregister a channel and best-effort remove its labeled time series.
    ///
    /// # Notes
    /// - This prevents "zombie" Prometheus series when channels are removed at runtime.
    /// - Removal is best-effort: failures are logged and ignored.
    pub(crate) fn unregister_channel(&self, channel_id: i32, driver: &str) {
        self.channels
            .remove(&(channel_id, Arc::<str>::from(driver)));

        let channel_id_s = channel_id.to_string();
        let base = [channel_id_s.as_str(), driver];

        // (channel_id, driver) series
        if let Err(e) = self.channel_connected.remove_label_values(&base) {
            warn!(
                metric_name = "southward_channel_connected",
                channel_id,
                driver,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.channel_state.remove_label_values(&base) {
            warn!(
                metric_name = "southward_channel_state",
                channel_id,
                driver,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.channel_reconnect_total.remove_label_values(&base) {
            warn!(
                metric_name = "southward_channel_reconnect_total",
                channel_id,
                driver,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.channel_connect_failed_total.remove_label_values(&base) {
            warn!(
                metric_name = "southward_channel_connect_failed_total",
                channel_id,
                driver,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.channel_disconnect_total.remove_label_values(&base) {
            warn!(
                metric_name = "southward_channel_disconnect_total",
                channel_id,
                driver,
                error = %e,
                "Failed to remove labeled series"
            );
        }

        // (channel_id, driver, direction)
        for direction in ["in", "out"] {
            let labels = [base[0], base[1], direction];
            if let Err(e) = self.channel_bytes_total.remove_label_values(&labels) {
                warn!(
                    metric_name = "southward_channel_bytes_total",
                    channel_id,
                    driver,
                    direction,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
        }

        // (channel_id, driver, result)
        for result in ["success", "failed", "timeout", "dropped"] {
            let labels = [base[0], base[1], result];
            if let Err(e) = self.channel_io_total.remove_label_values(&labels) {
                warn!(
                    metric_name = "southward_io_total",
                    channel_id,
                    driver,
                    result,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
            if let Err(e) = self
                .channel_report_publish_total
                .remove_label_values(&labels)
            {
                warn!(
                    metric_name = "southward_report_publish_total",
                    channel_id,
                    driver,
                    result,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
            if let Err(e) = self
                .channel_collect_cycle_seconds
                .remove_label_values(&labels)
            {
                warn!(
                    metric_name = "southward_collect_cycle_seconds",
                    channel_id,
                    driver,
                    result,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
            if let Err(e) = self.channel_point_read_total.remove_label_values(&labels) {
                warn!(
                    metric_name = "southward_point_read_total",
                    channel_id,
                    driver,
                    result,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
            if let Err(e) = self.channel_io_latency_seconds.remove_label_values(&labels) {
                warn!(
                    metric_name = "southward_io_latency_seconds",
                    channel_id,
                    driver,
                    result,
                    error = %e,
                    "Failed to remove labeled series"
                );
            }
        }
    }
}
