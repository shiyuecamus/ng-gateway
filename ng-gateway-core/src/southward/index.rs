use crate::southward::manager::{ChannelInstance, DeviceInstance};
use dashmap::DashMap;
use ng_gateway_sdk::{PointMeta, RuntimeAction, RuntimeDelta, RuntimePoint};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shared runtime point slice stored in the index.
pub type RuntimePointSlice = Arc<[Arc<dyn RuntimePoint>]>;

/// Shared runtime action slice stored in the index.
pub type RuntimeActionSlice = Arc<[Arc<dyn RuntimeAction>]>;

/// device_id -> points
pub type DevicePointsIndex = Arc<DashMap<i32, RuntimePointSlice>>;

/// device_id -> actions
pub type DeviceActionsIndex = Arc<DashMap<i32, RuntimeActionSlice>>;

/// Unified point entry stored in the runtime index.
///
/// Keeping `RuntimePoint` and its derived `PointMeta` together:
/// - avoids dual-map drift
/// - enables single-lookup hot paths (write-point)
/// - reduces the risk of deadlocks caused by cross-DashMap lock ordering
#[derive(Clone)]
pub struct PointEntry {
    pub point: Arc<dyn RuntimePoint>,
    pub meta: Arc<PointMeta>,
}

/// Aggregated runtime index for channels, devices, and their children.
///
/// Centralizes all high-frequency maps to improve cohesion and encapsulation.
#[derive(Clone)]
pub struct RuntimeIndex {
    // channel_id -> ChannelInstance
    pub channels: Arc<DashMap<i32, ChannelInstance>>,
    // device_id -> DeviceInstance
    pub devices: Arc<DashMap<i32, DeviceInstance>>,
    // device_name -> device_id
    pub device_name_index: Arc<DashMap<Arc<str>, i32>>,
    // channel_id -> set of device_id
    pub channel_devices: Arc<DashMap<i32, Arc<[i32]>>>,
    // device_id -> points (stored as Arc slice to avoid per-read Vec allocations)
    pub device_points: DevicePointsIndex,
    /// point_id -> point entry (point + meta) for control-plane and northward lookups.
    pub point_entries_by_id: Arc<DashMap<i32, Arc<PointEntry>>>,
    // device_id -> actions (stored as Arc slice to avoid per-read Vec allocations)
    pub device_actions: DeviceActionsIndex,

    /// Reverse lookup: (channel_name, device_name, point_key) -> point_id.
    ///
    /// This is primarily used by write-back paths and topic-based routing.
    pub point_id_by_path: Arc<DashMap<String, i32>>,

    /// Broadcast channel for runtime topology deltas.
    ///
    /// Northward plugins can subscribe to keep local caches in sync.
    pub runtime_delta_tx: broadcast::Sender<RuntimeDelta>,
}

impl Default for RuntimeIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeIndex {
    pub fn new() -> Self {
        let (runtime_delta_tx, _) = broadcast::channel(1024);
        Self {
            channels: Arc::new(DashMap::new()),
            devices: Arc::new(DashMap::new()),
            device_name_index: Arc::new(DashMap::new()),
            channel_devices: Arc::new(DashMap::new()),
            device_points: Arc::new(DashMap::new()),
            point_entries_by_id: Arc::new(DashMap::new()),
            device_actions: Arc::new(DashMap::new()),
            point_id_by_path: Arc::new(DashMap::new()),
            runtime_delta_tx,
        }
    }
}
