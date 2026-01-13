//! Subscription router for efficient device-to-app mapping
//!
//! Resolves which apps should receive data from specific devices based on subscriptions.
//! Uses DashMap for lock-free concurrent access.

use arc_swap::ArcSwap;
use dashmap::{mapref::entry::Entry as DashEntry, DashMap};
use ng_gateway_models::entities::prelude::AppSubModel;
use once_cell::sync::Lazy;
use std::sync::Arc;
use tracing::{debug, info};

type AppId = i32;
type Priority = i16;
type AppPriority = (AppId, Priority);
type AppPriorityList = Arc<Vec<AppPriority>>;
type DeviceSubscriptions = DashMap<AppId, AppPriorityList>;

static EMPTY_SUBSCRIPTIONS: Lazy<AppPriorityList> = Lazy::new(|| Arc::new(Vec::new()));

#[inline]
fn cmp_app_priority(a: &AppPriority, b: &AppPriority) -> std::cmp::Ordering {
    // Higher priority first; tie-break by app_id to keep ordering deterministic.
    b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0))
}

#[inline]
fn sort_by_priority(entries: &mut [AppPriority]) {
    if entries.len() <= 1 {
        return;
    }
    // We do not require stable ordering, only deterministic ordering.
    entries.sort_unstable_by(cmp_app_priority);
}

#[inline]
fn insert_sorted(entries: &mut Vec<AppPriority>, value: AppPriority) {
    // Keep the vector sorted by (priority desc, app_id asc).
    // Binary search gives O(log n) lookup; insert is O(n) due to shifting.
    // This is still typically cheaper than full sort O(n log n) for incremental updates.
    match entries.binary_search_by(|probe| cmp_app_priority(probe, &value)) {
        Ok(pos) | Err(pos) => entries.insert(pos, value),
    }
}

/// Subscription information for an app
#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    /// App ID
    pub app_id: i32,
    /// Subscribe to all devices?
    pub all_devices: bool,
    /// Specific device IDs (if not all_devices)
    pub device_ids: Vec<i32>,
    /// Priority for routing (higher = processed first)
    pub priority: i16,
}

/// High-performance subscription router using DashMap for lock-free access
#[derive(Clone)]
pub struct SubscriptionRouter {
    /// Device ID -> List of (app_id, priority)
    /// For fast lookup: "which apps subscribe to this device?"
    ///
    /// Stored as a sorted `Arc<Vec<_>>` so the hot-path can merge without allocating or sorting.
    device_subscriptions: Arc<DeviceSubscriptions>,

    /// Apps that subscribe to ALL devices -> priority
    /// Used as the source-of-truth for rebuilds of the sorted cache.
    all_devices_apps: Arc<DashMap<i32, i16>>,

    /// Cached, globally sorted list of all-devices apps (app_id, priority).
    ///
    /// This avoids scanning `all_devices_apps` on every routed message.
    all_devices_sorted: Arc<ArcSwap<Vec<AppPriority>>>,

    /// App ID -> Subscription info
    /// For management operations (update, remove)
    app_subscriptions: Arc<DashMap<i32, SubscriptionInfo>>,
}

/// Iterator merging per-device and all-devices subscriptions without allocating.
pub struct MergedApps {
    device: AppPriorityList,
    all: AppPriorityList,
    i: usize,
    j: usize,
}

impl MergedApps {
    #[inline]
    fn new(device: AppPriorityList, all: AppPriorityList) -> Self {
        Self {
            device,
            all,
            i: 0,
            j: 0,
        }
    }
}

impl Iterator for MergedApps {
    type Item = AppId;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let dlen = self.device.len();
        let alen = self.all.len();
        if self.i >= dlen && self.j >= alen {
            return None;
        }

        if self.i >= dlen {
            let (app_id, _) = self.all[self.j];
            self.j += 1;
            return Some(app_id);
        }

        if self.j >= alen {
            let (app_id, _) = self.device[self.i];
            self.i += 1;
            return Some(app_id);
        }

        let (did, dpri) = self.device[self.i];
        let (aid, apri) = self.all[self.j];

        // Defensive de-dup in case an app accidentally appears in both sources.
        if did == aid {
            self.i += 1;
            self.j += 1;
            return Some(did);
        }

        // Merge by priority desc, tie-break by app_id asc for determinism.
        if dpri > apri || (dpri == apri && did < aid) {
            self.i += 1;
            Some(did)
        } else {
            self.j += 1;
            Some(aid)
        }
    }
}

impl Default for SubscriptionRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionRouter {
    /// Create a new empty router
    pub fn new() -> Self {
        Self {
            device_subscriptions: Arc::new(DashMap::new()),
            all_devices_apps: Arc::new(DashMap::new()),
            all_devices_sorted: Arc::new(ArcSwap::from_pointee(Vec::new())),
            app_subscriptions: Arc::new(DashMap::new()),
        }
    }

    /// Resolve which apps should receive data from a device
    ///
    /// Returns app IDs sorted by priority (highest first)
    pub fn resolve(&self, device_id: i32) -> Vec<i32> {
        self.resolve_iter(device_id).collect()
    }

