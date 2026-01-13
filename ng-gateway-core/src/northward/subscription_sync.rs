//! Subscription synchronization helpers for northward applications.
//!
//! This module encapsulates state tracking and diff calculation logic that is
//! required when reconciling application subscriptions with the current southward
//! runtime state. It keeps NorthwardManager lean while providing reusable helpers
//! for future subscription-related workflows.

use crate::{
    northward::router::SubscriptionInfo,
    southward::manager::{ConnectedDeviceSnapshot, SubscriptionFilter},
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::{collections::HashSet, sync::Arc};

/// Tracks the last known status of a device for a specific application.
#[derive(Debug, Clone)]
pub struct DeviceSyncState {
    /// Last observed status.
    pub status: DeviceSyncStatus,
    /// Device name (cached for disconnect events).
    ///
    /// Stored as `Arc<str>` to minimise heap allocations when the same device
    /// name is referenced by multiple applications or events.
    pub device_name: Arc<str>,
    /// Device type (cached for disconnect events).
    ///
    /// Also stored as `Arc<str>` for the same sharing benefits as
    /// `device_name`.
    pub device_type: Arc<str>,
    /// Owning channel identifier.
    pub channel_id: i32,
    /// Last update timestamp.
    pub updated_at: DateTime<Utc>,
}

/// Enumerates the states maintained for subscription reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceSyncStatus {
    /// The application has already received a `DeviceConnected` event.
    Connected,
    /// The application has received a `DeviceDisconnected` event.
    Disconnected,
}

/// Resulting plan for a subscription reconciliation pass.
///
/// The plan separates devices that require a fresh `DeviceConnected` emission from
/// those that must be disconnected. The connect path delegates to the southward
/// manager using the provided filter to minimise snapshot costs.
#[derive(Debug, Clone)]
pub struct SyncPlan {
    /// Filter describing devices that must emit `DeviceConnected`.
    pub connect_filter: Option<SubscriptionFilter>,
    /// Device identifiers that require a `DeviceDisconnected` event.
    pub disconnect_ids: Vec<i32>,
}

impl SyncPlan {
    /// Returns true when the plan would not emit any events.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.connect_filter.is_none() && self.disconnect_ids.is_empty()
    }
}

/// In-memory tracker of device subscription states per application.
#[derive(Default)]
pub struct SubscriptionSyncTracker {
    states: DashMap<(i32, i32), DeviceSyncState>,
}

impl SubscriptionSyncTracker {
    /// Create a new subscription tracker.
    pub fn new() -> Self {
        Self {
            states: DashMap::new(),
        }
    }

    /// Record the fact that an application observed a connected device.
    pub fn mark_connected(&self, app_id: i32, snapshot: &ConnectedDeviceSnapshot) {
        self.states.insert(
            (app_id, snapshot.device_id),
            DeviceSyncState {
                status: DeviceSyncStatus::Connected,
                device_name: Arc::clone(&snapshot.device_name),
                device_type: Arc::clone(&snapshot.device_type),
                channel_id: snapshot.channel_id,
                updated_at: Utc::now(),
            },
        );
    }

    /// Record the fact that an application observed a disconnected device.
    pub fn mark_disconnected(&self, app_id: i32, device_id: i32) {
        if let Some(mut entry) = self.states.get_mut(&(app_id, device_id)) {
            entry.status = DeviceSyncStatus::Disconnected;
            entry.updated_at = Utc::now();
        }
    }

    /// Retrieve cached state for a given `(app_id, device_id)` pair.
    pub fn get_state(&self, app_id: i32, device_id: i32) -> Option<DeviceSyncState> {
        self.states
            .get(&(app_id, device_id))
            .map(|entry| entry.value().clone())
    }

    /// Enumerate all device identifiers cached as connected for the provided app.
    pub fn connected_device_ids(&self, app_id: i32) -> Vec<i32> {
        self.states
            .iter()
            .filter_map(|entry| {
                let (key_app_id, device_id) = *entry.key();
                if key_app_id == app_id && entry.value().status == DeviceSyncStatus::Connected {
                    Some(device_id)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Remove all cached state for the provided application.
    pub fn clear_app(&self, app_id: i32) {
        let keys: Vec<(i32, i32)> = self
            .states
            .iter()
            .filter_map(|entry| {
                let (key_app_id, device_id) = *entry.key();
                if key_app_id == app_id {
                    Some((key_app_id, device_id))
                } else {
                    None
                }
            })
            .collect();

        for key in keys.into_iter() {
            self.states.remove(&key);
        }
    }
}

/// Compute a reconciliation plan between the previous and current router states.
///
/// The computation is purely in-memory and does not access southward state.
pub fn compute_sync_plan(
    tracker: &SubscriptionSyncTracker,
    app_id: i32,
    previous: Option<&SubscriptionInfo>,
    current: &SubscriptionInfo,
) -> Option<SyncPlan> {
    let mut connect_filter = None;
    let mut disconnect_ids: Vec<i32> = Vec::new();

    match (previous, current.all_devices) {
        (None, true) => {
            connect_filter = Some(SubscriptionFilter::AllDevices);
        }
        (None, false) => {
            if !current.device_ids.is_empty() {
                connect_filter = Some(SubscriptionFilter::DeviceIds(current.device_ids.clone()));
            }
        }
        (Some(prev), true) => {
            if !prev.all_devices {
                connect_filter = Some(SubscriptionFilter::AllDevices);
            }
        }
        (Some(prev), false) if prev.all_devices => {
            if !current.device_ids.is_empty() {
                connect_filter = Some(SubscriptionFilter::DeviceIds(current.device_ids.clone()));
            }
            let new_ids: HashSet<i32> = current.device_ids.iter().copied().collect();
            disconnect_ids = tracker
                .connected_device_ids(app_id)
                .into_iter()
                .filter(|id| !new_ids.contains(id))
                .collect();
        }
        (Some(prev), false) => {
            let prev_ids: HashSet<i32> = prev.device_ids.iter().copied().collect();
            let new_ids: HashSet<i32> = current.device_ids.iter().copied().collect();

            let added: Vec<i32> = new_ids.difference(&prev_ids).copied().collect();
            if !added.is_empty() {
                connect_filter = Some(SubscriptionFilter::DeviceIds(added));
            }

            disconnect_ids = prev_ids.difference(&new_ids).copied().collect::<Vec<i32>>();
        }
    }

    disconnect_ids.sort_unstable();
    disconnect_ids.dedup();

    let plan = SyncPlan {
        connect_filter,
        disconnect_ids,
    };

    if plan.is_empty() {
        None
    } else {
        Some(plan)
    }
}
