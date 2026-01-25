//! Northward (app/plugin) metrics.
//!
//! This module provides low-cardinality Prometheus metrics for the northward subsystem,
//! aggregated by `(app_id, plugin_id)`.
//!
//! # Cardinality rules
//! - Labels must remain bounded: `app_id`, `plugin_id`, `direction`, `result`.
//! - Do NOT include device_id / point_id / error_message / topic in labels.

use chrono::{DateTime, Utc};
use dashmap::{mapref::entry::Entry, DashMap};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::core::metrics::{
    NorthwardAppMetricsSnapshot, NorthwardManagerMetricsSnapshot,
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

/// A bounded direction label for northward message metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NorthwardDirection {
    /// Data flowing from gateway to plugin (device -> app -> plugin).
    Uplink,
    /// Data flowing from plugin/platform to gateway (control/commands).
    Downlink,
}

impl NorthwardDirection {
    /// Convert to Prometheus `direction` label value.
    #[inline]
    pub const fn as_label(self) -> &'static str {
        match self {
            NorthwardDirection::Uplink => "uplink",
            NorthwardDirection::Downlink => "downlink",
        }
    }
}

/// A bounded result label for northward message metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NorthwardResult {
    /// Operation succeeded.
    Success,
    /// Operation failed.
    Fail,
    /// Operation dropped (queue full / policy / disconnected without buffer, etc.).
    Dropped,
}

impl NorthwardResult {
    /// Convert to Prometheus `result` label value.
    #[inline]
    pub const fn as_label(self) -> &'static str {
        match self {
            NorthwardResult::Success => "success",
            NorthwardResult::Fail => "fail",
            NorthwardResult::Dropped => "dropped",
        }
    }
}

/// Pre-resolved metric handles for one `(app_id, plugin_id)` pair.
///
/// This avoids label lookups on hot paths.
#[derive(Debug)]
pub struct NorthwardAppMetricHandles {
    connected: IntGauge,
    state: IntGauge,
    reconnect_total: IntCounter,
    messages_total: [[IntCounter; 3]; 2],
    message_latency_seconds: [[Histogram; 3]; 2],
    snapshot_state: Arc<AppSnapshotState>,
}

impl NorthwardAppMetricHandles {
    /// Set app connected gauge (0/1).
    #[inline]
    pub fn set_connected(&self, connected: bool) {
        self.connected.set(if connected { 1 } else { 0 });
    }

    /// Set app state gauge as enum value.
    #[inline]
    pub fn set_state(&self, state_value: i64) {
        self.state.set(state_value);
    }

    /// Increment reconnect counter.
    #[inline]
    pub fn inc_reconnect(&self) {
        self.reconnect_total.inc();
    }

    /// Increment messages counter for the given direction/result.
    #[inline]
    pub fn inc_message(&self, direction: NorthwardDirection, result: NorthwardResult) {
        self.messages_total[dir_idx(direction)][res_idx(result)].inc();
    }

    /// Observe message latency seconds for the given direction/result.
    #[inline]
    pub fn observe_latency_seconds(
        &self,
        direction: NorthwardDirection,
        result: NorthwardResult,
        seconds: f64,
    ) {
        self.message_latency_seconds[dir_idx(direction)][res_idx(result)].observe(seconds);
    }

    /// Record a successfully processed uplink message with latency.
    #[inline]
    pub fn record_uplink_success(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.inc_message(NorthwardDirection::Uplink, NorthwardResult::Success);
        self.observe_latency_seconds(
            NorthwardDirection::Uplink,
            NorthwardResult::Success,
            elapsed_seconds,
        );
        self.snapshot_state.record_sent(elapsed_ns);
    }

    /// Record a failed uplink message with latency.
    #[inline]
    pub fn record_uplink_fail(&self, elapsed_seconds: f64) {
        self.inc_message(NorthwardDirection::Uplink, NorthwardResult::Fail);
        self.observe_latency_seconds(
            NorthwardDirection::Uplink,
            NorthwardResult::Fail,
            elapsed_seconds,
        );
        self.snapshot_state.record_error();
    }

    /// Record a dropped uplink message.
    #[inline]
    pub fn record_uplink_dropped(&self) {
        self.inc_message(NorthwardDirection::Uplink, NorthwardResult::Dropped);
        self.snapshot_state.record_dropped();
    }

    /// Record a non-message error event (e.g. connection failure).
    ///
    /// # Notes
    /// This updates snapshot diagnostics (last_error timestamp) without changing message counters.
    #[inline]
    pub fn record_error_event(&self) {
        self.snapshot_state.record_error();
    }

