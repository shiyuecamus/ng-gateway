pub mod bus;
pub mod index;
pub(crate) mod internal;
mod lifecycle;
pub mod loader;
pub mod manager;
pub mod observer;
mod publisher;
mod queries;
mod runtime_mutation;
mod snapshot_cache;
mod snapshot_gc;
mod topology;

pub use bus::SouthwardDataBus;
pub use index::RuntimeIndex;
pub use loader::{SouthwardLoader, SouthwardProbeInfo, SouthwardRegistry};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use ng_gateway_common::metrics::{southward::SouthwardChannelMetricHandles, NGMetricsHub};
use ng_gateway_models::{
    entities::prelude::{ActionModel, ChannelModel, DeviceModel, PointModel},
    settings::Southward,
};
use ng_gateway_sdk::{
    ConnectionState, DeviceState, Driver, DriverFactory, NGValue, RuntimeChannel, RuntimeDevice,
    Status,
};
use std::{
    collections::HashMap,
    sync::{atomic::AtomicUsize, Arc},
};
use tokio_util::sync::CancellationToken;

// === Public aliases / data structures used across southward and other modules ===

/// Device initialization tuple alias used during topology assembly.
///
/// Represents one device with its associated data points and actions.
pub type DeviceInitTriple = (DeviceModel, Vec<PointModel>, Vec<ActionModel>);

/// Channel initialization entry alias used during topology assembly.
///
/// Represents one channel with a list of device entries.
pub type ChannelInitEntry = (ChannelModel, Vec<DeviceInitTriple>);

/// Device snapshot tuple containing device, driver, and channel id.
///
/// Used when snapshotting device runtime state along with its associated driver and channel.
pub type DeviceDriverSnapshot = (Arc<dyn RuntimeDevice>, Arc<dyn Driver>, i32);

/// Filter used when resolving connected devices for subscription synchronization.
#[derive(Debug, Clone)]
pub enum SubscriptionFilter {
    /// Match all devices that currently belong to connected channels.
    AllDevices,
    /// Match the provided device identifiers.
    DeviceIds(Vec<i32>),
}

/// Snapshot describing a currently connected device instance.
#[derive(Debug, Clone)]
pub struct ConnectedDeviceSnapshot {
    /// Device identifier.
    pub device_id: i32,
    /// Device name.
    pub device_name: Arc<str>,
    /// Device type.
    pub device_type: Arc<str>,
    /// Owning channel identifier.
    pub channel_id: i32,
    /// Owning channel name.
    pub channel_name: Arc<str>,
    /// Last activity timestamp recorded on the channel.
    pub last_activity: DateTime<Utc>,
}

/// Basic device info snapshot for observability/monitoring UIs.
#[derive(Debug, Clone)]
pub struct DeviceBasicSnapshot {
    pub device_id: i32,
    pub channel_id: i32,
    pub device_name: String,
    pub device_type: String,
    pub status: Status,
    pub state: DeviceState,
    pub last_collection: Option<DateTime<Utc>>,
    pub last_data_change: Option<DateTime<Utc>>,
}

/// Per-point snapshot entry stored in `DeviceDataSnapshot`.
///
/// Stores the monotonic-clock timestamp for TTL/change detection, the typed value,
/// and an optional source timestamp from the device/protocol layer (epoch milliseconds).
pub type PointSnapshotEntry = (u64, NGValue, Option<i64>);

/// Device data snapshot containing the latest telemetry and attribute values.
///
/// This snapshot is maintained for each device and updated whenever new data arrives.
/// It is used to provide full data snapshots to northward apps when they subscribe to devices.
#[derive(Debug, Clone)]
pub struct DeviceDataSnapshot {
    /// Device identifier.
    pub device_id: i32,
    /// Device name.
    pub device_name: Arc<str>,
    /// Latest telemetry values (`point_id` -> snapshot entry).
    ///
    /// `point_id` is the primary key for hot-path change detection.
    pub telemetry: HashMap<i32, PointSnapshotEntry>,
    /// Latest client attributes (`point_id` -> snapshot entry).
    pub client_attributes: HashMap<i32, PointSnapshotEntry>,
    /// Latest shared attributes (`point_id` -> snapshot entry).
    pub shared_attributes: HashMap<i32, PointSnapshotEntry>,
    /// Latest server attributes (`point_id` -> snapshot entry).
    pub server_attributes: HashMap<i32, PointSnapshotEntry>,
    /// Cached mapping from `point_id` to `point_key` for points that have ever appeared in this snapshot.
    pub point_key_by_id: HashMap<i32, Arc<str>>,
    /// Timestamp of the last update (wall clock).
    pub last_update: DateTime<Utc>,
}

/// Active channel instance with driver and metadata.
#[derive(Clone)]
pub struct ChannelInstance {
    /// Driver instance (direct, aligned with AppActor::plugin).
    pub driver: Arc<dyn Driver>,
    /// Driver factory for this channel.
    pub driver_factory: Arc<dyn DriverFactory>,
    /// Runtime channel (parsed and cached for driver init and updates).
    pub config: Arc<dyn RuntimeChannel>,
    /// Connection state (snapshot stream value).
    pub state: Arc<ConnectionState>,
    /// Channel status (enabled/disabled).
    pub status: Status,
    /// Southward per-channel metric handles (single source of truth in `NGMetricsHub`).
    pub prom: Arc<SouthwardChannelMetricHandles>,
    /// Cached driver label for metrics/logging (low-cardinality).
    pub driver_label: Arc<str>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Last activity timestamp.
    pub last_activity: DateTime<Utc>,
}

