//! Prometheus metrics for NG Gateway.
//!
//! This module provides a **single, process-wide** Prometheus `Registry` and a
//! small set of core metrics required by the observability plan.
//!
//! # Design goals
//! - **Low overhead**: update metrics on scrape (pull model).
//! - **Low cardinality by default**: do not put device/point identifiers into labels.
//! - **One registry**: all crates register metrics into the same registry.

pub mod channel;
pub mod collector;
pub mod control;
pub mod northward;
pub mod queue;
pub mod southward;
mod system;

use self::{
    collector::{CollectorMetricsHub, CollectorResult},
    control::{ControlChannelMetricHandles, ControlMetricsHub},
    northward::{NorthwardAppMetricHandles, NorthwardMetricsHub},
    queue::QueueMetricsHub,
    southward::{SouthwardChannelMetricHandles, SouthwardMetricsHub},
    system::SystemMetrics,
};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{
    core::metrics::{
        AppActorState, ChannelMetricsSnapshot, ChannelStatsSnapshot, CollectorMetricsSnapshot,
        ControlMetricsSnapshot, GatewayMetricsSnapshot, GatewayStatusSnapshot,
        NorthwardAppMetricsSnapshot, NorthwardAppStatsSnapshot, NorthwardManagerMetricsSnapshot,
        SouthwardManagerMetricsSnapshot,
    },
    enums::core::GatewayState,
    web::PrometheusTextPayload,
};
use ng_gateway_sdk::{HealthStatus, SouthwardConnectionState};
use prometheus::{Encoder, Registry, TextEncoder};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

/// Parameters for building a southward channel snapshot.
#[derive(Debug)]
pub struct SouthwardChannelSnapshotParams {
    pub channel_id: i32,
    pub name: String,
    pub driver_name: String,
    pub state: SouthwardConnectionState,
    pub health: Option<HealthStatus>,
    pub device_count: usize,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

/// Global metrics hub for the gateway process.
///
/// # Design constraints
/// - The hub is an explicit, stateful object (no `Lazy` singleton).
/// - The hub must be initialized once at startup (`init_global_metrics_hub`).
/// - Internal submodules (system/queue) are held as members to ensure a single
///   authoritative data source.
#[derive(Debug)]
pub struct NGMetricsHub {
    registry: Registry,
    system: SystemMetrics,
    queue: QueueMetricsHub,
    northward: NorthwardMetricsHub,
    southward: SouthwardMetricsHub,
    collector: CollectorMetricsHub,
    control: ControlMetricsHub,

    // Snapshot-only: gateway-level derived error rate state (best-effort).
    last_error_rate_total_errors: AtomicU64,
    last_error_rate_ts_ms: AtomicU64,
}

impl NGMetricsHub {
    /// Create a new metrics hub instance.
    ///
    /// # Notes
    /// - This registers all built-in metrics into the hub registry.
    /// - This function does not mutate any global state.
    pub fn new() -> NGResult<Self> {
        let registry = Registry::new_custom(Some("ng_gateway".to_string()), None).map_err(|e| {
            NGError::from(format!(
                "Failed to create Prometheus registry with namespace 'ng_gateway': {e}"
            ))
        })?;

        let system = SystemMetrics::new(&registry)?;
        let queue = QueueMetricsHub::new(&registry)?;
        let northward = NorthwardMetricsHub::new(&registry)?;
        let southward = SouthwardMetricsHub::new(&registry)?;
        let collector = CollectorMetricsHub::new(&registry)?;
        let control = ControlMetricsHub::new(&registry)?;

        Ok(Self {
            registry,
            system,
            queue,
            northward,
            southward,
            collector,
            control,
            last_error_rate_total_errors: AtomicU64::new(0),
            last_error_rate_ts_ms: AtomicU64::new(0),
        })
    }

    /// Expose the Prometheus registry so other crates can register their metrics.
    #[inline]
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Access queue metrics hub (instrumented queues backend).
    #[inline]
    pub(crate) fn queue(&self) -> &QueueMetricsHub {
        &self.queue
    }

    /// Refresh scrape-time metrics to keep the exposition accurate.
    #[inline]
    pub fn refresh_for_scrape(&self) {
        self.system.refresh();
        self.queue.refresh_all_queue_depths();
    }

