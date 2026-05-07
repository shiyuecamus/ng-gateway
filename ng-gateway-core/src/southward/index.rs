use super::{
    internal::{
        empty_runtime_actions, empty_runtime_points, make_point_path_key, with_point_path_key,
    },
    ChannelInstance, DeviceDriverSnapshot, DeviceInstance,
};
use dashmap::{mapref::entry::Entry as DashEntry, DashMap};
use ng_gateway_sdk::{
    Driver, DriverFactory, PointMeta, RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimePoint,
};
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

    /// Clear all in-memory runtime tables (best-effort).
    ///
    /// # Notes
    /// - This is a **control-plane** operation intended for shutdown paths.
    /// - This does **not** reset `runtime_delta_tx` to keep subscribers stable.
    #[inline]
    pub fn clear(&self) {
        self.device_points.clear();
        self.point_entries_by_id.clear();
        self.point_id_by_path.clear();
        self.device_actions.clear();
        self.channel_devices.clear();
        self.device_name_index.clear();
        self.devices.clear();
        self.channels.clear();
    }

    /// Get device ids bound to a channel (best-effort snapshot).
    ///
    /// # Notes
    /// This is a control-plane helper. It rebuilds a `Vec` from an `Arc<[i32]>`.
    #[inline]
    pub fn channel_device_ids(&self, channel_id: i32) -> Vec<i32> {
        self.channel_devices
            .get(&channel_id)
            .map(|e| e.value().iter().copied().collect())
            .unwrap_or_default()
    }

    /// Atomically add one device id into the `channel_id -> [device_id]` mapping.
    ///
    /// # Concurrency
    /// This method updates the entry under a `DashMap` entry guard to avoid lost updates.
    #[inline]
    pub fn add_device_to_channel(&self, channel_id: i32, device_id: i32) {
        match self.channel_devices.entry(channel_id) {
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

    /// Atomically remove one device id from the `channel_id -> [device_id]` mapping.
    ///
    /// # Notes
    /// This keeps empty slices instead of removing the key to avoid races deleting concurrent inserts.
    #[inline]
    pub fn remove_device_from_channel(&self, channel_id: i32, device_id: i32) {
        match self.channel_devices.entry(channel_id) {
            DashEntry::Occupied(mut occ) => {
                let mut ids: Vec<i32> = occ.get().iter().copied().collect();
                let before = ids.len();
                ids.retain(|x| *x != device_id);
                if ids.len() != before {
                    *occ.get_mut() = Arc::from(ids.into_boxed_slice());
                }
            }
            DashEntry::Vacant(_) => {}
        }
    }

    /// Snapshot the runtime point slice for a device, if present.
    #[inline]
    pub fn device_points_slice(&self, device_id: i32) -> Option<RuntimePointSlice> {
        self.device_points
            .get(&device_id)
            .map(|e| Arc::clone(e.value()))
    }

    /// Replace a device's points with a new slice (atomic write).
    #[inline]
    pub fn set_device_points(&self, device_id: i32, points: Vec<Arc<dyn RuntimePoint>>) {
        match self.device_points.entry(device_id) {
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
    /// # Safety
    /// The closure MUST NOT touch other `DashMap`s to avoid lock ordering risks.
    #[inline]
    pub fn mutate_device_points<R>(
        &self,
        device_id: i32,
        f: impl FnOnce(&mut Vec<Arc<dyn RuntimePoint>>) -> R,
    ) -> R {
        match self.device_points.entry(device_id) {
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

    /// Snapshot the runtime action slice for a device, if present.
    #[inline]
    pub fn device_actions_slice(&self, device_id: i32) -> Option<RuntimeActionSlice> {
        self.device_actions
            .get(&device_id)
            .map(|e| Arc::clone(e.value()))
    }

    /// Replace a device's actions with a new slice (atomic write).
    #[inline]
    pub fn set_device_actions(&self, device_id: i32, actions: Vec<Arc<dyn RuntimeAction>>) {
        match self.device_actions.entry(device_id) {
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
    /// # Safety
    /// The closure MUST NOT touch other `DashMap`s to avoid lock ordering risks.
    #[inline]
    pub fn mutate_device_actions<R>(
        &self,
        device_id: i32,
        f: impl FnOnce(&mut Vec<Arc<dyn RuntimeAction>>) -> R,
    ) -> R {
        match self.device_actions.entry(device_id) {
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

    /// Get points for a specific device (returns a cached empty slice when missing).
    #[inline]
    pub fn get_device_points(&self, device_id: i32) -> RuntimePointSlice {
        self.device_points_slice(device_id)
            .unwrap_or_else(empty_runtime_points)
    }

    /// Get actions for a specific device (returns a cached empty slice when missing).
    #[inline]
    pub fn get_device_actions(&self, device_id: i32) -> RuntimeActionSlice {
        self.device_actions_slice(device_id)
            .unwrap_or_else(empty_runtime_actions)
    }

    /// Get readable points for a device (best-effort snapshot).
    #[inline]
    pub fn get_readable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.get_device_points(device_id)
            .iter()
            .filter(|&p| p.readable())
            .cloned()
            .collect()
    }

    /// Get writable points for a device (best-effort snapshot).
    #[inline]
    pub fn get_writable_data_points(&self, device_id: i32) -> Vec<Arc<dyn RuntimePoint>> {
        self.get_device_points(device_id)
            .iter()
            .filter(|&p| p.writable())
            .cloned()
            .collect()
    }

    /// Upsert point entry indexes for a runtime point.
    ///
    /// # Notes
    /// This is called on topology changes and is not a hot path.
    pub fn upsert_point_entry(
        &self,
        channel_name: &str,
        device: &Arc<dyn RuntimeDevice>,
        point: &Arc<dyn RuntimePoint>,
        description: Option<Arc<str>>,
    ) {
        // If path changes, remove the old reverse mapping first (drop map guard before removal).
        let old_key = self.point_entries_by_id.get(&point.id()).map(|old| {
            let m = &old.value().meta;
            make_point_path_key(
                m.channel_name.as_ref(),
                m.device_name.as_ref(),
                m.point_key.as_ref(),
            )
        });
        if let Some(old_key) = old_key {
            self.point_id_by_path.remove(&old_key);
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
            point_type: point.r#type(),
            access_mode: point.access_mode(),
            unit: point.unit().map(Arc::<str>::from),
            min_value: point.min_value(),
            max_value: point.max_value(),
            transform: *point.transform(),
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
        self.point_entries_by_id.insert(meta.point_id, entry);
        self.point_id_by_path.insert(key, meta.point_id);
    }

    /// Remove one point entry by `point_id` and clear its reverse mapping.
    #[inline]
    pub fn remove_point_entry_by_id(&self, point_id: i32) {
        // Remove the entry first and derive the reverse key from the removed value.
        // This avoids an extra DashMap lookup on hot paths like bulk delete/clear.
        if let Some((_k, entry)) = self.point_entries_by_id.remove(&point_id) {
            let m = &entry.meta;
            let old_key = make_point_path_key(
                m.channel_name.as_ref(),
                m.device_name.as_ref(),
                m.point_key.as_ref(),
            );
            self.point_id_by_path.remove(&old_key);
        }
    }

    /// Snapshot the driver for a channel.
    #[inline]
    pub fn snapshot_channel_driver(&self, channel_id: i32) -> Option<Arc<dyn Driver>> {
        self.channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver))
    }

    /// Snapshot device runtime, its driver and channel id by device id.
    #[inline]
    pub fn snapshot_device_and_driver(&self, device_id: i32) -> Option<DeviceDriverSnapshot> {
        let dev = self.devices.get(&device_id)?;
        let channel_id = dev.config.channel_id();
        let device = Arc::clone(&dev.config);
        drop(dev);
        let driver = self.snapshot_channel_driver(channel_id)?;
        Some((device, driver, channel_id))
    }

    /// Snapshot driver factory for a channel.
    #[inline]
    pub fn snapshot_driver_factory_for_channel(
        &self,
        channel_id: i32,
    ) -> Option<Arc<dyn DriverFactory>> {
        self.channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver_factory))
    }

    /// Snapshot driver factory for a device, returning also its channel id.
    #[inline]
    pub fn snapshot_driver_factory_for_device(
        &self,
        device_id: i32,
    ) -> Option<(Arc<dyn DriverFactory>, i32)> {
        let dev = self.devices.get(&device_id)?;
        let channel_id = dev.config.channel_id();
        drop(dev);
        let factory = self.snapshot_driver_factory_for_channel(channel_id)?;
        Some((factory, channel_id))
    }

    /// Lookup `point_id` by `(channel_name, device_name, point_key)` without allocating.
    ///
    /// # Performance
    /// - This uses a thread-local buffer to build the composite key and performs a `DashMap` lookup
    ///   by `&str`, avoiding per-call heap allocations.
    ///
    /// # Safety
    /// - The lookup key is only valid during the call; this function never `.await`.
    #[inline]
    pub fn find_point_id_by_path_parts(
        &self,
        channel_name: &str,
        device_name: &str,
        point_key: &str,
    ) -> Option<i32> {
        with_point_path_key(channel_name, device_name, point_key, |key| {
            self.point_id_by_path.get(key).map(|e| *e.value())
        })
    }
}
