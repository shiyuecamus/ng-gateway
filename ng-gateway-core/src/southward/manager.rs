// Re-export types for historical imports:
// `crate::southward::manager::*` remains valid after refactor.
pub use super::{
    ChannelInitEntry, ChannelInstance, ConnectedDeviceSnapshot, DeviceBasicSnapshot,
    DeviceDataSnapshot, DeviceDriverSnapshot, DeviceInitTriple, DeviceInstance, NGSouthwardManager,
    SubscriptionFilter,
};

use super::{
    super::southward::{index::RuntimeIndex, SouthwardRegistry},
    SnapshotGcRuntime, SouthwardRuntime,
};
use dashmap::DashMap;
use ng_gateway_common::metrics::{
    southward::SouthwardChannelMetricHandles, NGMetricsHub, SouthwardChannelSnapshotParams,
};
use ng_gateway_models::{
    core::metrics::{ChannelStatsSnapshot, SouthwardManagerMetricsSnapshot},
    settings::Southward,
    AiEngineApi, SouthwardManager,
};
use ng_gateway_sdk::{ConnectionState, DeviceState, RuntimeAction, RuntimeDelta, RuntimePoint};
use std::sync::{atomic::AtomicUsize, Arc};
use tokio_util::sync::CancellationToken;

impl NGSouthwardManager {
    #[inline]
    /// Create a new data manager.
    ///
    /// `ai_engine` is an optional handle to the AI Processing Engine. When
    /// present, it is automatically injected into every driver's init context
    /// so camera drivers can submit frames for AI analysis.
    pub fn new(
        southward_registry: SouthwardRegistry,
        metrics_hub: Arc<NGMetricsHub>,
        snapshot_gc_cfg: Arc<Southward>,
        ai_engine: Option<Arc<dyn AiEngineApi>>,
    ) -> Self {
        let index = Arc::new(RuntimeIndex::new());
        let snapshot_gc = Arc::new(SnapshotGcRuntime {
            started: AtomicUsize::new(0),
            shutdown: CancellationToken::new(),
            cfg: snapshot_gc_cfg,
        });
        let runtime = Arc::new(SouthwardRuntime {
            index,
            device_snapshots: Arc::new(DashMap::new()),
            snapshot_gc,
        });
        Self {
            runtime,
            southward_registry,
            metrics_hub,
            ai_engine,
        }
    }

    /// Get a clone of the internal runtime index.
    ///
    /// This is intended for core-internal adapters (e.g. `NorthwardRuntimeApi`) and should not
    /// be exposed outside of core.
    #[inline]
    pub(crate) fn runtime_index(&self) -> Arc<RuntimeIndex> {
        Arc::clone(&self.runtime.index)
    }

    /// Get pre-resolved per-channel metric handles for the given channel.
    ///
    /// # Notes
    /// These handles are created during channel instance construction and are backed by
    /// `NGMetricsHub` (single source of truth).
    #[inline]
    pub fn get_channel_metric_handles(
        &self,
        channel_id: i32,
    ) -> Option<Arc<SouthwardChannelMetricHandles>> {
        self.runtime
            .index
            .channels
            .get(&channel_id)
            .map(|e| Arc::clone(&e.prom))
    }

    #[inline]
    pub(crate) fn broadcast_runtime_delta(&self, delta: RuntimeDelta) {
        // Ignore send errors when there are no active receivers.
        let _ = self.runtime.index.runtime_delta_tx.send(delta);
    }

    // === Thin delegates to `RuntimeIndex` (control-plane helpers) ===

    /// Get device ids bound to a channel (best-effort snapshot).
    #[inline]
    pub fn channel_device_ids(&self, channel_id: i32) -> Vec<i32> {
        self.runtime.index.channel_device_ids(channel_id)
    }

    /// Get points for a specific device (returns a cached empty slice when missing).
    #[inline]
    pub fn get_device_points(&self, device_id: i32) -> Arc<[Arc<dyn RuntimePoint>]> {
        self.runtime.index.get_device_points(device_id)
    }