    /// Register northward app metrics and return pre-resolved handles.
    ///
    /// # Notes
    /// This is used by the northward subsystem to avoid label lookups on hot paths.
    pub fn register_northward_app_metrics(
        &self,
        app_id: i32,
        plugin_id: i32,
    ) -> NGResult<Arc<NorthwardAppMetricHandles>> {
        self.northward.register_app(app_id, plugin_id)
    }

    /// Unregister northward app metrics and remove labeled Prometheus series (best-effort).
    ///
    /// This should be called when an app is removed at runtime to avoid "zombie" series.
    #[inline]
    pub fn unregister_northward_app_metrics(&self, app_id: i32, plugin_id: i32) {
        self.northward.unregister_app(app_id, plugin_id);
    }

    /// Set total apps count for northward manager (scrape-time gauge).
    #[inline]
    pub fn set_northward_apps_total(&self, value: u64) {
        self.northward.set_apps_total(value);
    }

    /// Set active apps count for northward manager (scrape-time gauge).
    #[inline]
    pub fn set_northward_apps_active(&self, value: u64) {
        self.northward.set_apps_active(value);
    }

    /// Increment collector retries counter (timeout-triggered).
    #[inline]
    pub fn inc_collector_retries_timeout(&self) {
        self.collector.inc_retries_timeout();
    }

    /// Increment collector retries counter (error-triggered).
    #[inline]
    pub fn inc_collector_retries_error(&self) {
        self.collector.inc_retries_error();
    }

    /// Increment events received counter for northward manager.
    #[inline]
    pub fn inc_northward_events_received(&self) {
        self.northward.inc_events_received();
    }

    /// Increment data routed counter for northward manager.
    #[inline]
    pub fn inc_northward_data_routed(&self) {
        self.northward.inc_data_routed();
    }

    /// Increment routing errors counter for northward manager.
    #[inline]
    pub fn inc_northward_routing_errors(&self) {
        self.northward.inc_routing_errors();
    }

    /// Snapshot northward manager metrics for REST/WS consumers.
    #[inline]
    pub fn snapshot_northward_manager(&self) -> NorthwardManagerMetricsSnapshot {
        self.northward.snapshot_manager()
    }

    /// Snapshot northward app metrics for REST/WS consumers.
    #[inline]
    pub fn snapshot_northward_app_metrics(
        &self,
        app_id: i32,
        plugin_id: i32,
    ) -> Option<NorthwardAppMetricsSnapshot> {
        self.northward.snapshot_app_metrics(app_id, plugin_id)
    }

    /// Build a northward app stats snapshot, sourcing metrics from the hub (single source of truth).
    #[inline]
    pub fn build_northward_app_snapshot(
        &self,
        app_id: i32,
        plugin_id: i32,
        name: String,
        state: AppActorState,
        is_connected: bool,
    ) -> NorthwardAppStatsSnapshot {
        let metrics = self
            .snapshot_northward_app_metrics(app_id, plugin_id)
            .unwrap_or(NorthwardAppMetricsSnapshot {
                messages_sent: 0,
                messages_dropped: 0,
                errors: 0,
                retries: 0,
                last_sent: None,
                last_error: None,
                avg_latency_ms: 0.0,
            });

        NorthwardAppStatsSnapshot {
            app_id,
            plugin_id,
            name,
            state,
            is_connected,
            metrics,
        }
    }

    /// Register southward channel metrics and return pre-resolved handles.
    ///
    /// # Notes
    /// This is used by southward monitors to avoid label lookups on hot paths.
    pub fn register_southward_channel_metrics(
        &self,
        channel_id: i32,
        driver: String,
    ) -> NGResult<Arc<SouthwardChannelMetricHandles>> {
        self.southward.register_channel(channel_id, driver)
    }

    /// Unregister southward channel metrics and remove labeled Prometheus series (best-effort).
    ///
    /// This should be called when a channel is removed at runtime to avoid "zombie" series.
    #[inline]
    pub fn unregister_southward_channel_metrics(&self, channel_id: i32, driver: &str) {
        self.southward.unregister_channel(channel_id, driver);
    }

    /// Register control-plane metrics for a channel and return pre-resolved handles.
    ///
    /// # Notes
    /// This is used by control-plane write paths to avoid label lookups on hot paths.
    pub fn register_control_channel_metrics(
        &self,
        channel_id: i32,
        driver: String,
    ) -> NGResult<Arc<ControlChannelMetricHandles>> {
        self.control.register_channel(channel_id, driver)
    }