impl ChannelInstance {
    /// Returns true when the channel is currently connected.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.state.is_connected()
    }

    /// Returns true when the channel is enabled.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.status == Status::Enabled
    }

    /// Returns true when the channel is configured as collectable (polling enabled + enabled).
    #[inline]
    pub fn is_collectable_configured(&self) -> bool {
        self.is_enabled() && self.config.collectable()
    }

    /// Returns true when the channel is collectable at runtime (configured + connected).
    #[inline]
    pub fn is_collectable_runtime(&self) -> bool {
        self.is_collectable_configured() && self.is_connected()
    }

    /// Update the channel connection state snapshot.
    #[inline]
    pub fn set_state(&mut self, state: Arc<ConnectionState>) {
        self.state = state;
    }

    /// Update the channel last activity timestamp.
    #[inline]
    pub fn touch_activity(&mut self, now: DateTime<Utc>) {
        self.last_activity = now;
    }

    /// Snapshot the cached driver label (low-cardinality).
    #[inline]
    pub fn snapshot_driver_label(&self) -> Arc<str> {
        Arc::clone(&self.driver_label)
    }

    /// Snapshot the driver handle.
    #[inline]
    pub fn snapshot_driver(&self) -> Arc<dyn Driver> {
        Arc::clone(&self.driver)
    }
}

/// Active device instance with configuration and runtime data.
#[derive(Clone)]
pub struct DeviceInstance {
    /// Device configuration.
    pub config: Arc<dyn RuntimeDevice>,
    /// Device state.
    pub state: DeviceState,
    /// Device status.
    pub status: Status,
    /// Driver instance (direct, aligned with AppActor::plugin).
    pub driver: Arc<dyn Driver>,
    /// Last data collection timestamp.
    pub last_collection: Option<DateTime<Utc>>,
    /// Last data change timestamp.
    pub last_data_change: Option<DateTime<Utc>>,
    /// Creation timestamp.
    pub created_at: DateTime<Utc>,
}

impl DeviceInstance {
    /// Update the device runtime status.
    #[inline]
    pub fn set_status(&mut self, status: Status) {
        self.status = status;
    }

    /// Update the device driver binding.
    #[inline]
    pub fn set_driver(&mut self, driver: Arc<dyn Driver>) {
        self.driver = driver;
    }

    /// Update the last collection timestamp.
    #[inline]
    pub fn touch_last_collection(&mut self, now: DateTime<Utc>) {
        self.last_collection = Some(now);
    }

    /// Update the last data change timestamp.
    #[inline]
    pub fn touch_last_data_change(&mut self, now: DateTime<Utc>) {
        self.last_data_change = Some(now);
    }
}

// === Internal GC runtime state (kept here so child modules can access private fields) ===

/// Snapshot GC runtime state.
///
/// This is a best-effort background memory control mechanism.
pub(crate) struct SnapshotGcRuntime {
    pub(crate) started: AtomicUsize, // 0 = not started, 1 = started
    pub(crate) shutdown: CancellationToken,
    pub(crate) cfg: Arc<Southward>,
}

/// Aggregated southward runtime state.
///
/// This struct groups the mutable runtime tables and maintenance runtimes that are used across
/// multiple southward submodules.
///
/// # Notes
/// - This is **core-internal** only: it must not leak outside the `ng-gateway-core` crate.
/// - The runtime is designed for high concurrency: hot tables are backed by `DashMap`.
#[derive(Clone)]
pub(crate) struct SouthwardRuntime {
    /// Aggregated runtime index (channels, devices, points, actions, mappings).
    pub(crate) index: Arc<RuntimeIndex>,
    /// Device data snapshots: device_id -> DeviceDataSnapshot.
    pub(crate) device_snapshots: Arc<DashMap<i32, DeviceDataSnapshot>>,
    /// Snapshot GC runtime (point-level TTL eviction).
    pub(crate) snapshot_gc: Arc<SnapshotGcRuntime>,
}

impl SouthwardRuntime {
    /// Clear all runtime tables and caches (best-effort).
    ///
    /// # Notes
    /// - This is a **control-plane** operation intended for shutdown paths.
    /// - This does not cancel GC tasks; lifecycle code must handle cancellation separately if needed.
    #[inline]
    pub(crate) fn clear(&self) {
        self.index.clear();
        self.device_snapshots.clear();
    }

    /// Start background snapshot GC tasks (idempotent).
    #[inline]
    pub(crate) fn start_snapshot_gc(&self) {
        self.snapshot_gc.start(Arc::clone(&self.device_snapshots));
    }
}

/// High-performance southward manager with connection pooling and health monitoring.
#[derive(Clone)]
pub struct NGSouthwardManager {
    /// Aggregated runtime state (index + caches + maintenance runtimes).
    pub(crate) runtime: Arc<SouthwardRuntime>,
    /// Driver registry for creating new drivers.
    pub(crate) southward_registry: SouthwardRegistry,
    /// Metrics hub (single source of truth).
    pub(crate) metrics_hub: Arc<NGMetricsHub>,
}