    /// Build a snapshot DTO for REST/WS consumers.
    ///
    /// # Notes
    /// - Snapshot is derived from Prometheus counters (for counts) and hub-owned state
    ///   for timestamps/avg latency.
    /// - Snapshot is **uplink-only** for now.
    pub fn snapshot(&self) -> NorthwardAppMetricsSnapshot {
        let sent = self.messages_total[dir_idx(NorthwardDirection::Uplink)]
            [res_idx(NorthwardResult::Success)]
        .get() as u64;
        let dropped = self.messages_total[dir_idx(NorthwardDirection::Uplink)]
            [res_idx(NorthwardResult::Dropped)]
        .get() as u64;
        let errors = self.messages_total[dir_idx(NorthwardDirection::Uplink)]
            [res_idx(NorthwardResult::Fail)]
        .get() as u64;

        let (last_sent, last_error, avg_latency_ms) = self.snapshot_state.snapshot();

        NorthwardAppMetricsSnapshot {
            messages_sent: sent,
            messages_dropped: dropped,
            errors,
            // Map snapshot retries to the authoritative reconnect counter.
            retries: self.reconnect_total.get(),
            last_sent,
            last_error,
            avg_latency_ms,
        }
    }
}

#[inline]
const fn dir_idx(d: NorthwardDirection) -> usize {
    match d {
        NorthwardDirection::Uplink => 0,
        NorthwardDirection::Downlink => 1,
    }
}

#[inline]
const fn res_idx(r: NorthwardResult) -> usize {
    match r {
        NorthwardResult::Success => 0,
        NorthwardResult::Fail => 1,
        NorthwardResult::Dropped => 2,
    }
}

#[inline]
fn register_collector_into(registry: &Registry, collector: Box<dyn Collector>, name: &'static str) {
    if let Err(e) = registry.register(collector) {
        // Duplicate registration should not crash the gateway.
        warn!(metric_name = name, error = %e, "Failed to register Prometheus collector");
    }
}

/// Snapshot-only runtime state for a northward app.
///
/// This state is part of the **single source of truth** inside `NGMetricsHub`.
#[derive(Debug, Default)]
struct AppSnapshotState {
    last_sent: RwLock<Option<DateTime<Utc>>>,
    last_error: RwLock<Option<DateTime<Utc>>>,
    avg_latency_ns: AtomicU64,
    dropped_total: AtomicU64,
}

impl AppSnapshotState {
    #[inline]
    fn record_sent(&self, elapsed_ns: u64) {
        if let Ok(mut g) = self.last_sent.write() {
            *g = Some(Utc::now());
        }

        let old = self.avg_latency_ns.load(Ordering::Relaxed);
        let new = if old == 0 {
            elapsed_ns
        } else {
            (old * 9 + elapsed_ns) / 10
        };
        self.avg_latency_ns.store(new, Ordering::Relaxed);
    }

    #[inline]
    fn record_error(&self) {
        if let Ok(mut g) = self.last_error.write() {
            *g = Some(Utc::now());
        }
    }

    #[inline]
    fn record_dropped(&self) {
        self.dropped_total.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>, f64) {
        let last_sent = self.last_sent.read().ok().and_then(|g| *g);
        let last_error = self.last_error.read().ok().and_then(|g| *g);
        let avg_latency_ms = self.avg_latency_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        (last_sent, last_error, avg_latency_ms)
    }
}

/// Snapshot-only runtime state for northward manager metrics.
#[derive(Debug, Default)]
struct ManagerSnapshotState {
    last_update: RwLock<Option<DateTime<Utc>>>,
}

impl ManagerSnapshotState {
    #[inline]
    fn touch(&self) {
        if let Ok(mut g) = self.last_update.write() {
            *g = Some(Utc::now());
        }
    }

    #[inline]
    fn last_update(&self) -> Option<DateTime<Utc>> {
        self.last_update.read().ok().and_then(|g| *g)
    }
}

/// Northward metrics owned by `NGMetricsHub`.
#[derive(Debug)]
pub(crate) struct NorthwardMetricsHub {
    // --- manager-level metrics (no labels) ---
    apps_total: IntGauge,
    apps_active: IntGauge,
    events_received_total: IntCounter,
    data_routed_total: IntCounter,
    routing_errors_total: IntCounter,
    manager_state: ManagerSnapshotState,

    // --- per-app metrics (labels: app_id, plugin_id) ---
    connected: IntGaugeVec,
    state: IntGaugeVec,
    reconnect_total: IntCounterVec,
    messages_total: IntCounterVec,
    message_latency_seconds: HistogramVec,
    apps: DashMap<(i32, i32), Arc<NorthwardAppMetricHandles>>,
}