    /// Unregister control-plane channel metrics and remove labeled Prometheus series (best-effort).
    ///
    /// This should be called when a channel is removed at runtime to avoid "zombie" series.
    #[inline]
    pub fn unregister_control_channel_metrics(&self, channel_id: i32, driver: &str) {
        self.control.unregister_channel(channel_id, driver);
    }

    /// Snapshot control-plane metrics for a channel (if registered).
    #[inline]
    pub fn snapshot_control_channel_metrics(
        &self,
        channel_id: i32,
        driver: &str,
    ) -> Option<ControlMetricsSnapshot> {
        self.control.snapshot_channel(channel_id, driver)
    }

    /// Set total channels gauge for southward subsystem.
    #[inline]
    pub fn set_southward_total_channels(&self, value: u64) {
        self.southward.set_total_channels(value);
    }

    /// Set connected channels gauge for southward subsystem.
    #[inline]
    pub fn set_southward_connected_channels(&self, value: u64) {
        self.southward.set_connected_channels(value);
    }

    /// Increment connected channels for southward subsystem (transition-based).
    #[inline]
    pub fn inc_southward_connected_channels(&self) {
        self.southward.inc_connected_channels();
    }

    /// Decrement connected channels for southward subsystem (transition-based).
    #[inline]
    pub fn dec_southward_connected_channels(&self) {
        self.southward.dec_connected_channels();
    }

    /// Set southward total devices for manager snapshot.
    #[inline]
    pub fn set_southward_total_devices(&self, value: u64) {
        self.southward.set_total_devices(value);
    }

    /// Set southward active devices for manager snapshot.
    #[inline]
    pub fn set_southward_active_devices(&self, value: u64) {
        self.southward.set_active_devices(value);
    }

    /// Set southward total data points for manager snapshot.
    #[inline]
    pub fn set_southward_total_data_points(&self, value: u64) {
        self.southward.set_total_data_points(value);
    }

    /// Set southward total actions for manager snapshot.
    #[inline]
    pub fn set_southward_total_actions(&self, value: u64) {
        self.southward.set_total_actions(value);
    }

    /// Snapshot southward manager metrics for REST/WS consumers.
    #[inline]
    pub fn snapshot_southward_manager(&self) -> SouthwardManagerMetricsSnapshot {
        self.southward.snapshot_manager()
    }

    /// Snapshot southward channel metrics for REST/WS consumers.
    #[inline]
    pub fn snapshot_southward_channel_metrics(
        &self,
        channel_id: i32,
        driver: &str,
    ) -> ChannelMetricsSnapshot {
        self.southward.snapshot_channel_metrics(channel_id, driver)
    }

    /// Build a southward channel stats snapshot, sourcing metrics from the hub (single source of truth).
    #[inline]
    pub fn build_southward_channel_snapshot(
        &self,
        params: SouthwardChannelSnapshotParams,
    ) -> ChannelStatsSnapshot {
        let metrics =
            self.snapshot_southward_channel_metrics(params.channel_id, params.driver_name.as_str());
        let control_metrics =
            self.snapshot_control_channel_metrics(params.channel_id, params.driver_name.as_str());
        ChannelStatsSnapshot {
            channel_id: params.channel_id,
            name: params.name,
            driver_name: params.driver_name,
            state: params.state,
            health: params.health,
            device_count: params.device_count,
            metrics,
            control_metrics,
            created_at: params.created_at,
            last_activity: params.last_activity,
        }
    }

    /// Record one collector cycle (per group call).
    #[inline]
    pub fn record_collector_cycle_success(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.collector
            .record_cycle(CollectorResult::Success, elapsed_ns, elapsed_seconds);
    }

    /// Record one failed collector cycle (per group call).
    #[inline]
    pub fn record_collector_cycle_fail(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.collector
            .record_cycle(CollectorResult::Fail, elapsed_ns, elapsed_seconds);
    }

    /// Record one timed-out collector cycle (per group call).
    #[inline]
    pub fn record_collector_cycle_timeout(&self, elapsed_ns: u64, elapsed_seconds: f64) {
        self.collector
            .record_cycle(CollectorResult::Timeout, elapsed_ns, elapsed_seconds);
    }

    /// Set active channel collection tasks gauge (collector).
    #[inline]
    pub fn set_collector_active_tasks(&self, value: u64) {
        self.collector.set_active_tasks(value);
    }

