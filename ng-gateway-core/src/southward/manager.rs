use crate::{
    lifecycle::{start_with_policy, StartPolicy},
    southward::{
        index::{PointEntry, RuntimeIndex},
        monitor::ChannelMonitor,
        publisher::MpscNorthwardPublisher,
    },
};
use chrono::{DateTime, Utc};
use dashmap::mapref::entry::Entry as DashEntry;
use dashmap::{mapref::one::Ref, DashMap};
use futures::{
    future::join_all,
    stream::{self, StreamExt},
};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{
    core::metrics::{ChannelMetrics, DeviceMetrics, SouthwardManagerMetrics},
    entities::prelude::{ActionModel, ChannelModel, DeviceModel, PointModel},
    SouthwardManager,
};
use ng_gateway_sdk::{
    AccessMode, AttributeData, CollectionType, DeviceState, Driver, DriverFactory, DriverHealth,
    DriverRegistry, NGValue, NorthwardData, PointMeta, ReportType, RuntimeAction, RuntimeChannel,
    RuntimeDelta, RuntimeDevice, RuntimePoint, SouthwardConnectionState, SouthwardInitContext,
    Status, TelemetryData,
};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    future::Future,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc::Sender, Mutex},
    time::timeout,
};
use tracing::{error, info, warn};

/// Build a reverse-lookup key for `(channel_name, device_name, point_key)`.
///
/// This is NOT a hot-path function. Telemetry encoding should prefer `point_id -> PointMeta`.
#[inline]
fn make_point_path_key(channel_name: &str, device_name: &str, point_key: &str) -> String {
    const SEP: char = '\u{1f}';
    let mut s = String::with_capacity(channel_name.len() + device_name.len() + point_key.len() + 2);
    s.push_str(channel_name);
    s.push(SEP);
    s.push_str(device_name);
    s.push(SEP);
    s.push_str(point_key);
    s
}

/// Device initialization tuple alias used during topology assembly
///
/// Represents one device with its associated data points and actions.
pub type DeviceInitTriple = (DeviceModel, Vec<PointModel>, Vec<ActionModel>);

/// Channel initialization entry alias used during topology assembly
///
/// Represents one channel with a list of device entries.
pub type ChannelInitEntry = (ChannelModel, Vec<DeviceInitTriple>);

/// Device snapshot tuple containing device, driver, and channel id
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

/// Device data snapshot containing the latest telemetry and attribute values
///
/// This snapshot is maintained for each device and updated whenever new data arrives.
/// It is used to provide full data snapshots to northward apps when they subscribe to devices.
#[derive(Debug, Clone)]
pub struct DeviceDataSnapshot {
    /// Device identifier
    pub device_id: i32,
    /// Device name
    pub device_name: Arc<str>,
    /// Latest telemetry values (`point_id` -> typed value).
    ///
    /// `point_id` is the primary key for hot-path change detection.
    pub telemetry: HashMap<i32, NGValue>,
    /// Latest client attributes (`point_id` -> typed value).
    pub client_attributes: HashMap<i32, NGValue>,
    /// Latest shared attributes (`point_id` -> typed value).
    pub shared_attributes: HashMap<i32, NGValue>,
    /// Latest server attributes (`point_id` -> typed value).
    pub server_attributes: HashMap<i32, NGValue>,
    /// Cached mapping from `point_id` to `point_key` for points that have ever appeared in this snapshot.
    ///
    /// # Why keep this cache?
    /// Monitoring and encoding layers often need a stable string key. Since `PointValue` now carries
    /// `point_key`, we persist it here to avoid repeated runtime metadata lookups when generating
    /// JSON snapshots.
    pub point_key_by_id: HashMap<i32, Arc<str>>,
    /// Timestamp of the last update
    pub last_update: DateTime<Utc>,
}

/// Active channel instance with driver and metadata
#[derive(Clone)]
pub struct ChannelInstance {
    /// Driver instance (direct, aligned with AppActor::plugin)
    pub driver: Arc<dyn Driver>,

    /// Driver factory for this channel
    pub driver_factory: Arc<dyn DriverFactory>,

    /// Runtime channel (parsed and cached for driver init and updates)
    pub config: Arc<dyn RuntimeChannel>,

    /// Connection state
    pub state: SouthwardConnectionState,

    /// Channel status
    pub status: Status,

    /// Performance metrics
    pub metrics: ChannelMetrics,

    /// Last health check result
    pub health: Option<DriverHealth>,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,

    /// Last activity timestamp
    pub last_activity: DateTime<Utc>,
}

/// Active device instance with configuration and runtime data
#[derive(Clone)]
pub struct DeviceInstance {
    /// Device configuration
    pub config: Arc<dyn RuntimeDevice>,

    /// Device state
    pub state: DeviceState,

    /// Device status
    pub status: Status,

    /// Driver instance (direct, aligned with AppActor::plugin)
    pub driver: Arc<dyn Driver>,

    /// Last data collection timestamp
    pub last_collection: Option<DateTime<Utc>>,

    /// Last data change timestamp
    pub last_data_change: Option<DateTime<Utc>>,

    /// Device metrics
    pub metrics: DeviceMetrics,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
}

/// High-performance channel manager with connection pooling and health monitoring
#[derive(Clone)]
pub struct NGSouthwardManager {
    /// Aggregated runtime index (channels, devices, points, actions, mappings)
    index: Arc<RuntimeIndex>,

    /// Data manager metrics
    metrics: Arc<Mutex<SouthwardManagerMetrics>>,

    /// Driver registry for creating new drivers
    driver_registry: DriverRegistry,

    /// Channel monitor component
    monitor: ChannelMonitor,

    /// Device data snapshots: device_id -> DeviceDataSnapshot
    ///
    /// Maintains the latest telemetry and attribute values for each device.
    /// Updated whenever new data arrives, used to provide full snapshots to
    /// northward apps when they subscribe to devices.
    ///
    /// NOTE: We store `DeviceDataSnapshot` directly to enable in-place updates
    /// under the `DashMap` shard lock and avoid cloning large maps on hot paths.
    device_snapshots: Arc<DashMap<i32, DeviceDataSnapshot>>,
}

impl NGSouthwardManager {
    #[inline]
    /// Create a new data manager
    pub fn new(driver_registry: DriverRegistry) -> Self {
        let index = Arc::new(RuntimeIndex::new());
        let monitor = ChannelMonitor::new(Arc::clone(&index));
        Self {
            index,
            metrics: Arc::new(Mutex::new(SouthwardManagerMetrics::default())),
            driver_registry,
            monitor,
            device_snapshots: Arc::new(DashMap::new()),
        }
    }

    /// Get a clone of the internal runtime index.
    ///
    /// This is intended for core-internal adapters (e.g. `NorthwardRuntimeApi`) and should not
    /// be exposed outside of core.
    #[inline]
    pub(crate) fn runtime_index(&self) -> Arc<RuntimeIndex> {
        Arc::clone(&self.index)
    }

    #[inline]
    fn broadcast_runtime_delta(&self, delta: RuntimeDelta) {
        // Ignore send errors when there are no active receivers.
        let _ = self.index.runtime_delta_tx.send(delta);
    }

    // === Arc-slice index helpers (control-plane path, OK to rebuild slices) ===

    #[inline]
    fn channel_device_ids(&self, channel_id: i32) -> Vec<i32> {
        self.index
            .channel_devices
            .get(&channel_id)
            .map(|e| e.value().iter().copied().collect())
            .unwrap_or_default()
    }

    #[inline]
    fn add_device_to_channel(&self, channel_id: i32, device_id: i32) {
        // IMPORTANT: do update atomically under DashMap entry guard to avoid lost updates.
        match self.index.channel_devices.entry(channel_id) {
            DashEntry::Occupied(mut occ) => {
                let mut ids: Vec<i32> = occ.get().iter().copied().collect();
                if !ids.contains(&device_id) {
                    ids.push(device_id);
                    *occ.get_mut() = Arc::from(ids.into_boxed_slice());
                }
            }
            DashEntry::Vacant(vac) => {
                vac.insert(Arc::from(vec![device_id].into_boxed_slice()));
            }
        }
    }

    #[inline]
    fn remove_device_from_channel(&self, channel_id: i32, device_id: i32) {
        // IMPORTANT: do update atomically under DashMap entry guard to avoid lost updates.
        match self.index.channel_devices.entry(channel_id) {
            DashEntry::Occupied(mut occ) => {
                let mut ids: Vec<i32> = occ.get().iter().copied().collect();
                let before = ids.len();
                ids.retain(|x| *x != device_id);
                if ids.len() != before {
                    // Keep empty slice rather than removing the key to avoid races that could
                    // delete concurrent inserts.
                    *occ.get_mut() = Arc::from(ids.into_boxed_slice());
                }
            }
            DashEntry::Vacant(_) => {}
        }
    }

    #[inline]
    fn device_points_slice(&self, device_id: i32) -> Option<Arc<[Arc<dyn RuntimePoint>]>> {
        self.index
            .device_points
            .get(&device_id)
            .map(|e| Arc::clone(e.value()))
    }

    #[inline]
    fn set_device_points(&self, device_id: i32, points: Vec<Arc<dyn RuntimePoint>>) {
        // Atomic write to avoid lost updates (even though most callers already serialize per device).
        match self.index.device_points.entry(device_id) {
            DashEntry::Occupied(mut occ) => {
                *occ.get_mut() = Arc::from(points.into_boxed_slice());
            }
            DashEntry::Vacant(vac) => {
                vac.insert(Arc::from(points.into_boxed_slice()));
            }
        }
    }

    /// Atomically mutate device points under the DashMap entry guard.
    ///
    /// NOTE: `f` MUST NOT touch other DashMaps to avoid lock ordering risks.
    #[inline]
    fn mutate_device_points<R>(
        &self,
        device_id: i32,
        f: impl FnOnce(&mut Vec<Arc<dyn RuntimePoint>>) -> R,
    ) -> R {
        match self.index.device_points.entry(device_id) {
            DashEntry::Occupied(mut occ) => {
                let mut v: Vec<Arc<dyn RuntimePoint>> = occ.get().iter().cloned().collect();
                let r = f(&mut v);
                *occ.get_mut() = Arc::from(v.into_boxed_slice());
                r
            }
            DashEntry::Vacant(vac) => {
                let mut v: Vec<Arc<dyn RuntimePoint>> = Vec::new();
                let r = f(&mut v);
                vac.insert(Arc::from(v.into_boxed_slice()));
                r
            }
        }
    }