    /// Get actions for a specific device (returns a cached empty slice when missing).
    #[inline]
    pub fn get_device_actions(&self, device_id: i32) -> Arc<[Arc<dyn RuntimeAction>]> {
        self.runtime.index.get_device_actions(device_id)
    }

    /// Get readable points for a device (best-effort snapshot).
    #[inline]
    pub fn get_readable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.runtime.index.get_readable_data_points(device_id)
    }

    /// Get writable points for a device (best-effort snapshot).
    #[inline]
    pub fn get_writable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.runtime.index.get_writable_data_points(device_id)
    }

    /// Refresh manager-level metrics from the current runtime index.
    ///
    /// # Notes
    /// This is intended to be called on topology mutations (init/add/remove), not on every snapshot.
    pub async fn refresh_manager_snapshot_from_index(&self) {
        let total_channels = self.runtime.index.channels.len() as u64;
        let total_devices = self.runtime.index.devices.len() as u64;

        // Baseline connected channels by scan (best-effort). After initialization,
        // the supervision observer maintains this value incrementally in the hub.
        let connected_channels = self
            .runtime
            .index
            .channels
            .iter()
            .filter(|entry| entry.state.is_connected())
            .count() as u64;

        let active_devices = self
            .runtime
            .index
            .devices
            .iter()
            .filter(|entry| entry.state == DeviceState::Active)
            .count() as u64;

        let total_data_points = self
            .runtime
            .index
            .device_points
            .iter()
            .map(|entry| entry.value().len() as u64)
            .sum::<u64>();

        let total_actions = self
            .runtime
            .index
            .device_actions
            .iter()
            .map(|entry| entry.value().len() as u64)
            .sum::<u64>();

        // Update the single source of truth in the hub (granular APIs).
        self.metrics_hub
            .set_southward_total_channels(total_channels);
        self.metrics_hub
            .set_southward_connected_channels(connected_channels);
        self.metrics_hub.set_southward_total_devices(total_devices);
        self.metrics_hub
            .set_southward_active_devices(active_devices);
        self.metrics_hub
            .set_southward_total_data_points(total_data_points);
        self.metrics_hub.set_southward_total_actions(total_actions);
    }

    /// Get manager metrics
    pub async fn get_metrics(&self) -> SouthwardManagerMetricsSnapshot {
        self.metrics_hub.snapshot_southward_manager()
    }

    /// Get a best-effort snapshot of a channel's runtime statistics.
    ///
    /// # Notes
    /// - This method is designed for **control-plane and observability** use cases (REST/WS/UI),
    ///   not for the hot-path data pipeline.
    /// - `driver_name` is currently populated as a stable placeholder (`driver_id`) because the
    ///   runtime layer does not guarantee a human-readable driver name without querying the DB.
    #[inline]
    pub fn get_channel_snapshot(&self, channel_id: i32) -> Option<ChannelStatsSnapshot> {
        let entry = self.runtime.index.channels.get(&channel_id)?;
        let channel_name = entry.config.name().to_string();
        let driver_name = entry.config.driver_id().to_string();
        let state = entry.state.clone();
        let created_at = entry.created_at;
        let last_activity = entry.last_activity;
        drop(entry);

        let device_count = self
            .runtime
            .index
            .channel_devices
            .get(&channel_id)
            .map(|e| e.value().len())
            .unwrap_or(0);

        Some(
            self.metrics_hub
                .build_southward_channel_snapshot(SouthwardChannelSnapshotParams {
                    channel_id,
                    name: channel_name,
                    driver_name,
                    state,
                    device_count,
                    created_at,
                    last_activity,
                }),
        )
    }

    /// Start background snapshot GC tasks (idempotent).
    #[inline]
    pub fn start_snapshot_gc(&self) {
        self.runtime.start_snapshot_gc();
    }
}

// Implement the trait for accessing connection states
impl SouthwardManager for NGSouthwardManager {
    fn get_channel_connection_state(&self, channel_id: i32) -> Option<Arc<ConnectionState>> {
        self.runtime
            .index
            .channels
            .get(&channel_id)
            .map(|entry| entry.state.clone())
    }
}