impl NorthwardMetricsHub {
    /// Create and register northward metrics into the given registry.
    pub(crate) fn new(registry: &Registry) -> NGResult<Self> {
        let apps_total = IntGauge::new(
            "northward_apps_total",
            "Total number of northward apps (enabled + disabled).",
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_apps_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(apps_total.clone()),
            "northward_apps_total",
        );

        let apps_active = IntGauge::new(
            "northward_apps_active",
            "Number of currently active (running) northward apps.",
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_apps_active: {e}")))?;
        register_collector_into(
            registry,
            Box::new(apps_active.clone()),
            "northward_apps_active",
        );

        let events_received_total = IntCounter::new(
            "northward_events_received_total",
            "Total events received from all northward apps.",
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create northward_events_received_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(events_received_total.clone()),
            "northward_events_received_total",
        );

        let data_routed_total = IntCounter::new(
            "northward_data_routed_total",
            "Total data items routed to northward apps.",
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_data_routed_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(data_routed_total.clone()),
            "northward_data_routed_total",
        );

        let routing_errors_total = IntCounter::new(
            "northward_routing_errors_total",
            "Total routing errors in northward subsystem.",
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create northward_routing_errors_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(routing_errors_total.clone()),
            "northward_routing_errors_total",
        );

        let connected = IntGaugeVec::new(
            opts!(
                "northward_app_connected",
                "Northward app connected state (1=connected, 0=not connected)."
            ),
            &["app_id", "plugin_id"],
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_app_connected: {e}")))?;
        register_collector_into(
            registry,
            Box::new(connected.clone()),
            "northward_app_connected",
        );

        let state = IntGaugeVec::new(
            opts!(
                "northward_app_state",
                "Northward app runtime state (enum value)."
            ),
            &["app_id", "plugin_id"],
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_app_state: {e}")))?;
        register_collector_into(registry, Box::new(state.clone()), "northward_app_state");

        let reconnect_total = IntCounterVec::new(
            opts!(
                "northward_app_reconnect_total",
                "Total reconnect attempts for northward app connection."
            ),
            &["app_id", "plugin_id"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create northward_app_reconnect_total: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(reconnect_total.clone()),
            "northward_app_reconnect_total",
        );

        let messages_total = IntCounterVec::new(
            opts!(
                "northward_messages_total",
                "Total northward messages processed by app."
            ),
            &["app_id", "plugin_id", "direction", "result"],
        )
        .map_err(|e| NGError::from(format!("Failed to create northward_messages_total: {e}")))?;
        register_collector_into(
            registry,
            Box::new(messages_total.clone()),
            "northward_messages_total",
        );

        let latency_opts = HistogramOpts::new(
            "northward_message_latency_seconds",
            "Northward plugin processing latency in seconds.",
        )
        .buckets(vec![
            0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0,
        ]);
        let message_latency_seconds = HistogramVec::new(
            latency_opts,
            &["app_id", "plugin_id", "direction", "result"],
        )
        .map_err(|e| {
            NGError::from(format!(
                "Failed to create northward_message_latency_seconds: {e}"
            ))
        })?;
        register_collector_into(
            registry,
            Box::new(message_latency_seconds.clone()),
            "northward_message_latency_seconds",
        );

        Ok(Self {
            apps_total,
            apps_active,
            events_received_total,
            data_routed_total,
            routing_errors_total,
            manager_state: ManagerSnapshotState::default(),
            connected,
            state,
            reconnect_total,
            messages_total,
            message_latency_seconds,
            apps: DashMap::new(),
        })
    }

    /// Update total apps gauge.
    #[inline]
    pub(crate) fn set_apps_total(&self, value: u64) {
        self.apps_total.set(value as i64);
        self.manager_state.touch();
    }

    /// Update active apps gauge.
    #[inline]
    pub(crate) fn set_apps_active(&self, value: u64) {
        self.apps_active.set(value as i64);
        self.manager_state.touch();
    }

    /// Increment events received counter.
    #[inline]
    pub(crate) fn inc_events_received(&self) {
        self.events_received_total.inc();
        self.manager_state.touch();
    }

    /// Increment data routed counter.
    #[inline]
    pub(crate) fn inc_data_routed(&self) {
        self.data_routed_total.inc();
        self.manager_state.touch();
    }

    /// Increment routing errors counter.
    #[inline]
    pub(crate) fn inc_routing_errors(&self) {
        self.routing_errors_total.inc();
        self.manager_state.touch();
    }

    /// Snapshot manager-level metrics for REST/WS.
    pub(crate) fn snapshot_manager(&self) -> NorthwardManagerMetricsSnapshot {
        NorthwardManagerMetricsSnapshot {
            total_apps: self.apps_total.get() as u64,
            active_apps: self.apps_active.get() as u64,
            total_events_received: self.events_received_total.get(),
            total_data_routed: self.data_routed_total.get(),
            routing_errors: self.routing_errors_total.get(),
            last_update: self.manager_state.last_update(),
        }
    }

    /// Snapshot a northward app metrics DTO, if it is registered.
    #[inline]
    pub(crate) fn snapshot_app_metrics(
        &self,
        app_id: i32,
        plugin_id: i32,
    ) -> Option<NorthwardAppMetricsSnapshot> {
        self.apps.get(&(app_id, plugin_id)).map(|h| h.snapshot())
    }

    /// Register an app and return its pre-resolved metric handles.
    pub(crate) fn register_app(
        &self,
        app_id: i32,
        plugin_id: i32,
    ) -> NGResult<Arc<NorthwardAppMetricHandles>> {
        match self.apps.entry((app_id, plugin_id)) {
            Entry::Occupied(e) => Ok(Arc::clone(e.get())),
            Entry::Vacant(v) => {
                let app_id_s = app_id.to_string();
                let plugin_id_s = plugin_id.to_string();
                let labels = [app_id_s.as_str(), plugin_id_s.as_str()];

                let connected = self.connected.get_metric_with_label_values(&labels).map_err(|e| {
                    NGError::from(format!(
                        "Failed to get connected gauge for app_id={app_id}, plugin_id={plugin_id}: {e}"
                    ))
                })?;
                let state = self
                    .state
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                        "Failed to get state gauge for app_id={app_id}, plugin_id={plugin_id}: {e}"
                    ))
                    })?;
                let reconnect_total = self
                    .reconnect_total
                    .get_metric_with_label_values(&labels)
                    .map_err(|e| {
                        NGError::from(format!(
                            "Failed to get reconnect counter for app_id={app_id}, plugin_id={plugin_id}: {e}"
                        ))
                    })?;

                // Pre-resolve counters/histograms for (direction,result) combinations.
                let messages_total = [
                    [
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Success.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Fail.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Dropped.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                    ],
                    [
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Success.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Fail.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                        self.messages_total
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Dropped.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get messages_total: {e}"))
                            })?,
                    ],
                ];

                let message_latency_seconds = [
                    [
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Success.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Fail.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Uplink.as_label(),
                                NorthwardResult::Dropped.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                    ],
                    [
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Success.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Fail.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                        self.message_latency_seconds
                            .get_metric_with_label_values(&[
                                labels[0],
                                labels[1],
                                NorthwardDirection::Downlink.as_label(),
                                NorthwardResult::Dropped.as_label(),
                            ])
                            .map_err(|e| {
                                NGError::from(format!("Failed to get latency histogram: {e}"))
                            })?,
                    ],
                ];

                let handles = Arc::new(NorthwardAppMetricHandles {
                    connected,
                    state,
                    reconnect_total,
                    messages_total,
                    message_latency_seconds,
                    snapshot_state: Arc::new(AppSnapshotState::default()),
                });

                v.insert(Arc::clone(&handles));
                Ok(handles)
            }
        }
    }

    /// Unregister an app and best-effort remove its labeled time series.
    ///
    /// # Notes
    /// - This prevents "zombie" Prometheus series when apps are removed at runtime.
    /// - Removal is best-effort: failures are logged and ignored.
    pub(crate) fn unregister_app(&self, app_id: i32, plugin_id: i32) {
        // Drop handle state first (releases snapshot state).
        self.apps.remove(&(app_id, plugin_id));

        let app_id_s = app_id.to_string();
        let plugin_id_s = plugin_id.to_string();
        let base = [app_id_s.as_str(), plugin_id_s.as_str()];

        if let Err(e) = self.connected.remove_label_values(&base) {
            warn!(
                metric_name = "northward_app_connected",
                app_id,
                plugin_id,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.state.remove_label_values(&base) {
            warn!(
                metric_name = "northward_app_state",
                app_id,
                plugin_id,
                error = %e,
                "Failed to remove labeled series"
            );
        }
        if let Err(e) = self.reconnect_total.remove_label_values(&base) {
            warn!(
                metric_name = "northward_app_reconnect_total",
                app_id,
                plugin_id,
                error = %e,
                "Failed to remove labeled series"
            );
        }

        for dir in [NorthwardDirection::Uplink, NorthwardDirection::Downlink] {
            for res in [
                NorthwardResult::Success,
                NorthwardResult::Fail,
                NorthwardResult::Dropped,
            ] {
                let labels = [base[0], base[1], dir.as_label(), res.as_label()];
                if let Err(e) = self.messages_total.remove_label_values(&labels) {
                    warn!(
                        metric_name = "northward_messages_total",
                        app_id,
                        plugin_id,
                        direction = dir.as_label(),
                        result = res.as_label(),
                        error = %e,
                        "Failed to remove labeled series"
                    );
                }
                if let Err(e) = self.message_latency_seconds.remove_label_values(&labels) {
                    warn!(
                        metric_name = "northward_message_latency_seconds",
                        app_id,
                        plugin_id,
                        direction = dir.as_label(),
                        result = res.as_label(),
                        error = %e,
                        "Failed to remove labeled series"
                    );
                }
            }
        }
    }
}
