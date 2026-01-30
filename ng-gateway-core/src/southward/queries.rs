//! Southward read-side APIs (control-plane queries).
//!
//! This module groups *read-only* accessors that are primarily used by REST/WS/UI layers.
//! Keeping them here reduces the size and cognitive load of the southward manager implementation.

use super::{
    ChannelInstance, ConnectedDeviceSnapshot, DeviceBasicSnapshot, DeviceInstance,
    NGSouthwardManager, SubscriptionFilter,
};
use chrono::{DateTime, Utc};
use dashmap::mapref::one::Ref;
use ng_gateway_models::core::metrics::DeviceStatsSnapshot;
use ng_gateway_sdk::{PointMeta, RuntimePoint, Status};
use std::{collections::HashSet, sync::Arc};

impl NGSouthwardManager {
    /// Get enabled channel instances that are configured for polling collection.
    #[inline]
    pub fn get_collectable_channels(&self) -> Vec<i32> {
        self.runtime
            .index
            .channels
            .iter()
            .filter(|entry| {
                entry.value().status == Status::Enabled && entry.value().config.collectable()
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

    /// Get channel instance by ID.
    #[inline]
    pub fn get_channel(&self, channel_id: i32) -> Option<Ref<'_, i32, ChannelInstance>> {
        self.runtime.index.channels.get(&channel_id)
    }

    /// Snapshot the driver id string for a channel (best-effort).
    ///
    /// # Notes
    /// This is used by control-plane metrics to keep label cardinality bounded.
    #[inline]
    pub fn snapshot_channel_driver_label(&self, channel_id: i32) -> Option<Arc<str>> {
        self.runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver_label))
    }

    /// Get all channel IDs.
    #[inline]
    pub fn get_channel_ids(&self) -> Vec<i32> {
        self.runtime
            .index
            .channels
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    /// Get enabled channel IDs.
    #[inline]
    pub fn get_enabled_channel_ids(&self) -> Vec<i32> {
        self.runtime
            .index
            .channels
            .iter()
            .filter(|entry| entry.value().status == Status::Enabled)
            .map(|entry| *entry.key())
            .collect()
    }

    /// Check if a channel is connected.
    #[inline]
    pub fn is_channel_connected(&self, channel_id: i32) -> bool {
        self.runtime
            .index
            .channels
            .get(&channel_id)
            .map(|entry| entry.state.is_connected())
            .unwrap_or(false)
    }

    /// Check if a channel is collectable (enabled + polling + connected).
    #[inline]
    pub fn is_channel_collectable(&self, channel_id: i32) -> bool {
        self.runtime
            .index
            .channels
            .get(&channel_id)
            .map(|entry| {
                entry.status == Status::Enabled
                    && entry.config.collectable()
                    && entry.state.is_connected()
            })
            .unwrap_or(false)
    }

    /// Get device instance by ID.
    #[inline]
    pub fn get_device(&self, device_id: i32) -> Option<Ref<'_, i32, DeviceInstance>> {
        self.runtime.index.devices.get(&device_id)
    }

    /// Snapshot basic device info for UIs (best-effort, O(1)).
    #[inline]
    pub fn snapshot_device_basic(&self, device_id: i32) -> Option<DeviceBasicSnapshot> {
        let dev = self.runtime.index.devices.get(&device_id)?;
        let channel_id = dev.config.channel_id();
        let device_name = dev.config.device_name().to_string();
        let device_type = dev.config.device_type().to_string();
        let status = dev.status;
        let state = dev.state;
        let last_collection = dev.last_collection;
        let last_data_change = dev.last_data_change;
        drop(dev);

        Some(DeviceBasicSnapshot {
            device_id,
            channel_id,
            device_name,
            device_type,
            status,
            state,
            last_collection,
            last_data_change,
        })
    }