    #[inline]
    fn device_actions_slice(&self, device_id: i32) -> Option<Arc<[Arc<dyn RuntimeAction>]>> {
        self.index
            .device_actions
            .get(&device_id)
            .map(|e| Arc::clone(e.value()))
    }

    #[inline]
    fn set_device_actions(&self, device_id: i32, actions: Vec<Arc<dyn RuntimeAction>>) {
        // Atomic write to avoid lost updates.
        match self.index.device_actions.entry(device_id) {
            DashEntry::Occupied(mut occ) => {
                *occ.get_mut() = Arc::from(actions.into_boxed_slice());
            }
            DashEntry::Vacant(vac) => {
                vac.insert(Arc::from(actions.into_boxed_slice()));
            }
        }
    }

    /// Atomically mutate device actions under the DashMap entry guard.
    ///
    /// NOTE: `f` MUST NOT touch other DashMaps to avoid lock ordering risks.
    #[inline]
    fn mutate_device_actions<R>(
        &self,
        device_id: i32,
        f: impl FnOnce(&mut Vec<Arc<dyn RuntimeAction>>) -> R,
    ) -> R {
        match self.index.device_actions.entry(device_id) {
            DashEntry::Occupied(mut occ) => {
                let mut v: Vec<Arc<dyn RuntimeAction>> = occ.get().iter().cloned().collect();
                let r = f(&mut v);
                *occ.get_mut() = Arc::from(v.into_boxed_slice());
                r
            }
            DashEntry::Vacant(vac) => {
                let mut v: Vec<Arc<dyn RuntimeAction>> = Vec::new();
                let r = f(&mut v);
                vac.insert(Arc::from(v.into_boxed_slice()));
                r
            }
        }
    }

    /// Upsert point entry indexes for a runtime point.
    ///
    /// This is called on topology changes and is not a hot path.
    /// Implementation detail: avoids holding a DashMap shard guard while touching another DashMap.
    fn upsert_point_entry(
        &self,
        channel_name: &str,
        device: &Arc<dyn RuntimeDevice>,
        point: &Arc<dyn RuntimePoint>,
        description: Option<Arc<str>>,
    ) {
        // If path changes, remove the old reverse mapping first (drop map guard before removal).
        let old_key = self.index.point_entries_by_id.get(&point.id()).map(|old| {
            let m = &old.value().meta;
            make_point_path_key(
                m.channel_name.as_ref(),
                m.device_name.as_ref(),
                m.point_key.as_ref(),
            )
        });
        if let Some(old_key) = old_key {
            self.index.point_id_by_path.remove(&old_key);
        }

        let channel_id = device.channel_id();
        let meta = PointMeta {
            point_id: point.id(),
            channel_id,
            channel_name: Arc::<str>::from(channel_name),
            device_id: device.id(),
            device_name: Arc::<str>::from(device.device_name()),
            point_name: Arc::<str>::from(point.name()),
            point_key: Arc::<str>::from(point.key()),
            data_type: point.data_type(),
            access_mode: point.access_mode(),
            unit: point.unit().map(Arc::<str>::from),
            min_value: point.min_value(),
            max_value: point.max_value(),
            scale: point.scale(),
            description,
        };
        let meta = Arc::new(meta);
        let entry = Arc::new(PointEntry {
            point: Arc::clone(point),
            meta: Arc::clone(&meta),
        });
        let key = make_point_path_key(
            meta.channel_name.as_ref(),
            meta.device_name.as_ref(),
            meta.point_key.as_ref(),
        );
        // Insert the entry first so `point_id -> meta` is immediately consistent.
        self.index.point_entries_by_id.insert(meta.point_id, entry);
        self.index.point_id_by_path.insert(key, meta.point_id);
    }

    #[inline]
    fn remove_point_entry_by_id(&self, point_id: i32) {
        // Remove reverse mapping first (compute key without holding the guard across map ops).
        let old_key = self.index.point_entries_by_id.get(&point_id).map(|old| {
            let m = &old.value().meta;
            make_point_path_key(
                m.channel_name.as_ref(),
                m.device_name.as_ref(),
                m.point_key.as_ref(),
            )
        });
        if let Some(old_key) = old_key {
            self.index.point_id_by_path.remove(&old_key);
        }
        self.index.point_entries_by_id.remove(&point_id);
    }

