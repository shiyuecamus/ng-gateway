//! Southward device snapshot cache and change detection.
//!
//! This module owns the "device snapshot" logic used by northward routing:
//! - Maintains the latest telemetry/attribute baseline per device.
//! - Applies `ReportType` policies (Always vs Change) to filter uplink payloads.
//!
//! # Performance principles
//! - Avoid cloning large structures on the hot path.
//! - Prefer in-place filtering with `Vec::retain`.
//! - Use monotonic timestamps (`snapshot_now_ms`) for TTL refresh bookkeeping.

use super::{DeviceDataSnapshot, NGSouthwardManager, PointSnapshotEntry};
use crate::southward::internal::snapshot_now_ms;
use chrono::{DateTime, Utc};
use ng_gateway_sdk::{
    AttributeData, DeviceState, NorthwardData, PointValue, ReportType, TelemetryData,
};
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::Arc,
};

/// Build a `PointSnapshotEntry` from a `PointValue` and the current monotonic time.
#[inline]
fn make_entry(now_ms: u64, pv: &PointValue) -> PointSnapshotEntry {
    (
        now_ms,
        pv.value.clone(),
        pv.ts.map(|t| t.timestamp_millis()),
    )
}

impl NGSouthwardManager {
    /// Update device data snapshot and filter changes based on `ReportType`.
    ///
    /// This method:
    /// 1. Reads the `ReportType` from the device's channel
    /// 2. If `ReportType::Change`, compares new data with existing snapshot to detect changes
    /// 3. Updates the snapshot with latest values
    /// 4. Returns filtered data (only changed values for `ReportType::Change`,
    ///    or full data for `ReportType::Always`)
    ///
    /// # Returns
    /// - `Some(data)` when the payload should be forwarded northward
    /// - `None` when `ReportType::Change` yields no changes (drop)
    pub fn update_and_filter_device_snapshot(
        &self,
        mut data: Arc<NorthwardData>,
    ) -> Option<Arc<NorthwardData>> {
        let device_id = data.device_id();
        let now = Utc::now();
        let now_ms = snapshot_now_ms();

        let device = match self.get_device(device_id) {
            Some(d) => d,
            None => {
                return Some(data);
            }
        };

        let channel_id = device.config.channel_id();

        let report_type = match self.get_channel(channel_id) {
            Some(channel) => channel.config.report_type(),
            None => {
                return Some(data);
            }
        };

        if matches!(report_type, ReportType::Always) {
            self.update_snapshot_internal(data.as_ref(), device_id, now, now_ms);
            return Some(data);
        }

        let data_mut = Arc::make_mut(&mut data);
        match data_mut {
            NorthwardData::Telemetry(telemetry) => {
                if self.filter_telemetry_changes_in_place(device_id, telemetry, now, now_ms) {
                    Some(data)
                } else {
                    None
                }
            }
            NorthwardData::Attributes(attributes) => {
                if self.filter_attributes_changes_in_place(device_id, attributes, now, now_ms) {
                    Some(data)
                } else {
                    None
                }
            }
            _ => Some(data),
        }
    }

    /// Get device data snapshot by device ID.
    ///
    /// Returns the latest snapshot of telemetry and attribute values for the device.
    /// Returns None if no snapshot exists (device has never reported data).
    #[inline]
    pub fn get_device_snapshot(&self, device_id: i32) -> Option<DeviceDataSnapshot> {
        self.runtime
            .device_snapshots
            .get(&device_id)
            .map(|e| e.value().clone())
    }

    /// Get runtime state for a specific device, if present in the index.
    ///
    /// This is primarily used by monitoring APIs to filter online/active devices.
    #[inline]
    pub fn get_device_state(&self, device_id: i32) -> Option<DeviceState> {
        self.runtime
            .index
            .devices
            .get(&device_id)
            .map(|entry| entry.state)
    }