    /// Return a merged iterator over subscribed apps for this device (priority desc).
    ///
    /// This avoids per-message allocations and avoids scanning the all-devices map.
    #[inline]
    pub fn resolve_iter(&self, device_id: i32) -> MergedApps {
        let device = self
            .device_subscriptions
            .get(&device_id)
            .map(|entry| Arc::clone(entry.value()))
            .unwrap_or_else(|| Arc::clone(&EMPTY_SUBSCRIPTIONS));
        let all = self.all_devices_sorted.load_full();
        MergedApps::new(device, all)
    }

    /// Update subscription in the router
    ///
    /// This replaces the previous subscription for this app in the router.
    /// The subscription should already be persisted in the database.
    pub fn update(&self, app_id: i32, sub: AppSubModel) {
        // Remove old subscriptions first
        self.remove(app_id);

        if sub.all_devices {
            // Subscribe to all devices
            self.all_devices_apps.insert(app_id, sub.priority);
            self.refresh_all_devices_sorted();
            self.app_subscriptions.insert(
                app_id,
                SubscriptionInfo {
                    app_id,
                    all_devices: true,
                    device_ids: vec![],
                    priority: sub.priority,
                },
            );
            info!(
                "App {} subscribed to ALL devices (priority {})",
                app_id, sub.priority
            );
        } else if let Some(device_ids) = sub.device_ids {
            // Subscribe to specific devices (deduplicated)
            for device_id in &device_ids.0 {
                self.upsert_device_subscription(*device_id, app_id, sub.priority);
            }

            self.app_subscriptions.insert(
                app_id,
                SubscriptionInfo {
                    app_id,
                    all_devices: false,
                    device_ids: device_ids.0,
                    priority: sub.priority,
                },
            );
        }
    }

    /// Remove all subscriptions for an app
    pub fn remove(&self, app_id: i32) {
        // Get subscription info
        let sub_info = match self.app_subscriptions.remove(&app_id) {
            Some((_, info)) => info,
            None => {
                debug!("App {} has no subscriptions to remove", app_id);
                return;
            }
        };

        // Remove from all_devices if applicable
        if sub_info.all_devices {
            self.all_devices_apps.remove(&app_id);
            self.refresh_all_devices_sorted();
            info!("Removed app {} from all-devices subscriptions", app_id);
        } else {
            // Remove from specific device subscriptions
            for device_id in &sub_info.device_ids {
                self.remove_device_subscription(*device_id, app_id);
            }
            debug!(
                "Removed app {} from {} device subscriptions",
                app_id,
                sub_info.device_ids.len()
            );
        }
    }

    /// Check if an app has any subscriptions
    pub fn has_subscriptions(&self, app_id: i32) -> bool {
        self.app_subscriptions.contains_key(&app_id)
    }

    /// Get subscription info for an app
    pub fn get_subscription_info(&self, app_id: i32) -> Option<SubscriptionInfo> {
        self.app_subscriptions
            .get(&app_id)
            .map(|entry| entry.value().clone())
    }

    /// Get device IDs that an app subscribes to
    pub fn get_app_subscriptions(&self, app_id: i32) -> Vec<i32> {
        self.app_subscriptions
            .get(&app_id)
            .map(|entry| entry.value().device_ids.clone())
            .unwrap_or_default()
    }

    /// Get total number of apps with subscriptions
    pub fn total_apps(&self) -> usize {
        self.app_subscriptions.len()
    }

    /// Get total number of devices with subscriptions
    pub fn total_devices(&self) -> usize {
        self.device_subscriptions.len()
    }

    /// Clear all subscriptions (useful for testing or shutdown)
    pub fn clear(&self) {
        self.device_subscriptions.clear();
        self.all_devices_apps.clear();
        self.all_devices_sorted.store(Arc::new(Vec::new()));
        self.app_subscriptions.clear();
        info!("Cleared all subscriptions");
    }

    #[inline]
    fn refresh_all_devices_sorted(&self) {
        let mut entries: Vec<AppPriority> = self
            .all_devices_apps
            .iter()
            .map(|e| (*e.key(), *e.value()))
            .collect();
        sort_by_priority(&mut entries);
        self.all_devices_sorted.store(Arc::new(entries));
    }

    #[inline]
    fn upsert_device_subscription(&self, device_id: i32, app_id: i32, priority: i16) {
        match self.device_subscriptions.entry(device_id) {
            DashEntry::Vacant(v) => {
                v.insert(Arc::new(vec![(app_id, priority)]));
            }
            DashEntry::Occupied(mut o) => {
                let mut vec = (**o.get()).clone();
                insert_sorted(&mut vec, (app_id, priority));
                o.insert(Arc::new(vec));
            }
        }
    }

    #[inline]
    fn remove_device_subscription(&self, device_id: i32, app_id: i32) {
        let Some(mut entry) = self.device_subscriptions.get_mut(&device_id) else {
            return;
        };
        let mut vec = (**entry).clone();
        vec.retain(|(id, _)| *id != app_id);

        if vec.is_empty() {
            drop(entry);
            self.device_subscriptions.remove(&device_id);
            return;
        }

        // Order is preserved, no need to re-sort.
        *entry = Arc::new(vec);
    }
}