    /// Set collector concurrency permits gauges.
    #[inline]
    pub fn set_collector_concurrency_permits(&self, current: u64, available: u64) {
        self.collector.set_concurrency_permits(current, available);
    }

    /// Snapshot collector metrics for REST/WS consumers.
    #[inline]
    pub fn snapshot_collector(&self) -> CollectorMetricsSnapshot {
        self.collector.snapshot()
    }

    /// Build a gateway status snapshot, sourcing all metrics from the hub (single source of truth).
    ///
    /// # Notes
    /// - `system_info` is sourced from `SystemMetrics` (sysinfo-backed).
    /// - Gateway-level `metrics` is derived from hub-owned subsystem snapshots to avoid drift.
    #[inline]
    pub fn build_gateway_snapshot(
        &self,
        state: GatewayState,
        version: String,
        uptime: chrono::Duration,
    ) -> GatewayStatusSnapshot {
        let now = chrono::Utc::now();
        let now_ms = now.timestamp_millis().max(0) as u64;
        let system_info = self.system.snapshot_system_info();
        let (network_bytes_sent, network_bytes_received) = self.system.snapshot_network_bytes();
        let southward_metrics = self.snapshot_southward_manager();
        let northward_metrics = self.snapshot_northward_manager();
        let collector_metrics = self.snapshot_collector();
        let process_rss_bytes = self.system.snapshot_process_rss_bytes();

        // Gateway error aggregation (best-effort).
        //
        // Definition (current):
        // - Collector errors: failed + timeout collections
        // - Northward routing errors: manager routing_errors
        //
        // NOTE: This is intentionally low-cost and does not scan per-channel/app series.
        let total_errors = collector_metrics.failed_collections
            + collector_metrics.timeout_collections
            + northward_metrics.routing_errors;

        // Derive a best-effort error rate (errors/min) from successive snapshots.
        let last_ms = self.last_error_rate_ts_ms.swap(now_ms, Ordering::Relaxed);
        let last_total = self
            .last_error_rate_total_errors
            .swap(total_errors, Ordering::Relaxed);
        let error_rate = if last_ms == 0 || now_ms <= last_ms {
            0.0
        } else {
            let dt_sec = (now_ms - last_ms) as f64 / 1000.0;
            if dt_sec <= 0.0 {
                0.0
            } else {
                let delta = total_errors.saturating_sub(last_total) as f64;
                (delta / dt_sec) * 60.0
            }
        };

        let metrics = GatewayMetricsSnapshot {
            uptime,
            total_channels: southward_metrics.total_channels,
            connected_channels: southward_metrics.connected_channels,
            total_devices: southward_metrics.total_devices,
            active_devices: southward_metrics.active_devices,
            total_data_points: southward_metrics.total_data_points,
            total_collections: collector_metrics.total_collections,
            successful_collections: collector_metrics.successful_collections,
            failed_collections: collector_metrics.failed_collections,
            timeout_collections: collector_metrics.timeout_collections,
            average_collection_time_ms: collector_metrics.average_collection_time_ms,
            active_tasks: collector_metrics.active_tasks,
            // Process RSS memory in bytes (best-effort).
            memory_usage: process_rss_bytes,
            cpu_usage: system_info.cpu_usage_percent,
            network_bytes_sent,
            network_bytes_received,
            total_errors,
            error_rate,
            last_update: Some(now),
        };

        GatewayStatusSnapshot {
            state,
            metrics,
            southward_metrics,
            northward_metrics,
            collector_metrics,
            version,
            system_info,
        }
    }

    /// Gather all metrics in Prometheus text exposition format.
    ///
    /// # Notes
    /// - System metrics are refreshed right before encoding.
    /// - Queue depth gauges are refreshed right before encoding.
    /// - The caller is expected to expose the returned payload at `GET /metrics`.
    #[inline]
    pub fn gather_prometheus_text(&self) -> NGResult<PrometheusTextPayload> {
        self.refresh_for_scrape();

        let metric_families = self.registry.gather();

        let encoder = TextEncoder::new();
        let mut buffer = Vec::with_capacity(8 * 1024);
        encoder.encode(&metric_families, &mut buffer).map_err(|e| {
            NGError::from(format!("Failed to encode Prometheus metric families: {e}"))
        })?;

        Ok(PrometheusTextPayload {
            content_type: encoder.format_type().to_string(),
            body: buffer,
        })
    }
}