    /// Internal helper to update snapshot without filtering.
    fn update_snapshot_internal(
        &self,
        data: &NorthwardData,
        device_id: i32,
        now: DateTime<Utc>,
        now_ms: u64,
    ) {
        match data {
            NorthwardData::Telemetry(telemetry) => {
                self.runtime
                    .device_snapshots
                    .entry(device_id)
                    .and_modify(|snapshot| {
                        for pv in telemetry.values.iter() {
                            match snapshot.telemetry.entry(pv.point_id) {
                                Entry::Occupied(mut o) => {
                                    let (ts, old, src_ts) = o.get_mut();
                                    if old != &pv.value {
                                        *old = pv.value.clone();
                                        *ts = now_ms;
                                    }
                                    *src_ts = pv.ts.map(|t| t.timestamp_millis());
                                }
                                Entry::Vacant(v) => {
                                    v.insert(make_entry(now_ms, pv));
                                }
                            }
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
                            telemetry_map.insert(pv.point_id, make_entry(now_ms, pv));
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
                self.runtime
                    .device_snapshots
                    .entry(device_id)
                    .and_modify(|snapshot| {
                        for pv in attributes.client_attributes.iter() {
                            snapshot
                                .client_attributes
                                .entry(pv.point_id)
                                .and_modify(|(ts, old, src_ts)| {
                                    if old != &pv.value {
                                        *old = pv.value.clone();
                                        *ts = now_ms;
                                    }
                                    *src_ts = pv.ts.map(|t| t.timestamp_millis());
                                })
                                .or_insert_with(|| make_entry(now_ms, pv));
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        for pv in attributes.shared_attributes.iter() {
                            snapshot
                                .shared_attributes
                                .entry(pv.point_id)
                                .and_modify(|(ts, old, src_ts)| {
                                    if old != &pv.value {
                                        *old = pv.value.clone();
                                        *ts = now_ms;
                                    }
                                    *src_ts = pv.ts.map(|t| t.timestamp_millis());
                                })
                                .or_insert_with(|| make_entry(now_ms, pv));
                            Self::upsert_point_key_by_id(
                                &mut snapshot.point_key_by_id,
                                pv.point_id,
                                &pv.point_key,
                            );
                        }
                        for pv in attributes.server_attributes.iter() {
                            snapshot
                                .server_attributes
                                .entry(pv.point_id)
                                .and_modify(|(ts, old, src_ts)| {
                                    if old != &pv.value {
                                        *old = pv.value.clone();
                                        *ts = now_ms;
                                    }
                                    *src_ts = pv.ts.map(|t| t.timestamp_millis());
                                })
                                .or_insert_with(|| make_entry(now_ms, pv));
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
                            client.insert(pv.point_id, make_entry(now_ms, pv));
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        for pv in attributes.shared_attributes.iter() {
                            shared.insert(pv.point_id, make_entry(now_ms, pv));
                            point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                        }
                        for pv in attributes.server_attributes.iter() {
                            server.insert(pv.point_id, make_entry(now_ms, pv));
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
            _ => {}
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
        now_ms: u64,
    ) -> bool {
        let existing_snapshot = self.runtime.device_snapshots.get(&device_id);
        let existing_telemetry = existing_snapshot.as_ref().map(|s| &s.telemetry);
        telemetry.values.retain(|pv| match existing_telemetry {
            Some(existing) => existing
                .get(&pv.point_id)
                .map(|(_ts, old_value, _src_ts)| old_value != &pv.value)
                .unwrap_or(true),
            None => true,
        });

        if telemetry.values.is_empty() {
            return false;
        }

        drop(existing_snapshot);

        self.runtime
            .device_snapshots
            .entry(device_id)
            .and_modify(|snapshot| {
                for pv in telemetry.values.iter() {
                    snapshot
                        .telemetry
                        .insert(pv.point_id, make_entry(now_ms, pv));
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
                    telemetry_map.insert(pv.point_id, make_entry(now_ms, pv));
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
        now_ms: u64,
    ) -> bool {
        let existing_snapshot = self.runtime.device_snapshots.get(&device_id);
        let snapshot_ref = existing_snapshot.as_ref();

        attributes
            .client_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .client_attributes
                    .get(&pv.point_id)
                    .map(|(_ts, old_value, _src_ts)| old_value != &pv.value)
                    .unwrap_or(true),
                None => true,
            });
        attributes
            .shared_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .shared_attributes
                    .get(&pv.point_id)
                    .map(|(_ts, old_value, _src_ts)| old_value != &pv.value)
                    .unwrap_or(true),
                None => true,
            });
        attributes
            .server_attributes
            .retain(|pv| match snapshot_ref {
                Some(snapshot) => snapshot
                    .server_attributes
                    .get(&pv.point_id)
                    .map(|(_ts, old_value, _src_ts)| old_value != &pv.value)
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

        self.runtime
            .device_snapshots
            .entry(device_id)
            .and_modify(|snapshot| {
                for pv in attributes.client_attributes.iter() {
                    snapshot
                        .client_attributes
                        .insert(pv.point_id, make_entry(now_ms, pv));
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                for pv in attributes.shared_attributes.iter() {
                    snapshot
                        .shared_attributes
                        .insert(pv.point_id, make_entry(now_ms, pv));
                    Self::upsert_point_key_by_id(
                        &mut snapshot.point_key_by_id,
                        pv.point_id,
                        &pv.point_key,
                    );
                }
                for pv in attributes.server_attributes.iter() {
                    snapshot
                        .server_attributes
                        .insert(pv.point_id, make_entry(now_ms, pv));
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
                    client.insert(pv.point_id, make_entry(now_ms, pv));
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                for pv in attributes.shared_attributes.iter() {
                    shared.insert(pv.point_id, make_entry(now_ms, pv));
                    point_key_by_id.insert(pv.point_id, Arc::clone(&pv.point_key));
                }
                for pv in attributes.server_attributes.iter() {
                    server.insert(pv.point_id, make_entry(now_ms, pv));
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
}