    /// Build per-device stats snapshots for a channel, intended for WS/observability UIs.
    ///
    /// This follows the same style as `get_channel_snapshot`: acquire DashMap guard, copy fields,
    /// drop guard, then build fully-serializable DTOs.
    #[inline]
    pub fn get_channel_device_snapshots(&self, channel_id: i32) -> Vec<DeviceStatsSnapshot> {
        let prom = self.get_channel_metric_handles(channel_id);
        let device_ids = self.channel_device_ids(channel_id);
        let mut out = Vec::with_capacity(device_ids.len());

        for device_id in device_ids {
            let Some(dev) = self.snapshot_device_basic(device_id) else {
                continue;
            };

            let metrics_opt = prom
                .as_ref()
                .and_then(|h| h.snapshot_device_metrics(device_id));
            let metrics = metrics_opt.unwrap_or_default();

            out.push(DeviceStatsSnapshot {
                device_id,
                channel_id,
                device_name: dev.device_name,
                device_type: dev.device_type,
                status: dev.status as i32,
                runtime_state: Some(dev.state),
                metrics,
            });
        }

        out
    }

    /// Get the channel id for a specific device, if present in the index.
    #[inline]
    pub fn get_device_channel_id(&self, device_id: i32) -> Option<i32> {
        self.runtime
            .index
            .devices
            .get(&device_id)
            .map(|entry| entry.config.channel_id())
    }

    /// Get point metadata by `point_id`.
    ///
    /// # Performance
    /// This is an **O(1)** lookup backed by `DashMap` and returns `Arc<PointMeta>` so clones are cheap.
    #[inline]
    pub fn get_point_meta(&self, point_id: i32) -> Option<Arc<PointMeta>> {
        self.runtime
            .index
            .point_entries_by_id
            .get(&point_id)
            .map(|e| {
                let entry = e.value();
                Arc::clone(&entry.meta)
            })
    }

    /// Get runtime point by `point_id` (O(1)).
    #[inline]
    pub fn get_runtime_point(&self, point_id: i32) -> Option<Arc<dyn RuntimePoint>> {
        self.runtime
            .index
            .point_entries_by_id
            .get(&point_id)
            .map(|e| {
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
        self.runtime
            .index
            .point_entries_by_id
            .get(&point_id)
            .map(|e| {
                let entry = e.value();
                (Arc::clone(&entry.meta), Arc::clone(&entry.point))
            })
    }

    /// Find device id by name.
    #[inline]
    pub fn find_device_id_by_name(&self, device_name: &str) -> Option<i32> {
        self.runtime
            .index
            .device_name_index
            .get(device_name)
            .map(|entry| *entry.value())
    }

    /// Get collectable device IDs for a specific channel (enabled devices only).
    #[inline]
    pub fn get_collectable_device_ids(&self, channel_id: i32) -> Vec<i32> {
        self.runtime
            .index
            .channel_devices
            .get(&channel_id)
            .map(|entry| entry.value().iter().copied().collect::<Vec<i32>>())
            .unwrap_or_default()
            .into_iter()
            .filter(|device_id| {
                self.runtime
                    .index
                    .devices
                    .get(device_id)
                    .map(|dev| dev.status == Status::Enabled)
                    .unwrap_or(false)
            })
            .collect()
    }

    /// Collect connected devices for all channels that are currently online.
    fn collect_all_connected_devices(&self) -> Vec<ConnectedDeviceSnapshot> {
        let connected_channels: Vec<(i32, Arc<str>, DateTime<Utc>)> = self
            .runtime
            .index
            .channels
            .iter()
            .filter(|entry| entry.value().state.is_connected())
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
            let Some(device_ids) = self.runtime.index.channel_devices.get(&channel_id) else {
                continue;
            };

            for device_id in device_ids.iter().copied() {
                let Some(device) = self.runtime.index.devices.get(&device_id) else {
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
    /// The function deduplicates identifiers and filters out devices whose channels are not currently connected.
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
            let Some(device) = self.runtime.index.devices.get(&device_id) else {
                continue;
            };
            let channel_id = device.config.channel_id();
            let Some(channel) = self.runtime.index.channels.get(&channel_id) else {
                continue;
            };
            if !channel.state.is_connected() {
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
}