    /// Initialize manager from a fully assembled topology in a single high-performance pass.
    ///
    /// Best-effort behavior: channel/device/point/action failures are isolated and logged; others continue.
    /// Concurrency model: per-channel tasks run concurrently; within each task, devices/points/actions are processed sequentially
    /// to leverage a shared `DriverFactory` and avoid contention. This minimizes allocations and roundtrips.
    pub async fn initialize_topology(
        &self,
        topology: Vec<ChannelInitEntry>,
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> NGResult<()> {
        let successful_channels = Arc::new(AtomicUsize::new(0));
        let failed_channels = Arc::new(AtomicUsize::new(0));
        let total_devices_ok = Arc::new(AtomicUsize::new(0));
        let total_points_ok = Arc::new(AtomicUsize::new(0));
        let total_actions_ok = Arc::new(AtomicUsize::new(0));

        // Run per-channel concurrently while reusing extracted initializer APIs
        stream::iter(topology)
            .for_each_concurrent(None, |(channel_config, dev_triples)| async {
                // Initialize channel with full topology context and commit
                match self
                    .initialize_channel_with_topology(channel_config, &dev_triples, data_tx)
                    .await
                {
                    Ok(_) => {
                        successful_channels.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        error!(error=%e, "Failed to initialize channel");
                        failed_channels.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }

                // Populate runtime indexes from topology without notifying driver (already configured via init context)
                let (d_ok, p_ok, a_ok) = self
                    .populate_indexes_from_topology(dev_triples)
                    .unwrap_or((0, 0, 0));
                total_devices_ok.fetch_add(d_ok, Ordering::Relaxed);
                total_points_ok.fetch_add(p_ok, Ordering::Relaxed);
                total_actions_ok.fetch_add(a_ok, Ordering::Relaxed);
            })
            .await;

        let sc = successful_channels.load(Ordering::Relaxed);
        let fc = failed_channels.load(Ordering::Relaxed);
        let d_ok = total_devices_ok.load(Ordering::Relaxed);
        let p_ok = total_points_ok.load(Ordering::Relaxed);
        let a_ok = total_actions_ok.load(Ordering::Relaxed);

        info!(
            "Topology initialization completed: {sc} channels ok, {fc} failed; {d_ok} devices, {p_ok} points, {a_ok} actions"
        );

        if sc == 0 && fc > 0 {
            return Err(NGError::InitializationError(
                "No channels were successfully initialized".to_string(),
            ));
        }

        Ok(())
    }

    #[inline]
    /// Change channel status
    pub fn change_channel_status(&self, channel_id: i32, status: Status) {
        if let Some(mut chan) = self.index.channels.get_mut(&channel_id) {
            chan.status = status;
        }
    }

    #[inline]
    /// Notify driver about device status change without altering runtime tables
    pub async fn change_device_status(&self, device: &DeviceModel, status: Status) -> NGResult<()> {
        // Resolve device instance and its channel
        let mut dev = match self.index.devices.get_mut(&device.id) {
            Some(d) => d,
            None => return Ok(()),
        };
        // Update in-memory status
        dev.status = status;
        let channel_id = dev.config.channel_id();
        // Build and send delta (fire-and-forget)
        let delta = RuntimeDelta::DevicesChanged {
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
            status_changed: vec![(Arc::clone(&dev.config), status)],
        };
        let driver = match self.snapshot_channel_driver(channel_id) {
            Some(d) => d,
            None => return Ok(()),
        };
        tokio::spawn(async move {
            if let Err(e) = driver.apply_runtime_delta(delta).await {
                error!(error=%e, "Failed to apply runtime delta");
            }
        });
        Ok(())
    }

    #[inline]
    /// Wait until driver connection reaches Connected or Failed, with timeout.
    ///
    /// Kept as a thin wrapper used by the lifecycle implementation for drivers.
    pub async fn wait_for_final(&self, driver: &Arc<dyn Driver>, timeout_ms: u64) -> NGResult<()> {
        let mut rx = driver.subscribe_connection_state();

        match timeout(Duration::from_millis(timeout_ms), async move {
            // Convert Ref<'_, ConnectionState> to owned ConnectionState immediately
            rx.wait_for(|state| {
                matches!(
                    state,
                    SouthwardConnectionState::Connected | SouthwardConnectionState::Failed(_)
                )
            })
            .await
            .map(|r| r.clone())
        })
        .await
        {
            Ok(Ok(state)) => match state {
                SouthwardConnectionState::Connected => Ok(()),
                SouthwardConnectionState::Failed(reason) => Err(NGError::DriverError(format!(
                    "Driver connection failed: {}",
                    reason
                ))),
                _ => Err(NGError::DriverError("Invalid connection state".to_string())),
            },
            Ok(Err(_)) => Err(NGError::DriverError(
                "Driver connection state channel closed".to_string(),
            )),
            Err(_) => Err(NGError::DriverError(format!(
                "Driver connection timeout after {} ms",
                timeout_ms
            ))),
        }
    }

    // init_channel_by_id removed: drivers are created during channel instance creation

    /// Start a channel by id using its current runtime context
    pub async fn start_channel(
        &self,
        channel_id: i32,
        _data_tx: &Sender<Arc<NorthwardData>>,
        policy: StartPolicy,
    ) -> NGResult<()> {
        // Driver should already be created at instance creation time
        let driver = match self.snapshot_channel_driver(channel_id) {
            Some(d) => d,
            None => return Ok(()),
        };

        // Bridge driver lifecycle into the generic start helper.
        let driver_for_start = Arc::clone(&driver);
        let start_fn = move || async move {
            driver_for_start
                .start()
                .await
                .map_err(|e| NGError::DriverError(e.to_string()))
        };

        let this = self;
        let driver_for_wait = Arc::clone(&driver);
        let wait_fn = move |timeout_ms: u64| async move {
            this.wait_for_final(&driver_for_wait, timeout_ms).await
        };

        start_with_policy(policy, start_fn, wait_fn).await?;

        // Update bookkeeping (last_activity) after start operation.
        if let Some(mut entry) = self.index.channels.get_mut(&channel_id) {
            entry.last_activity = Utc::now();
        }

        Ok(())
    }

    #[inline]
    /// Create a channel, commit it, and start with provided policy.
    pub async fn create_and_start_channel(
        &self,
        config: &ChannelModel,
        data_tx: &Sender<Arc<NorthwardData>>,
        policy: StartPolicy,
    ) -> NGResult<()> {
        // Prepare instance (driver created but not started) and commit
        let instance = self.create_channel_instance(config, data_tx).await?;
        let channel_id = instance.config.id();
        self.index
            .channels
            .insert(instance.config.id(), instance.clone());

        // Start according to policy via by-id path
        match self.start_channel(channel_id, data_tx, policy).await {
            Ok(()) => {
                self.monitor.spawn(channel_id, data_tx);
                Ok(())
            }
            Err(e) => {
                match policy {
                    StartPolicy::SyncWaitConnected { .. } => {
                        // On sync start failure, clean up driver and remove channel entry
                        let _ = instance.driver.stop().await;
                    }
                    StartPolicy::AsyncFireAndForget => {
                        // For async, keep instance but mark as failed
                        if let Some(mut entry) = self.index.channels.get_mut(&channel_id) {
                            entry.state =
                                SouthwardConnectionState::Failed("async start failed".to_string());
                        }
                    }
                }
                Err(e)
            }
        }
    }

    #[inline]
    /// Initialize a single channel using full topology to build the driver's init context.
    /// Then insert the channel into indexes. Devices/points/actions will be populated into
    /// indexes separately without driver deltas.
    async fn initialize_channel_with_topology(
        &self,
        config: ChannelModel,
        dev_triples: &[DeviceInitTriple],
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> NGResult<()> {
        let instance = self
            .create_channel_instance_with_topology(&config, dev_triples, data_tx)
            .await?;
        self.index.channels.insert(instance.config.id(), instance);
        Ok(())
    }

    #[inline]
    /// Create a single channel instance with driver (uninitialized)
    ///
    /// The driver is created but not initialized. Call `init_channel_by_id` to initialize it.
    pub async fn create_channel_instance(
        &self,
        config: &ChannelModel,
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> NGResult<ChannelInstance> {
        // Get driver factory by driver_id
        let driver_factory = self
            .driver_registry
            .get(&config.driver_id)
            .map(|entry| entry.value().clone())
            .ok_or(NGError::DriverError(format!(
                "Unknown driver id: {}",
                config.driver_id
            )))?;

        // Build runtime channel first (needed for driver context)
        let config = driver_factory
            .convert_runtime_channel(config.clone().into())
            .map_err(|e| NGError::DriverError(e.to_string()))?;

        // Build init context with best-effort preload from current indexes.
        // At gateway boot, devices/points may already be present; at runtime add, they may be empty.
        let ctx = self.build_channel_runtime_context(Arc::clone(&config), data_tx);

        // Create driver (Box) and convert to Arc
        let driver = driver_factory
            .create_driver(ctx)
            .map_err(|e| NGError::DriverError(e.to_string()))?;
        let driver: Arc<dyn Driver> = Arc::from(driver);

        // Defer connection to the unified start phase after devices/points are loaded
        let connection_state = SouthwardConnectionState::Disconnected;

        let now = Utc::now();
        let status = config.status();
        Ok(ChannelInstance {
            driver,
            driver_factory,
            config,
            state: connection_state,
            status,
            metrics: ChannelMetrics::default(),
            health: None,
            created_at: now,
            last_activity: now,
        })
    }

    #[inline]
    /// Create a channel instance where the driver is initialized with full topology context.
    pub async fn create_channel_instance_with_topology(
        &self,
        config: &ChannelModel,
        dev_triples: &[DeviceInitTriple],
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> NGResult<ChannelInstance> {
        // Get driver factory by driver_id
        let driver_factory = self
            .driver_registry
            .get(&config.driver_id)
            .map(|entry| entry.value().clone())
            .ok_or(NGError::DriverError(format!(
                "Unknown driver id: {}",
                config.driver_id
            )))?;

        // Build runtime channel first (needed for driver context)
        let runtime_channel = driver_factory
            .convert_runtime_channel(config.clone().into())
            .map_err(|e| NGError::DriverError(e.to_string()))?;

        // Convert devices and points from the provided topology into runtime forms
        // to supply a complete init context without relying on runtime indexes.
        let mut devices: Vec<Arc<dyn RuntimeDevice>> = Vec::with_capacity(dev_triples.len());
        let mut points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>> =
            HashMap::with_capacity(dev_triples.len());
        for (dev, pts, _acts) in dev_triples.iter() {
            // Only include devices that belong to this channel
            if dev.channel_id != config.id {
                continue;
            }
            if let Ok(rdev) = driver_factory.convert_runtime_device(dev.clone().into()) {
                let device_id = rdev.id();
                // Convert points for this device
                let rpoints = pts
                    .iter()
                    .filter_map(
                        |p| match driver_factory.convert_runtime_point(p.clone().into()) {
                            Ok(p) => Some(p),
                            Err(e) => {
                                tracing::error!("Error converting point: {:?}", e);
                                None
                            }
                        },
                    )
                    .collect::<Vec<Arc<dyn RuntimePoint>>>();
                points_by_device.insert(device_id, rpoints);
                devices.push(rdev);
            }
        }

        // Assemble init context directly from converted topology
        let ctx = SouthwardInitContext {
            devices,
            points_by_device,
            runtime_channel: Arc::clone(&runtime_channel),
            publisher: Arc::new(MpscNorthwardPublisher::new(data_tx.clone())),
        };

        // Create driver and wrap
        let driver = driver_factory
            .create_driver(ctx)
            .map_err(|e| NGError::DriverError(e.to_string()))?;
        let driver: Arc<dyn Driver> = Arc::from(driver);

        // Defer connection; status/state copied from channel config
        let connection_state = SouthwardConnectionState::Disconnected;
        let now = Utc::now();
        let status = runtime_channel.status();
        Ok(ChannelInstance {
            driver,
            driver_factory,
            config: runtime_channel,
            state: connection_state,
            status,
            metrics: ChannelMetrics::default(),
            health: None,
            created_at: now,
            last_activity: now,
        })
    }

    #[inline]
    /// Populate runtime indexes from a channel's topology triples without applying driver deltas.
    /// Assumes the channel instance is already inserted into `index.channels`.
    fn populate_indexes_from_topology(
        &self,
        dev_triples: Vec<DeviceInitTriple>,
    ) -> NGResult<(usize, usize, usize)> {
        let mut devices_ok = 0usize;
        let mut points_ok = 0usize;
        let mut actions_ok = 0usize;

        for (dev, pts, acts) in dev_triples.into_iter() {
            // Get channel instance to obtain factory and driver binding
            let channel = match self.index.channels.get(&dev.channel_id) {
                Some(c) => c,
                None => continue,
            };

            // Convert runtime device and bind to channel driver
            let runtime_device = match channel.driver_factory.convert_runtime_device(dev.into()) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            let device_id = runtime_device.id();
            let device_name: Arc<str> = Arc::from(runtime_device.device_name());
            let instance = DeviceInstance {
                config: Arc::clone(&runtime_device),
                state: DeviceState::Active,
                status: runtime_device.status(),
                driver: Arc::clone(&channel.driver),
                last_collection: None,
                last_data_change: None,
                metrics: DeviceMetrics::default(),
                created_at: Utc::now(),
            };

            // Insert device + mappings
            self.index.devices.insert(device_id, instance);
            self.add_device_to_channel(channel.config.id(), device_id);
            self.index
                .device_name_index
                .insert(Arc::clone(&device_name), device_id);
            devices_ok += 1;

            // Convert and insert points
            if !pts.is_empty() {
                let channel_name = channel.config.name();
                let mut rpoints = Vec::with_capacity(pts.len());
                for p in pts.into_iter() {
                    match channel.driver_factory.convert_runtime_point(p.into()) {
                        Ok(rp) => {
                            // Build unified point entry (point + meta) indexes.
                            self.upsert_point_entry(channel_name, &runtime_device, &rp, None);
                            rpoints.push(rp);
                        }
                        Err(e) => {
                            tracing::error!("Error converting point: {:?}", e);
                        }
                    }
                }
                if !rpoints.is_empty() {
                    points_ok += rpoints.len();
                    self.set_device_points(device_id, rpoints);
                }
            }

            // Convert and insert actions
            if !acts.is_empty() {
                let ractions = acts
                    .into_iter()
                    .filter_map(
                        |a| match channel.driver_factory.convert_runtime_action(a.into()) {
                            Ok(a) => Some(a),
                            Err(e) => {
                                tracing::error!("Error converting action: {:?}", e);
                                None
                            }
                        },
                    )
                    .collect::<Vec<Arc<dyn RuntimeAction>>>();
                if !ractions.is_empty() {
                    actions_ok += ractions.len();
                    self.set_device_actions(device_id, ractions);
                }
            }
        }

        Ok((devices_ok, points_ok, actions_ok))
    }

    /// Build init context using a prepared runtime channel (not yet committed).
    /// This allows preloading devices/points when initializing channels at gateway boot.
    #[inline]
    fn build_channel_runtime_context(
        &self,
        runtime_channel: Arc<dyn RuntimeChannel>,
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> SouthwardInitContext {
        let channel_id = runtime_channel.id();
        // Collect device ids already bound to this channel in indexes (if any)
        let device_ids: Vec<i32> = self.channel_device_ids(channel_id);

        let mut devices = Vec::with_capacity(device_ids.len());
        let mut points_by_device = HashMap::with_capacity(device_ids.len());

        for id in device_ids.into_iter() {
            if let Some(dev) = self.index.devices.get(&id) {
                devices.push(Arc::clone(&dev.config));
                let points_vec = self
                    .device_points_slice(id)
                    .map(|s| s.iter().cloned().collect())
                    .unwrap_or_default();
                points_by_device.insert(id, points_vec);
            }
        }

        SouthwardInitContext {
            devices,
            points_by_device,
            runtime_channel,
            publisher: Arc::new(MpscNorthwardPublisher::new(data_tx.clone())),
        }
    }

    /// Snapshot the driver for a channel.
    #[inline]
    fn snapshot_channel_driver(&self, channel_id: i32) -> Option<Arc<dyn Driver>> {
        self.index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver))
    }

    /// Snapshot device runtime, its driver and channel id by device id.
    #[inline]
    fn snapshot_device_and_driver(&self, device_id: i32) -> Option<DeviceDriverSnapshot> {
        let dev = self.index.devices.get(&device_id)?;
        let channel_id = dev.config.channel_id();
        let device = Arc::clone(&dev.config);
        drop(dev);
        let driver = self.snapshot_channel_driver(channel_id)?;
        Some((device, driver, channel_id))
    }

    /// Snapshot driver factory for a channel.
    #[inline]
    fn snapshot_driver_factory_for_channel(
        &self,
        channel_id: i32,
    ) -> Option<Arc<dyn DriverFactory>> {
        self.index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver_factory))
    }

    /// Snapshot driver factory for a device, returning also its channel id.
    #[inline]
    fn snapshot_driver_factory_for_device(
        &self,
        device_id: i32,
    ) -> Option<(Arc<dyn DriverFactory>, i32)> {
        let dev = self.index.devices.get(&device_id)?;
        let channel_id = dev.config.channel_id();
        drop(dev);
        let factory = self.snapshot_driver_factory_for_channel(channel_id)?;
        Some((factory, channel_id))
    }

    /// Start all channels by constructing runtime contexts and injecting a high-performance publisher
    pub async fn start_channels(&self, data_tx: &Sender<Arc<NorthwardData>>) -> NGResult<()> {
        // Collect channel ids first to avoid holding iter_mut guards across await
        let ids = self.get_enabled_channel_ids();
        // Spawn monitors up-front to capture transitions uniformly
        self.monitor.spawn_all(ids.clone(), data_tx);
        for id in ids.into_iter() {
            self.start_channel(id, data_tx, StartPolicy::AsyncFireAndForget)
                .await?;
        }
        Ok(())
    }

    /// Stop a channel's runtime, cancel its monitor, and optionally remove all runtime mappings
    pub async fn stop_channel(&self, channel_id: i32, remove: bool) {
        // Cancel per-channel monitor
        self.monitor.cancel(channel_id);

        // Stop driver if channel exists.
        //
        // IMPORTANT: never hold a DashMap guard across `.await`, otherwise we can deadlock if the
        // driver stop path indirectly touches the runtime index (or blocks other tasks that need
        // this shard).
        let driver = self
            .index
            .channels
            .get(&channel_id)
            .map(|instance| Arc::clone(&instance.driver));
        if let Some(driver) = driver {
            let _ = driver.stop().await;
        }

        if remove {
            if let Some((_, device_ids)) = self.index.channel_devices.remove(&channel_id) {
                for device_id in device_ids.iter().copied() {
                    if let Some((_, dev)) = self.index.devices.remove(&device_id) {
                        // Best-effort remove device name index
                        let name = dev.config.device_name();
                        self.index
                            .device_name_index
                            .remove_if(name, |_, v| *v == device_id);
                    }
                    self.index.device_points.remove(&device_id);
                    self.index.device_actions.remove(&device_id);
                }
            }
            // Remove channel entry itself after stopping
            self.index.channels.remove(&channel_id);
        }
    }

    /// Rebind all devices under a channel to the channel's current driver handle
    pub fn rebind_channel_devices(&self, channel_id: i32) {
        let new_driver = match self.index.channels.get(&channel_id) {
            Some(chan) => chan.driver.clone(),
            None => return,
        };
        if let Some(ids) = self
            .index
            .channel_devices
            .get(&channel_id)
            .map(|e| e.value().iter().copied().collect::<Vec<i32>>())
        {
            for device_id in ids.into_iter() {
                if let Some(mut dev) = self.index.devices.get_mut(&device_id) {
                    dev.driver = Arc::clone(&new_driver);
                }
            }
        }
    }

    /// Replace channel instance without starting (for Disabled channels or pre-start update)
    #[inline]
    pub async fn replace_channel_instance(
        &self,
        config: &ChannelModel,
        data_tx: &Sender<Arc<NorthwardData>>,
    ) -> NGResult<()> {
        let instance = self.create_channel_instance(config, data_tx).await?;
        self.index.channels.insert(instance.config.id(), instance);
        self.rebind_channel_devices(config.id);
        Ok(())
    }

    #[inline]
    /// Restart a channel atomically with new configuration
    pub async fn restart_channel(
        &self,
        config: &ChannelModel,
        data_tx: &Sender<Arc<NorthwardData>>,
        timeout_ms: u64,
    ) -> NGResult<()> {
        let channel_id = config.id;
        // Stop previous runtime and clean monitor/task entries
        self.stop_channel(channel_id, false).await;
        // Create and start synchronously (will insert and spawn monitor)
        self.create_and_start_channel(
            config,
            data_tx,
            StartPolicy::SyncWaitConnected { timeout_ms },
        )
        .await?;
        // Rebind devices to the new channel driver
        self.rebind_channel_devices(channel_id);
        Ok(())
    }

    #[inline]
    /// Create a single device instance with proper error handling
    fn create_device_instance(&self, device: DeviceModel) -> NGResult<(i32, DeviceInstance)> {
        // Get channel with proper error handling
        let channel =
            self.index
                .channels
                .get(&device.channel_id)
                .ok_or(NGError::InitializationError(format!(
                    "Channel {} not found for device: {}",
                    device.channel_id, device.device_name
                )))?;

        let device_id = device.id;

        // Convert to runtime device
        let runtime_device = channel
            .driver_factory
            .convert_runtime_device(device.into())
            .map_err(|e| {
                NGError::InitializationError(format!("Failed to convert device to runtime: {e}"))
            })?;

        let status = runtime_device.status();
        let instance = DeviceInstance {
            config: runtime_device,
            state: DeviceState::Active,
            status,
            driver: Arc::clone(&channel.driver),
            last_collection: None,
            last_data_change: None,
            metrics: DeviceMetrics::default(),
            created_at: Utc::now(),
        };

        Ok((device_id, instance))
    }

    #[inline]
    /// Get enabled channel instances
    pub fn get_collectable_channels(&self) -> Vec<i32> {
        self.index
            .channels
            .iter()
            .filter(|entry| {
                entry.value().status == Status::Enabled
                    && entry.value().config.collection_type() == CollectionType::Collection
            })
            .map(|entry| *entry.key())
            .collect()
    }

    /// List currently connected devices according to the provided subscription filter.
    ///
    /// This method snapshots the runtime indexes without holding long-lived locks,
    /// making it safe to call from high-throughput paths such as subscription updates.
    #[inline]
    pub fn list_connected_devices(
        &self,
        filter: SubscriptionFilter,
    ) -> Vec<ConnectedDeviceSnapshot> {
        match filter {
            SubscriptionFilter::AllDevices => self.collect_all_connected_devices(),
            SubscriptionFilter::DeviceIds(device_ids) => {
                self.collect_specific_connected_devices(device_ids)
            }
        }
    }

    /// Collect connected devices for all channels that are currently online.
    fn collect_all_connected_devices(&self) -> Vec<ConnectedDeviceSnapshot> {
        let connected_channels: Vec<(i32, Arc<str>, DateTime<Utc>)> = self
            .index
            .channels
            .iter()
            .filter(|entry| entry.value().state == SouthwardConnectionState::Connected)
            .map(|entry| {
                (
                    *entry.key(),
                    Arc::<str>::from(entry.value().config.name()),
                    entry.value().last_activity,
                )
            })
            .collect();

        let mut snapshots = Vec::with_capacity(connected_channels.len().saturating_mul(4));

        for (channel_id, channel_name, last_activity) in connected_channels.into_iter() {
            let Some(device_ids) = self.index.channel_devices.get(&channel_id) else {
                continue;
            };

            for device_id in device_ids.iter().copied() {
                let Some(device) = self.index.devices.get(&device_id) else {
                    continue;
                };
                snapshots.push(ConnectedDeviceSnapshot {
                    device_id,
                    device_name: Arc::<str>::from(device.config.device_name()),
                    device_type: Arc::<str>::from(device.config.device_type()),
                    channel_id,
                    channel_name: Arc::clone(&channel_name),
                    last_activity,
                });
            }
        }

        snapshots
    }

    /// Collect connected devices matching the supplied identifiers.
    ///
    /// The function deduplicates identifiers and filters out devices whose channels are
    /// not currently connected.
    fn collect_specific_connected_devices(
        &self,
        device_ids: Vec<i32>,
    ) -> Vec<ConnectedDeviceSnapshot> {
        if device_ids.is_empty() {
            return Vec::new();
        }

        let mut unique_ids = HashSet::with_capacity(device_ids.len());
        unique_ids.extend(device_ids);

        let mut snapshots = Vec::with_capacity(unique_ids.len());

        for device_id in unique_ids.into_iter() {
            let Some(device) = self.index.devices.get(&device_id) else {
                continue;
            };
            let channel_id = device.config.channel_id();
            let Some(channel) = self.index.channels.get(&channel_id) else {
                continue;
            };
            if channel.state != SouthwardConnectionState::Connected {
                continue;
            }

            snapshots.push(ConnectedDeviceSnapshot {
                device_id,
                device_name: Arc::<str>::from(device.config.device_name()),
                device_type: Arc::<str>::from(device.config.device_type()),
                channel_id,
                channel_name: Arc::<str>::from(channel.config.name()),
                last_activity: channel.last_activity,
            });
        }

        snapshots
    }

    #[inline]
    /// Get channel instance by ID
    pub fn get_channel(&self, channel_id: i32) -> Option<Ref<'_, i32, ChannelInstance>> {
        self.index.channels.get(&channel_id)
    }

    #[inline]
    /// Get all channel IDs
    pub fn get_channel_ids(&self) -> Vec<i32> {
        self.index
            .channels
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    /// Get enabled channel IDs
    pub fn get_enabled_channel_ids(&self) -> Vec<i32> {
        self.index
            .channels
            .iter()
            .filter(|entry| entry.value().status == Status::Enabled)
            .map(|entry| *entry.key())
            .collect()
    }

    #[inline]
    /// Check if channel is connected
    pub fn is_channel_connected(&self, channel_id: i32) -> bool {
        self.index
            .channels
            .get(&channel_id)
            .map(|entry| entry.state == SouthwardConnectionState::Connected)
            .unwrap_or(false)
    }

    #[inline]
    /// Check if channel is collectable
    pub fn is_channel_collectable(&self, channel_id: i32) -> bool {
        self.index
            .channels
            .get(&channel_id)
            .map(|entry| {
                entry.status == Status::Enabled
                    && entry.config.collection_type() == CollectionType::Collection
                    && entry.state == SouthwardConnectionState::Connected
            })
            .unwrap_or(false)
    }

    #[inline]
    /// Get device instance by ID
    pub fn get_device(&self, device_id: i32) -> Option<Ref<'_, i32, DeviceInstance>> {
        self.index.devices.get(&device_id)
    }

    /// Update device data snapshot and filter changes based on ReportType
    ///
    /// This method:
    /// 1. Gets the ReportType from the device's channel
    /// 2. If ReportType::Change, compares new data with existing snapshot to detect changes
    /// 3. Updates the snapshot with latest values
    /// 4. Returns filtered data (only changed values for ReportType::Change, or full data for ReportType::Always)
    ///
    /// # Arguments
    /// * `data` - Arc-wrapped NorthwardData to process
    ///
    /// # Returns
    /// * `Some(Arc<NorthwardData>)` - Filtered data (only changed values for ReportType::Change)
    /// * `None` - No changes detected (ReportType::Change with no changes)
    pub fn update_and_filter_device_snapshot(
        &self,
        mut data: Arc<NorthwardData>,
    ) -> Option<Arc<NorthwardData>> {
        let device_id = data.device_id();
        let now = Utc::now();

        // Get device to find channel_id
        let device = match self.get_device(device_id) {
            Some(d) => d,
            None => {
                // Device not found, pass through
                return Some(data);
            }
        };

        let channel_id = device.config.channel_id();

        // Get channel to find ReportType
        let report_type = match self.get_channel(channel_id) {
            Some(channel) => channel.config.report_type(),
            None => {
                // Channel not found, pass through
                return Some(data);
            }
        };

        // If ReportType::Always, update snapshot and return full data
        if matches!(report_type, ReportType::Always) {
            self.update_snapshot_internal(data.as_ref(), device_id, now);
            return Some(data);
        }

        // ReportType::Change - detect changes and filter (in-place when possible).
        //
        // # Performance
        // - `Arc::make_mut` avoids cloning when `data` has strong_count == 1 (common in routing path).
        // - Filtering uses `Vec::retain` to move elements in-place, removing the need to clone
        //   `PointValue` on the hottest path.
        let data_mut = Arc::make_mut(&mut data);
        match data_mut {
            NorthwardData::Telemetry(telemetry) => {
                if self.filter_telemetry_changes_in_place(device_id, telemetry, now) {
                    Some(data)
                } else {
                    None
                }
            }
            NorthwardData::Attributes(attributes) => {
                if self.filter_attributes_changes_in_place(device_id, attributes, now) {
                    Some(data)
                } else {
                    None
                }
            }
            _ => {
                // Other data types don't update snapshots, pass through
                Some(data)
            }
        }
    }

    /// Internal helper to update snapshot without filtering
    fn update_snapshot_internal(&self, data: &NorthwardData, device_id: i32, now: DateTime<Utc>) {
        match data {
            NorthwardData::Telemetry(telemetry) => {
                self.device_snapshots
                    .entry(device_id)
                    .and_modify(|snapshot| {
                        // Update in-place to avoid cloning large maps.
                        for pv in telemetry.values.iter() {
                            snapshot.telemetry.insert(pv.point_id, pv.value.clone());
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        snapshot.last_update = now;
                    })
                    .or_insert_with(|| {
                        let mut telemetry_map =
                            HashMap::with_capacity(telemetry.values.len().saturating_mul(2));
                        let mut point_key_by_id =
                            HashMap::with_capacity(telemetry.values.len().saturating_mul(2));
                        for pv in telemetry.values.iter() {
                            telemetry_map.insert(pv.point_id, pv.value.clone());
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        DeviceDataSnapshot {
                            device_id,
                            device_name: Arc::<str>::from(telemetry.device_name.as_str()),
                            telemetry: telemetry_map,
                            client_attributes: HashMap::new(),
                            shared_attributes: HashMap::new(),
                            server_attributes: HashMap::new(),
                            point_key_by_id,
                            last_update: now,
                        }
                    });
            }
            NorthwardData::Attributes(attributes) => {
                self.device_snapshots
                    .entry(device_id)
                    .and_modify(|snapshot| {
                        for pv in attributes.client_attributes.iter() {
                            snapshot
                                .client_attributes
                                .insert(pv.point_id, pv.value.clone());
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        for pv in attributes.shared_attributes.iter() {
                            snapshot
                                .shared_attributes
                                .insert(pv.point_id, pv.value.clone());
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        for pv in attributes.server_attributes.iter() {
                            snapshot
                                .server_attributes
                                .insert(pv.point_id, pv.value.clone());
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        snapshot.last_update = now;
                    })
                    .or_insert_with(|| {
                        let mut client = HashMap::with_capacity(attributes.client_attributes.len());
                        let mut shared = HashMap::with_capacity(attributes.shared_attributes.len());
                        let mut server = HashMap::with_capacity(attributes.server_attributes.len());
                        let mut point_key_by_id = HashMap::with_capacity(
                            (attributes.client_attributes.len()
                                + attributes.shared_attributes.len()
                                + attributes.server_attributes.len())
                            .saturating_mul(2),
                        );
                        for pv in attributes.client_attributes.iter() {
                            client.insert(pv.point_id, pv.value.clone());
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        for pv in attributes.shared_attributes.iter() {
                            shared.insert(pv.point_id, pv.value.clone());
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        for pv in attributes.server_attributes.iter() {
                            server.insert(pv.point_id, pv.value.clone());
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        DeviceDataSnapshot {
                            device_id,
                            device_name: Arc::<str>::from(attributes.device_name.as_str()),
                            telemetry: HashMap::new(),
                            client_attributes: client,
                            shared_attributes: shared,
                            server_attributes: server,
                            point_key_by_id,
                            last_update: now,
                        }
                    });
            }
            _ => {
                // Other data types don't update snapshots
            }
        }
    }

    /// Upsert point key mapping for a point id.
    ///
    /// # Performance
    /// Uses `HashMap::entry` to avoid unnecessary `Arc` clones and value replacements
    /// when the point key is unchanged (common case).
    #[inline]
    fn upsert_point_key_by_id(
        map: &mut HashMap<i32, Arc<str>>,
        point_id: i32,
        point_key: &Arc<str>,
    ) {
        match map.entry(point_id) {
            Entry::Vacant(v) => {
                v.insert(Arc::clone(point_key));
            }
            Entry::Occupied(mut o) => {
                // Point keys are expected to be stable, but may change due to reconfiguration.
                // Only replace when the string differs.
                if o.get().as_ref() != point_key.as_ref() {
                    o.insert(Arc::clone(point_key));
                }
            }
        }
    }

    /// Filter telemetry changes using device snapshot (in-place).
    ///
    /// Returns `true` if there are changed points (`telemetry.values` is mutated to contain only
    /// changed points), otherwise `false`.
    fn filter_telemetry_changes_in_place(
        &self,
        device_id: i32,
        telemetry: &mut TelemetryData,
        now: DateTime<Utc>,
    ) -> bool {
        // First pass: detect changes using a single read guard (early return if no changes).
        let existing_snapshot = self.device_snapshots.get(&device_id);
        let existing_telemetry = existing_snapshot.as_ref().map(|s| &s.telemetry);
        telemetry.values.retain(|pv| match existing_telemetry {
            Some(existing) => existing
                .get(&pv.point_id)
                .map(|old_value| old_value != &pv.value)
                .unwrap_or(true), // First time seeing this point_id
            None => true, // No snapshot exists
        });

        if telemetry.values.is_empty() {
            return false;
        }

        // Release read guard before write path.
        drop(existing_snapshot);

        // Update snapshot: only write changed points to minimize hash ops on hot path.
        self.device_snapshots
            .entry(device_id)
            .and_modify(|snapshot| {
                for pv in telemetry.values.iter() {
                    snapshot.telemetry.insert(pv.point_id, pv.value.clone());
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                snapshot.last_update = now;
            })
            .or_insert_with(|| {
                let mut telemetry_map =
                    HashMap::with_capacity(telemetry.values.len().saturating_mul(2));
                let mut point_key_by_id =
                    HashMap::with_capacity(telemetry.values.len().saturating_mul(2));
                for pv in telemetry.values.iter() {
                    telemetry_map.insert(pv.point_id, pv.value.clone());
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                DeviceDataSnapshot {
                    device_id,
                    device_name: Arc::<str>::from(telemetry.device_name.as_str()),
                    telemetry: telemetry_map,
                    client_attributes: HashMap::new(),
                    shared_attributes: HashMap::new(),
                    server_attributes: HashMap::new(),
                    point_key_by_id,
                    last_update: now,
                }
            });
        true
    }

    /// Filter attributes changes using device snapshot (in-place).
    ///
    /// Returns `true` if there are changed points (attribute vectors are mutated to contain only
    /// changed points), otherwise `false`.
    fn filter_attributes_changes_in_place(
        &self,
        device_id: i32,
        attributes: &mut AttributeData,
        now: DateTime<Utc>,
    ) -> bool {
        let existing_snapshot = self.device_snapshots.get(&device_id);
        let snapshot_ref = existing_snapshot.as_ref();

        attributes
            .client_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .client_attributes
                    .get(&pv.point_id)
                    .map(|old_value| old_value != &pv.value)
                    .unwrap_or(true),
                None => true,
            });
        attributes
            .shared_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .shared_attributes
                    .get(&pv.point_id)
                    .map(|old_value| old_value != &pv.value)
                    .unwrap_or(true),
                None => true,
            });
        attributes
            .server_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .server_attributes
                    .get(&pv.point_id)
                    .map(|old_value| old_value != &pv.value)
                    .unwrap_or(true),
                None => true,
            });

        if attributes.client_attributes.is_empty()
            && attributes.shared_attributes.is_empty()
            && attributes.server_attributes.is_empty()
        {
            return false;
        }

        drop(existing_snapshot);

        // Update snapshot with only changed attributes.
        self.device_snapshots
            .entry(device_id)
            .and_modify(|snapshot| {
                for pv in attributes.client_attributes.iter() {
                    snapshot
                        .client_attributes
                        .insert(pv.point_id, pv.value.clone());
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                for pv in attributes.shared_attributes.iter() {
                    snapshot
                        .shared_attributes
                        .insert(pv.point_id, pv.value.clone());
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                for pv in attributes.server_attributes.iter() {
                    snapshot
                        .server_attributes
                        .insert(pv.point_id, pv.value.clone());
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                snapshot.last_update = now;
            })
            .or_insert_with(|| {
                // No existing snapshot: seed from incoming values so subsequent Change reports work.
                let mut client = HashMap::with_capacity(attributes.client_attributes.len());
                let mut shared = HashMap::with_capacity(attributes.shared_attributes.len());
                let mut server = HashMap::with_capacity(attributes.server_attributes.len());
                let mut point_key_by_id = HashMap::with_capacity(
                    (attributes.client_attributes.len()
                        + attributes.shared_attributes.len()
                        + attributes.server_attributes.len())
                    .saturating_mul(2),
                );
                for pv in attributes.client_attributes.iter() {
                    client.insert(pv.point_id, pv.value.clone());
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                for pv in attributes.shared_attributes.iter() {
                    shared.insert(pv.point_id, pv.value.clone());
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                for pv in attributes.server_attributes.iter() {
                    server.insert(pv.point_id, pv.value.clone());
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                DeviceDataSnapshot {
                    device_id,
                    device_name: Arc::<str>::from(attributes.device_name.as_str()),
                    telemetry: HashMap::new(),
                    client_attributes: client,
                    shared_attributes: shared,
                    server_attributes: server,
                    point_key_by_id,
                    last_update: now,
                }
            });
        true
    }

    /// Get device data snapshot by device ID
    ///
    /// Returns the latest snapshot of telemetry and attribute values for the device.
    /// Returns None if no snapshot exists (device has never reported data).
    #[inline]
    pub fn get_device_snapshot(&self, device_id: i32) -> Option<DeviceDataSnapshot> {
        self.device_snapshots
            .get(&device_id)
            .map(|e| e.value().clone())
    }

    #[inline]
    /// Get runtime state for a specific device, if present in the index.
    ///
    /// This is primarily used by monitoring APIs to filter online/active devices.
    pub fn get_device_state(&self, device_id: i32) -> Option<DeviceState> {
        self.index.devices.get(&device_id).map(|entry| entry.state)
    }

    #[inline]
    /// Get the channel id for a specific device, if present in the index.
    pub fn get_device_channel_id(&self, device_id: i32) -> Option<i32> {
        self.index
            .devices
            .get(&device_id)
            .map(|entry| entry.config.channel_id())
    }

    /// Get point metadata by `point_id`.
    ///
    /// # Performance
    /// This is an **O(1)** lookup backed by `DashMap` and returns `Arc<PointMeta>` so clones are cheap.
    ///
    /// # Notes
    /// This method is primarily intended for monitoring and encoding layers that need to map
    /// internal `point_id` keys to stable external identifiers (e.g. `point_key`).
    #[inline]
    pub fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>> {
        self.index.point_entries_by_id.get(&point_id).map(|e| {
            let entry = e.value();
            Arc::clone(&entry.meta)
        })
    }

    /// Get runtime point by `point_id` (O(1)).
    #[inline]
    pub fn get_runtime_point(&self, point_id: i32) -> Option<Arc<dyn RuntimePoint>> {
        self.index.point_entries_by_id.get(&point_id).map(|e| {
            let entry = e.value();
            Arc::clone(&entry.point)
        })
    }

    /// Get both `PointMeta` and runtime point with a single DashMap lookup (hot-path helper).
    #[inline]
    pub fn get_point_entry(
        &self,
        point_id: i32,
    ) -> Option<(Arc<PointMeta>, Arc<dyn RuntimePoint>)> {
        self.index.point_entries_by_id.get(&point_id).map(|e| {
            let entry = e.value();
            (Arc::clone(&entry.meta), Arc::clone(&entry.point))
        })
    }

    #[inline]
    /// Find device id by name
    pub fn find_device_id_by_name(&self, device_name: &str) -> Option<i32> {
        self.index
            .device_name_index
            .get(device_name)
            .map(|entry| *entry.value())
    }

    #[inline]
    /// Get collectable device IDs for a specific channel
    pub fn get_channel_collectable_device_ids(&self, channel_id: i32) -> Vec<i32> {
        self.index
            .channel_devices
            .get(&channel_id)
            .map(|entry| entry.value().iter().copied().collect::<Vec<i32>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|device_id| {
                self.index
                    .devices
                    .get(device_id)
                    .map(|dev| dev.status == Status::Enabled)
                    .unwrap_or(false)
            })
            .collect()
    }

    #[inline]
    /// Get points for a specific device
    pub fn get_device_points(&self, device_id: i32) -> Arc<[Arc<dyn RuntimePoint>]> {
        self.device_points_slice(device_id)
            .unwrap_or_else(|| Arc::from(Vec::<Arc<dyn RuntimePoint>>::new().into_boxed_slice()))
    }

    #[inline]
    /// Get actions for a specific device
    pub fn get_device_actions(&self, device_id: i32) -> Arc<[Arc<dyn RuntimeAction>]> {
        self.device_actions_slice(device_id)
            .unwrap_or_else(|| Arc::from(Vec::<Arc<dyn RuntimeAction>>::new().into_boxed_slice()))
    }

    #[inline]
    /// Get readable points for a device
    pub fn get_readable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.get_device_points(device_id)
            .iter()
            .filter(|&dp| matches!(dp.access_mode(), AccessMode::Read | AccessMode::ReadWrite))
            .cloned()
            .collect()
    }

    #[inline]
    /// Get writable points for a device
    pub fn get_writable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.get_device_points(device_id)
            .iter()
            .filter(|&dp| matches!(dp.access_mode(), AccessMode::Write | AccessMode::ReadWrite))
            .cloned()
            .collect()
    }

    /// Update internal metrics
    async fn update_metrics(&self) {
        let mut metrics = self.metrics.lock().await;

        metrics.total_channels = self.index.channels.len();
        metrics.total_devices = self.index.devices.len();

        metrics.connected_channels = self
            .index
            .channels
            .iter()
            .filter(|entry| entry.state == SouthwardConnectionState::Connected)
            .count();

        metrics.active_devices = self
            .index
            .devices
            .iter()
            .filter(|entry| entry.state == DeviceState::Active)
            .count();

        metrics.total_data_points = self
            .index
            .device_points
            .iter()
            .map(|entry| entry.value().len())
            .sum();

        metrics.total_actions = self
            .index
            .device_actions
            .iter()
            .map(|entry| entry.value().len())
            .sum();

        metrics.average_points_per_device = if metrics.total_devices > 0 {
            metrics.total_data_points as f64 / metrics.total_devices as f64
        } else {
            0.0
        };

        metrics.last_update = Some(Utc::now());
    }

    /// Get manager metrics
    pub async fn get_metrics(&self) -> SouthwardManagerMetrics {
        self.update_metrics().await;
        self.metrics.lock().await.clone()
    }

    /// Shutdown the data manager
    pub async fn shutdown(&self) -> NGResult<()> {
        // Stop all channels concurrently using unified stop logic
        let ids = self.get_channel_ids();
        let futures_iter = ids.into_iter().map(|id| self.stop_channel(id, true));
        let _ = timeout(Duration::from_secs(6), join_all(futures_iter)).await;

        // Clear caches and mappings
        self.index.device_points.clear();
        self.index.point_entries_by_id.clear();
        self.index.point_id_by_path.clear();
        self.index.device_actions.clear();
        self.index.channel_devices.clear();
        self.index.device_name_index.clear();
        self.index.devices.clear();
        self.index.channels.clear();
        self.device_snapshots.clear();
        self.monitor.cancel_all();

        Ok(())
    }
}

// Implement the trait for accessing connection states
impl SouthwardManager for NGSouthwardManager {
    fn get_channel_connection_state(&self, channel_id: i32) -> Option<SouthwardConnectionState> {
        self.index
            .channels
            .get(&channel_id)
            .map(|entry| entry.state.clone())
    }
}

impl NGSouthwardManager {
    /// Add many devices into runtime tables of a channel; revert memory on driver error.
    pub async fn add_devices(&self, channel_id: i32, devices: Vec<DeviceModel>) -> NGResult<()> {
        if devices.is_empty() {
            return Ok(());
        }

        // Ensure channel exists; driver will be fetched later via a short-lived guard
        if !self.index.channels.contains_key(&channel_id) {
            return Ok(());
        }

        // Build instances; skip conversions that fail to avoid poisoning the batch
        let instances: Vec<(i32, DeviceInstance)> = devices
            .into_iter()
            .filter_map(|d| match self.create_device_instance(d) {
                Ok(instance) => Some(instance),
                Err(e) => {
                    tracing::error!("Error creating device instance: {:?}", e);
                    None
                }
            })
            .collect();
        if instances.is_empty() {
            return Ok(());
        }

        let driver = match self.snapshot_channel_driver(channel_id) {
            Some(d) => d,
            None => return Ok(()),
        };

        struct DevicesAddedRecord {
            added_ids: Vec<i32>,
            added_names: Vec<(Arc<str>, i32)>,
            added_cfgs: Vec<Arc<dyn RuntimeDevice>>,
        }

        apply_with_revert(
            || {
                let mut added_ids = Vec::with_capacity(instances.len());
                let mut added_names = Vec::with_capacity(instances.len());
                let mut added_cfgs = Vec::with_capacity(instances.len());

                for (device_id, instance) in instances.iter() {
                    let device_name: Arc<str> = Arc::from(instance.config.device_name());
                    self.index.devices.insert(*device_id, instance.clone());
                    self.add_device_to_channel(channel_id, *device_id);
                    if let Some(prev) = self
                        .index
                        .device_name_index
                        .insert(Arc::clone(&device_name), *device_id)
                    {
                        if prev != *device_id {
                            warn!(
                                "Device name index overwritten: old_id={prev}, new_id={device_id}"
                            );
                        }
                    }
                    added_cfgs.push(Arc::clone(&instance.config));
                    added_ids.push(*device_id);
                    added_names.push((device_name, *device_id));
                }
                DevicesAddedRecord {
                    added_ids,
                    added_names,
                    added_cfgs,
                }
            },
            |rec| RuntimeDelta::DevicesChanged {
                added: rec.added_cfgs.clone(),
                updated: Vec::new(),
                removed: Vec::new(),
                status_changed: Vec::new(),
            },
            |delta| async {
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                for id in rec.added_ids.iter().copied() {
                    self.remove_device_from_channel(channel_id, id);
                }
                for (name, id) in rec.added_names.into_iter() {
                    self.index
                        .device_name_index
                        .remove_if(&name, |_, v| *v == id);
                    self.index.devices.remove(&id);
                }
            },
        )
        .await
    }

    /// Replace (upsert) many devices under a specific channel; revert memory on driver error.
    pub async fn replace_devices(
        &self,
        channel_id: i32,
        devices: Vec<DeviceModel>,
    ) -> NGResult<()> {
        if devices.is_empty() {
            return Ok(());
        }

        if !self.index.channels.contains_key(&channel_id) {
            return Ok(());
        }

        // Build instances; ensure all belong to the target channel
        let group = devices
            .into_iter()
            .filter_map(|d| match self.create_device_instance(d) {
                Ok(instance) => Some(instance),
                Err(e) => {
                    tracing::error!("Error creating device instance: {:?}", e);
                    None
                }
            })
            .collect::<Vec<_>>();

        let driver = match self.snapshot_channel_driver(channel_id) {
            Some(d) => d,
            None => return Ok(()),
        };

        struct DevicesReplaceRecord {
            added: Vec<Arc<dyn RuntimeDevice>>,
            updated: Vec<Arc<dyn RuntimeDevice>>,
            added_ids: Vec<i32>,
            updated_snapshots: Vec<(i32, DeviceInstance, Arc<str>)>,
        }

        apply_with_revert(
            || {
                // Prepare added/updated and snapshots for revert
                let mut added = Vec::new();
                let mut updated = Vec::new();
                let mut added_ids: Vec<i32> = Vec::new();
                let mut updated_snapshots = Vec::new();

                for (device_id, instance) in group.iter() {
                    let new_name: Arc<str> = Arc::from(instance.config.device_name());
                    // Get old device info and explicitly drop the read lock before insert
                    let old_name = {
                        let old_ref = self.index.devices.get(device_id);
                        let result = old_ref.as_ref().map(|old| {
                            let name: Arc<str> = Arc::from(old.config.device_name());
                            updated_snapshots.push((
                                *device_id,
                                old.value().clone(),
                                Arc::clone(&name),
                            ));
                            name
                        });
                        // Explicitly drop the read lock before any write operations
                        drop(old_ref);
                        result
                    };

                    if let Some(old_name) = old_name {
                        if old_name != new_name {
                            self.index
                                .device_name_index
                                .remove_if(&old_name, |_, v| *v == *device_id);
                            self.index
                                .device_name_index
                                .insert(Arc::clone(&new_name), *device_id);
                        }
                        // Replace device
                        self.index.devices.insert(*device_id, instance.clone());
                        updated.push(Arc::clone(&instance.config));
                    } else {
                        // Insert new
                        self.index.devices.insert(*device_id, instance.clone());
                        self.index
                            .device_name_index
                            .insert(Arc::clone(&new_name), *device_id);
                        self.add_device_to_channel(channel_id, *device_id);
                        added.push(Arc::clone(&instance.config));
                        added_ids.push(*device_id);
                    }
                }

                DevicesReplaceRecord {
                    added,
                    updated,
                    added_ids,
                    updated_snapshots,
                }
            },
            |rec| RuntimeDelta::DevicesChanged {
                added: rec.added.clone(),
                updated: rec.updated.clone(),
                removed: Vec::new(),
                status_changed: Vec::new(),
            },
            |delta| async {
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                // Revert: remove added; restore updated snapshots and mappings
                for id in rec.added_ids.into_iter() {
                    if let Some((_, inst)) = self.index.devices.remove(&id) {
                        let name: Arc<str> = Arc::from(inst.config.device_name());
                        self.index
                            .device_name_index
                            .remove_if(&name, |_, v| *v == id);
                        self.remove_device_from_channel(channel_id, id);
                    }
                }
                for (id, old_inst, old_name) in rec.updated_snapshots.into_iter() {
                    self.index.devices.insert(id, old_inst.clone());
                    // Get current name and explicitly drop read lock before insert
                    let current_name = {
                        let name_ref = self.index.device_name_index.get(&old_name);
                        let result = name_ref.as_ref().map(|e| *e.value()).unwrap_or_default();
                        drop(name_ref); // Explicitly drop read lock before write
                        result
                    };
                    if current_name != id {
                        self.index.device_name_index.insert(old_name.clone(), id);
                    }
                }
            },
        )
        .await
    }

    /// Remove many devices under a specific channel; optionally keep children; revert memory on driver error.
    pub async fn remove_devices(
        &self,
        channel_id: i32,
        ids: Vec<i32>,
        preserve_children: bool,
    ) -> NGResult<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let driver = match self.snapshot_channel_driver(channel_id) {
            Some(d) => d,
            None => return Ok(()),
        };

        struct DevicesRemovedRecord {
            removed_devices: HashMap<i32, DeviceInstance>,
            _removed_names: Vec<(Arc<str>, i32)>,
            removed_children_points: HashMap<i32, Vec<Arc<dyn RuntimePoint>>>,
            removed_children_actions: HashMap<i32, Vec<Arc<dyn RuntimeAction>>>,
            removed_runtime_cfgs: Vec<(i32, Arc<dyn RuntimeDevice>)>,
        }

        apply_with_revert(
            || {
                // Snapshots for revert
                let mut removed_devices = HashMap::new();
                let mut removed_names = Vec::new();
                let mut removed_children_points = HashMap::new();
                let mut removed_children_actions = HashMap::new();
                let mut removed_runtime: Vec<(i32, Arc<dyn RuntimeDevice>)> = Vec::new();

                for id in ids.iter().copied() {
                    if let Some((_, dev)) = self.index.devices.remove(&id) {
                        removed_runtime.push((id, Arc::clone(&dev.config)));

                        // remove name index and channel mapping
                        let name: Arc<str> = Arc::from(dev.config.device_name());
                        self.index
                            .device_name_index
                            .remove_if(&name, |_, v| *v == id);
                        self.remove_device_from_channel(channel_id, id);

                        // snapshot for revert
                        removed_names.push((name, id));
                        removed_devices.insert(id, dev.clone());

                        if !preserve_children {
                            if let Some((_, points)) = self.index.device_points.remove(&id) {
                                let points_vec: Vec<Arc<dyn RuntimePoint>> =
                                    points.iter().cloned().collect();
                                for p in points_vec.iter() {
                                    self.remove_point_entry_by_id(p.id());
                                }
                                removed_children_points.insert(id, points_vec);
                            }
                            if let Some((_, actions)) = self.index.device_actions.remove(&id) {
                                let actions_vec: Vec<Arc<dyn RuntimeAction>> =
                                    actions.iter().cloned().collect();
                                removed_children_actions.insert(id, actions_vec);
                            }
                        }
                    }
                }

                DevicesRemovedRecord {
                    removed_devices,
                    _removed_names: removed_names,
                    removed_children_points,
                    removed_children_actions,
                    removed_runtime_cfgs: removed_runtime,
                }
            },
            |rec| {
                if rec.removed_runtime_cfgs.is_empty() {
                    return None;
                }
                let removed = rec
                    .removed_runtime_cfgs
                    .iter()
                    .map(|(_, cfg)| Arc::clone(cfg))
                    .collect();
                Some(RuntimeDelta::DevicesChanged {
                    added: Vec::new(),
                    updated: Vec::new(),
                    removed,
                    status_changed: Vec::new(),
                })
            },
            |maybe_delta| async {
                if let Some(delta) = maybe_delta {
                    driver
                        .apply_runtime_delta(delta)
                        .await
                        .map_err(|e| NGError::DriverError(e.to_string()))
                } else {
                    Ok(())
                }
            },
            |mut rec| {
                // Revert these ids
                for (id, old) in rec.removed_devices.drain() {
                    let ch = old.config.channel_id();
                    let name: Arc<str> = Arc::from(old.config.device_name());
                    let channel_name = self
                        .index
                        .channels
                        .get(&ch)
                        .map(|c| c.config.name().to_string())
                        .unwrap_or_default();
                    self.index.devices.insert(id, old.clone());
                    self.index.device_name_index.insert(Arc::clone(&name), id);
                    self.add_device_to_channel(ch, id);
                    if !preserve_children {
                        if let Some(points) = rec.removed_children_points.remove(&id) {
                            for p in points.iter() {
                                self.upsert_point_entry(&channel_name, &old.config, p, None);
                            }
                            self.set_device_points(id, points);
                        }
                        if let Some(actions) = rec.removed_children_actions.remove(&id) {
                            self.set_device_actions(id, actions);
                        }
                    }
                }
            },
        )
        .await
    }

    /// Add runtime points to a device and wait for driver to apply. On failure, revert in-memory changes.
    pub async fn add_points(&self, device_id: i32, points: Vec<PointModel>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        // Snapshot factory and device/driver
        let (factory, _channel_id) = match self.snapshot_driver_factory_for_device(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let channel_name = self
            .get_channel(device.channel_id())
            .map(|c| c.config.name().to_string())
            .unwrap_or_default();

        // Convert input models to runtime points
        let converted = points
            .into_iter()
            .filter_map(|rp| match factory.convert_runtime_point(rp.into()) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!("Error converting point: {:?}", e);
                    None
                }
            })
            .collect::<Vec<Arc<dyn RuntimePoint>>>();
        if converted.is_empty() {
            return Ok(());
        }

        struct PointsAddedRecord {
            added: Vec<Arc<dyn RuntimePoint>>,
            added_ids: Vec<i32>,
        }

        apply_with_revert(
            || {
                let mut added = Vec::with_capacity(converted.len());
                let mut to_push: Vec<Arc<dyn RuntimePoint>> = Vec::with_capacity(converted.len());
                for rp in converted.into_iter() {
                    self.upsert_point_entry(&channel_name, &device, &rp, None);
                    added.push(Arc::clone(&rp));
                    to_push.push(rp);
                }
                // Atomically append all points under one entry lock to avoid lost updates.
                self.mutate_device_points(device_id, |v| v.extend(to_push.into_iter()));

                let added_ids = added.iter().map(|rp| rp.id()).collect::<Vec<i32>>();
                PointsAddedRecord { added, added_ids }
            },
            |rec| {
                let added = rec.added.clone();
                RuntimeDelta::PointsChanged {
                    device: Arc::clone(&device),
                    added,
                    updated: Vec::new(),
                    removed: Vec::new(),
                }
            },
            |delta| async {
                self.broadcast_runtime_delta(delta.clone());
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                self.mutate_device_points(device_id, |v| {
                    v.retain(|p| !rec.added_ids.iter().any(|id| *id == p.id()))
                });
                for id in rec.added_ids.iter() {
                    self.remove_point_entry_by_id(*id);
                }
            },
        )
        .await
    }

    /// Replace (upsert) runtime points on a device by id; wait for driver and revert on failure.
    pub async fn replace_points(&self, device_id: i32, points: Vec<PointModel>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        // Snapshot channel_id and factory via helper
        let (factory, _channel_id) = match self.snapshot_driver_factory_for_device(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let channel_name = self
            .get_channel(device.channel_id())
            .map(|c| c.config.name().to_string())
            .unwrap_or_default();

        // Convert models
        let rps = points
            .into_iter()
            .filter_map(|rp| match factory.convert_runtime_point(rp.into()) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!("Error converting point: {:?}", e);
                    None
                }
            })
            .collect::<Vec<Arc<dyn RuntimePoint>>>();
        if rps.is_empty() {
            return Ok(());
        }

        struct PointsReplaceRecord {
            added: Vec<Arc<dyn RuntimePoint>>,
            updated: Vec<Arc<dyn RuntimePoint>>,
            added_ids: Vec<i32>,
            replaced_old: Vec<(i32, Arc<dyn RuntimePoint>)>,
        }

        apply_with_revert(
            || {
                let mut added = Vec::new();
                let mut updated = Vec::new();
                let mut replaced_old = Vec::new();

                for rp in rps.iter() {
                    self.upsert_point_entry(&channel_name, &device, rp, None);
                }
                self.mutate_device_points(device_id, |current| {
                    for rp in rps.into_iter() {
                        if let Some(pos) = current.iter().position(|p| p.id() == rp.id()) {
                            let old = Arc::clone(&current[pos]);
                            replaced_old.push((old.id(), old));
                            current[pos] = Arc::clone(&rp);
                            updated.push(rp);
                        } else {
                            current.push(Arc::clone(&rp));
                            added.push(rp);
                        }
                    }
                });
                let added_ids = added.iter().map(|rp| rp.id()).collect::<Vec<i32>>();
                PointsReplaceRecord {
                    added,
                    updated,
                    added_ids,
                    replaced_old,
                }
            },
            |rec| {
                let added = rec.added.clone();
                let updated = rec.updated.clone();
                RuntimeDelta::PointsChanged {
                    device: Arc::clone(&device),
                    added,
                    updated,
                    removed: Vec::new(),
                }
            },
            |delta| async {
                self.broadcast_runtime_delta(delta.clone());
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                let replaced_old = rec.replaced_old.clone();
                self.mutate_device_points(device_id, |current| {
                    current.retain(|p| !rec.added_ids.iter().any(|id| *id == p.id()));
                    for (id, old) in replaced_old.iter().cloned() {
                        if let Some(pos) = current.iter().position(|p| p.id() == id) {
                            current[pos] = old;
                        } else {
                            current.push(old);
                        }
                    }
                });
                for id in rec.added_ids.iter() {
                    self.remove_point_entry_by_id(*id);
                }
                for (_id, old) in replaced_old.iter() {
                    self.upsert_point_entry(&channel_name, &device, old, None);
                }
            },
        )
        .await
    }

    /// Remove runtime points by id and wait for driver; revert in-memory on failure.
    pub async fn remove_points(&self, device_id: i32, point_ids: Vec<i32>) -> NGResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let channel_name = self
            .get_channel(device.channel_id())
            .map(|c| c.config.name().to_string())
            .unwrap_or_default();

        struct PointsRemovedRecord {
            removed: Vec<Arc<dyn RuntimePoint>>,
        }

        apply_with_revert(
            || {
                let mut removed = Vec::new();
                self.mutate_device_points(device_id, |current| {
                    let mut kept: Vec<Arc<dyn RuntimePoint>> = Vec::with_capacity(current.len());
                    for p in current.drain(..) {
                        if point_ids.iter().any(|id| *id == p.id()) {
                            removed.push(Arc::clone(&p));
                        } else {
                            kept.push(p);
                        }
                    }
                    *current = kept;
                });
                for p in removed.iter() {
                    self.remove_point_entry_by_id(p.id());
                }
                PointsRemovedRecord { removed }
            },
            |rec| {
                if rec.removed.is_empty() {
                    return None;
                }
                let removed = rec.removed.clone();
                Some(RuntimeDelta::PointsChanged {
                    device: Arc::clone(&device),
                    added: Vec::new(),
                    updated: Vec::new(),
                    removed,
                })
            },
            |maybe_delta| async {
                if let Some(delta) = maybe_delta {
                    self.broadcast_runtime_delta(delta.clone());
                    driver
                        .apply_runtime_delta(delta)
                        .await
                        .map_err(|e| NGError::DriverError(e.to_string()))
                } else {
                    Ok(())
                }
            },
            |rec| {
                if !rec.removed.is_empty() {
                    self.mutate_device_points(device_id, |current| {
                        current.extend(rec.removed.into_iter())
                    });
                    // Restore metadata entries best-effort.
                    if let Some(points) = self.device_points_slice(device_id) {
                        for p in points.iter() {
                            if point_ids.iter().any(|id| *id == p.id()) {
                                self.upsert_point_entry(&channel_name, &device, p, None);
                            }
                        }
                    }
                }
            },
        )
        .await
    }

    /// Add actions to a device and wait for driver; revert on failure.
    pub async fn add_actions(&self, device_id: i32, actions: Vec<ActionModel>) -> NGResult<()> {
        if actions.is_empty() {
            return Ok(());
        }

        // Snapshot factory via helper
        let (factory, _channel_id) = match self.snapshot_driver_factory_for_device(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        // Convert models to runtime actions
        let ractions = actions
            .into_iter()
            .filter_map(|am| match factory.convert_runtime_action(am.into()) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::error!("Error converting action: {:?}", e);
                    None
                }
            })
            .collect::<Vec<Arc<dyn RuntimeAction>>>();

        if ractions.is_empty() {
            return Ok(());
        }

        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        struct ActionsAddedRecord {
            added: Vec<Arc<dyn RuntimeAction>>,
            added_ids: Vec<i32>,
        }

        apply_with_revert(
            || {
                let mut added = Vec::with_capacity(ractions.len());
                let mut to_push: Vec<Arc<dyn RuntimeAction>> = Vec::with_capacity(ractions.len());
                for ra in ractions.into_iter() {
                    added.push(Arc::clone(&ra));
                    to_push.push(ra);
                }
                self.mutate_device_actions(device_id, |v| v.extend(to_push.into_iter()));
                let added_ids: Vec<i32> = added.iter().map(|a| a.id()).collect();
                ActionsAddedRecord { added, added_ids }
            },
            |rec| RuntimeDelta::ActionsChanged {
                device: Arc::clone(&device),
                added: rec.added.clone(),
                updated: Vec::new(),
                removed: Vec::new(),
            },
            |delta| async {
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                self.mutate_device_actions(device_id, |v| {
                    v.retain(|a| !rec.added_ids.iter().any(|id| *id == a.id()))
                });
            },
        )
        .await
    }

    /// Replace (upsert) actions by models; wait for driver and revert on failure.
    pub async fn replace_actions(&self, device_id: i32, actions: Vec<ActionModel>) -> NGResult<()> {
        if actions.is_empty() {
            return Ok(());
        }

        // Snapshot factory via helper
        let (factory, _channel_id) = match self.snapshot_driver_factory_for_device(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        let ractions = actions
            .into_iter()
            .filter_map(|am| match factory.convert_runtime_action(am.into()) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::error!("Error converting action: {:?}", e);
                    None
                }
            })
            .collect::<Vec<Arc<dyn RuntimeAction>>>();

        if ractions.is_empty() {
            return Ok(());
        }

        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        struct ActionsReplaceRecord {
            added: Vec<Arc<dyn RuntimeAction>>,
            updated: Vec<Arc<dyn RuntimeAction>>,
            added_ids: Vec<i32>,
            replaced_old: Vec<(i32, Arc<dyn RuntimeAction>)>,
        }

        apply_with_revert(
            || {
                let mut added = Vec::new();
                let mut updated = Vec::new();
                let mut replaced_old = Vec::new();
                self.mutate_device_actions(device_id, |current| {
                    for ra in ractions.into_iter() {
                        let id = ra.id();
                        if let Some(pos) = current.iter().position(|a| a.id() == id) {
                            let old = Arc::clone(&current[pos]);
                            replaced_old.push((old.id(), old));
                            current[pos] = Arc::clone(&ra);
                            updated.push(ra);
                        } else {
                            current.push(Arc::clone(&ra));
                            added.push(ra);
                        }
                    }
                });

                let added_ids: Vec<i32> = added.iter().map(|a| a.id()).collect();
                ActionsReplaceRecord {
                    added,
                    updated,
                    added_ids,
                    replaced_old,
                }
            },
            |rec| RuntimeDelta::ActionsChanged {
                device: Arc::clone(&device),
                added: rec.added.clone(),
                updated: rec.updated.clone(),
                removed: Vec::new(),
            },
            |delta| async {
                driver
                    .apply_runtime_delta(delta)
                    .await
                    .map_err(|e| NGError::DriverError(e.to_string()))
            },
            |rec| {
                self.mutate_device_actions(device_id, |current| {
                    current.retain(|a| !rec.added_ids.iter().any(|id| *id == a.id()));
                    for (id, old) in rec.replaced_old.into_iter() {
                        if let Some(pos) = current.iter().position(|a| a.id() == id) {
                            current[pos] = old;
                        } else {
                            current.push(old);
                        }
                    }
                });
            },
        )
        .await
    }

    /// Remove runtime actions by id; wait for driver and revert on failure.
    pub async fn remove_actions(&self, device_id: i32, action_ids: Vec<i32>) -> NGResult<()> {
        if action_ids.is_empty() {
            return Ok(());
        }

        struct ActionsRemovedRecord {
            removed: Vec<Arc<dyn RuntimeAction>>,
        }

        let (device, driver, _) = match self.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };

        apply_with_revert(
            || {
                let mut removed = Vec::new();
                self.mutate_device_actions(device_id, |current| {
                    let mut kept: Vec<Arc<dyn RuntimeAction>> = Vec::with_capacity(current.len());
                    for a in current.drain(..) {
                        if action_ids.iter().any(|id| *id == a.id()) {
                            removed.push(Arc::clone(&a));
                        } else {
                            kept.push(a);
                        }
                    }
                    *current = kept;
                });
                ActionsRemovedRecord { removed }
            },
            |rec| {
                if rec.removed.is_empty() {
                    return None;
                }
                Some(RuntimeDelta::ActionsChanged {
                    device: Arc::clone(&device),
                    added: Vec::new(),
                    updated: Vec::new(),
                    removed: rec.removed.clone(),
                })
            },
            |maybe_delta| async {
                if let Some(delta) = maybe_delta {
                    driver
                        .apply_runtime_delta(delta)
                        .await
                        .map_err(|e| NGError::DriverError(e.to_string()))
                } else {
                    Ok(())
                }
            },
            |rec| {
                if !rec.removed.is_empty() {
                    self.mutate_device_actions(device_id, |current| {
                        current.extend(rec.removed.into_iter())
                    });
                }
            },
        )
        .await
    }
}

/// Execute a memory change followed by an async delta application.
/// If the delta application fails, revert the in-memory change using the provided revert function.
pub async fn apply_with_revert<R, T, Fut, BuildFn, ApplyFn, RevertFn>(
    apply_mem: impl FnOnce() -> R,
    build: BuildFn,
    apply_delta: ApplyFn,
    revert_mem: RevertFn,
) -> NGResult<()>
where
    BuildFn: FnOnce(&R) -> T,
    ApplyFn: FnOnce(T) -> Fut,
    Fut: Future<Output = NGResult<()>>,
    RevertFn: FnOnce(R),
{
    let record = apply_mem();
    let payload = build(&record);
    if let Err(e) = apply_delta(payload).await {
        revert_mem(record);
        return Err(e);
    }
    Ok(())
}
