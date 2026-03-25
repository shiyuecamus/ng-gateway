//! Southward runtime mutation helpers (control-plane).
//!
//! This module groups low-frequency runtime mutations that update in-memory indexes and
//! notify southward drivers via `RuntimeDelta`.
//!
//! # Safety
//! - Never hold `DashMap` guards across `.await`.
//! - All driver calls are best-effort and performed in spawned tasks.

use super::{DeviceInstance, NGSouthwardManager};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::entities::prelude::{ActionModel, DeviceModel, PointModel};
use ng_gateway_sdk::{Driver, RuntimeAction, RuntimeDelta, RuntimeDevice, RuntimePoint, Status};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::broadcast;
use tracing::Instrument;

impl NGSouthwardManager {
    /// Build a `DeviceInstance` from a persisted `DeviceModel`.
    ///
    /// # Notes
    /// - This is a control-plane conversion helper used by runtime mutation paths.
    /// - It relies on the channel's `DriverFactory` to convert the persisted model into a runtime device.
    /// - The returned instance is bound to the channel's current driver handle.
    fn build_device_instance_from_model(
        &self,
        device: DeviceModel,
    ) -> NGResult<(i32, DeviceInstance)> {
        let channel = self.runtime.index.channels.get(&device.channel_id).ok_or(
            NGError::InitializationError(format!(
                "Channel {} not found for device: {}",
                device.channel_id, device.device_name
            )),
        )?;

        let device_id = device.id;
        let runtime_device = channel
            .driver_factory
            .convert_runtime_device(device.into())
            .map_err(|e| {
                NGError::InitializationError(format!("Failed to convert device to runtime: {e}"))
            })?;

        let status = runtime_device.status();
        let instance = DeviceInstance {
            config: runtime_device,
            state: ng_gateway_sdk::DeviceState::Active,
            status,
            driver: Arc::clone(&channel.driver),
            last_collection: None,
            last_data_change: None,
            created_at: chrono::Utc::now(),
        };

        Ok((device_id, instance))
    }

    /// Change channel status in the runtime index (best-effort).
    #[inline]
    pub fn change_channel_status(&self, channel_id: i32, status: Status) {
        if let Some(mut chan) = self.runtime.index.channels.get_mut(&channel_id) {
            chan.status = status;
        }
    }

    /// Notify driver about device status change without altering runtime tables.
    ///
    /// This updates the in-memory `DeviceInstance.status` and emits a `RuntimeDelta`
    /// to the owning channel driver in a fire-and-forget task.
    #[inline]
    pub async fn change_device_status(&self, device: &DeviceModel, status: Status) -> NGResult<()> {
        // Resolve device instance and its channel.
        let mut dev = match self.runtime.index.devices.get_mut(&device.id) {
            Some(d) => d,
            None => return Ok(()),
        };
        // Update in-memory status.
        dev.set_status(status);
        let channel_id = dev.config.channel_id();

        // Snapshot driver handle before spawning (never hold DashMap guard across await).
        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver));
        let Some(driver) = driver else {
            return Ok(());
        };

        // Build delta (fire-and-forget).
        let delta = RuntimeDelta::DevicesChanged {
            added: Vec::new(),
            updated: Vec::new(),
            removed: Vec::new(),
            status_changed: vec![(Arc::clone(&dev.config), status)],
        };

        // Bind a per-channel span so apply-delta errors can be attributed to `channel_id`.
        let device_id = device.id;
        let span = tracing::info_span!(
            "southward-apply-delta",
            channel_id = channel_id,
            device_id = device_id
        );
        tokio::spawn(
            async move {
                match driver.apply_runtime_delta(delta).await {
                    Ok(()) => {}
                    Err(e) if e.is_unreachable() => {
                        tracing::debug!(error = %e, "Driver unreachable, status delta skipped");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to apply runtime delta");
                    }
                }
            }
            .instrument(span),
        );
        Ok(())
    }

    /// Add many devices into runtime tables of a channel; revert memory on driver error.
    pub async fn add_devices(&self, channel_id: i32, devices: Vec<DeviceModel>) -> NGResult<()> {
        if devices.is_empty() {
            return Ok(());
        }

        // Ensure channel exists; driver will be fetched later via a short-lived guard.
        if !self.runtime.index.channels.contains_key(&channel_id) {
            return Ok(());
        }

        // Build instances; skip conversions that fail to avoid poisoning the batch.
        let instances: Vec<(i32, DeviceInstance)> = devices
            .into_iter()
            .filter_map(|d| match self.build_device_instance_from_model(d) {
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

        // Snapshot driver handle (never hold guards across await).
        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver));
        let Some(driver) = driver else {
            return Ok(());
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
                    self.runtime
                        .index
                        .devices
                        .insert(*device_id, instance.clone());
                    self.runtime
                        .index
                        .add_device_to_channel(channel_id, *device_id);
                    if let Some(prev) = self
                        .runtime
                        .index
                        .device_name_index
                        .insert(Arc::clone(&device_name), *device_id)
                    {
                        if prev != *device_id {
                            tracing::warn!(
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
            DeltaSink::new(&driver, channel_id),
            |rec| {
                for id in rec.added_ids.iter().copied() {
                    self.runtime
                        .index
                        .remove_device_from_channel(channel_id, id);
                }
                for (name, id) in rec.added_names.into_iter() {
                    self.runtime
                        .index
                        .device_name_index
                        .remove_if(&name, |_, v| *v == id);
                    self.runtime.index.devices.remove(&id);
                }
            },
        )
        .await?;

        // Control-plane path: refresh aggregated counts for observability snapshots.
        self.refresh_manager_snapshot_from_index().await;
        Ok(())
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

        if !self.runtime.index.channels.contains_key(&channel_id) {
            return Ok(());
        }

        // Build instances; ensure all belong to the target channel.
        let group = devices
            .into_iter()
            .filter_map(|d| match self.build_device_instance_from_model(d) {
                Ok(instance) => Some(instance),
                Err(e) => {
                    tracing::error!("Error creating device instance: {:?}", e);
                    None
                }
            })
            .collect::<Vec<_>>();

        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver));
        let Some(driver) = driver else {
            return Ok(());
        };

        struct DevicesReplaceRecord {
            added: Vec<Arc<dyn RuntimeDevice>>,
            updated: Vec<Arc<dyn RuntimeDevice>>,
            added_ids: Vec<i32>,
            updated_snapshots: Vec<(i32, DeviceInstance, Arc<str>)>,
        }

        apply_with_revert(
            || {
                // Prepare added/updated and snapshots for revert.
                let mut added = Vec::new();
                let mut updated = Vec::new();
                let mut added_ids: Vec<i32> = Vec::new();
                let mut updated_snapshots = Vec::new();

                for (device_id, instance) in group.iter() {
                    let new_name: Arc<str> = Arc::from(instance.config.device_name());
                    // Get old device info and explicitly drop the read lock before insert.
                    let old_name = {
                        let old_ref = self.runtime.index.devices.get(device_id);
                        let result = old_ref.as_ref().map(|old| {
                            let name: Arc<str> = Arc::from(old.config.device_name());
                            updated_snapshots.push((
                                *device_id,
                                old.value().clone(),
                                Arc::clone(&name),
                            ));
                            name
                        });
                        drop(old_ref);
                        result
                    };

                    if let Some(old_name) = old_name {
                        if old_name != new_name {
                            self.runtime
                                .index
                                .device_name_index
                                .remove_if(&old_name, |_, v| *v == *device_id);
                            self.runtime
                                .index
                                .device_name_index
                                .insert(Arc::clone(&new_name), *device_id);
                        }
                        // Replace device.
                        self.runtime
                            .index
                            .devices
                            .insert(*device_id, instance.clone());
                        updated.push(Arc::clone(&instance.config));
                    } else {
                        // Insert new.
                        self.runtime
                            .index
                            .devices
                            .insert(*device_id, instance.clone());
                        self.runtime
                            .index
                            .device_name_index
                            .insert(Arc::clone(&new_name), *device_id);
                        self.runtime
                            .index
                            .add_device_to_channel(channel_id, *device_id);
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
            DeltaSink::new(&driver, channel_id),
            |rec| {
                // Revert: remove added; restore updated snapshots and mappings.
                for id in rec.added_ids.into_iter() {
                    if let Some((_, inst)) = self.runtime.index.devices.remove(&id) {
                        let name: Arc<str> = Arc::from(inst.config.device_name());
                        self.runtime
                            .index
                            .device_name_index
                            .remove_if(&name, |_, v| *v == id);
                        self.runtime
                            .index
                            .remove_device_from_channel(channel_id, id);
                    }
                }
                for (id, old_inst, old_name) in rec.updated_snapshots.into_iter() {
                    self.runtime.index.devices.insert(id, old_inst.clone());
                    // Get current name and explicitly drop read lock before insert.
                    let current_name = {
                        let name_ref = self.runtime.index.device_name_index.get(&old_name);
                        let result = name_ref.as_ref().map(|e| *e.value()).unwrap_or_default();
                        drop(name_ref);
                        result
                    };
                    if current_name != id {
                        self.runtime
                            .index
                            .device_name_index
                            .insert(old_name.clone(), id);
                    }
                }
            },
        )
        .await
    }

    /// Remove many devices under a specific channel; optionally keep children; revert memory on driver error.
    ///
    /// Note: this method currently keeps the original behavior and does not yet move point/action subtrees
    /// into separate modules; that will be done in a follow-up split.
    pub async fn remove_devices(
        &self,
        channel_id: i32,
        ids: Vec<i32>,
        preserve_children: bool,
    ) -> NGResult<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let driver = self
            .runtime
            .index
            .channels
            .get(&channel_id)
            .map(|c| Arc::clone(&c.driver));
        let Some(driver) = driver else {
            return Ok(());
        };

        struct DevicesRemovedRecord {
            removed_devices: HashMap<i32, DeviceInstance>,
            _removed_names: Vec<(Arc<str>, i32)>,
            removed_children_points: HashMap<i32, Vec<Arc<dyn RuntimePoint>>>,
            removed_children_actions: HashMap<i32, Vec<Arc<dyn RuntimeAction>>>,
            removed_runtime_cfgs: Vec<(i32, Arc<dyn RuntimeDevice>)>,
        }

        let result = apply_with_revert(
            || {
                // Snapshots for revert.
                let mut removed_devices = HashMap::new();
                let mut removed_names = Vec::new();
                let mut removed_children_points = HashMap::new();
                let mut removed_children_actions = HashMap::new();
                let mut removed_runtime: Vec<(i32, Arc<dyn RuntimeDevice>)> = Vec::new();

                for id in ids.iter().copied() {
                    if let Some((_, dev)) = self.runtime.index.devices.remove(&id) {
                        removed_runtime.push((id, Arc::clone(&dev.config)));

                        // remove name index and channel mapping
                        let name: Arc<str> = Arc::from(dev.config.device_name());
                        self.runtime
                            .index
                            .device_name_index
                            .remove_if(&name, |_, v| *v == id);
                        self.runtime
                            .index
                            .remove_device_from_channel(channel_id, id);

                        // snapshot for revert
                        removed_names.push((name, id));
                        removed_devices.insert(id, dev.clone());

                        if !preserve_children {
                            if let Some((_, points)) = self.runtime.index.device_points.remove(&id)
                            {
                                let points_vec: Vec<Arc<dyn RuntimePoint>> =
                                    points.iter().cloned().collect();
                                for p in points_vec.iter() {
                                    self.runtime.index.remove_point_entry_by_id(p.id());
                                }
                                removed_children_points.insert(id, points_vec);
                            }
                            if let Some((_, actions)) =
                                self.runtime.index.device_actions.remove(&id)
                            {
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
            DeltaSink::new(&driver, channel_id),
            |rec| {
                // Revert: restore devices and indexes best-effort.
                for (id, inst) in rec.removed_devices.into_iter() {
                    let name: Arc<str> = Arc::from(inst.config.device_name());
                    self.runtime.index.devices.insert(id, inst.clone());
                    self.runtime
                        .index
                        .device_name_index
                        .insert(Arc::clone(&name), id);
                    self.runtime.index.add_device_to_channel(channel_id, id);
                }
                for (device_id, pts) in rec.removed_children_points.into_iter() {
                    self.runtime.index.set_device_points(device_id, pts);
                }
                for (device_id, acts) in rec.removed_children_actions.into_iter() {
                    self.runtime.index.set_device_actions(device_id, acts);
                }
            },
        )
        .await;

        // Keep hub snapshot consistent with final in-memory state.
        self.refresh_manager_snapshot_from_index().await;

        result
    }

    /// Add runtime points to a device and wait for driver to apply.
    /// On failure, revert in-memory changes.
    pub async fn add_points(&self, device_id: i32, points: Vec<PointModel>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        // Snapshot factory and device/driver.
        let (factory, channel_id) = match self
            .runtime
            .index
            .snapshot_driver_factory_for_device(device_id)
        {
            Some(v) => v,
            None => return Ok(()),
        };
        let (device, driver, _) = match self.runtime.index.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let channel_name = self
            .get_channel(device.channel_id())
            .map(|c| c.config.name().to_string())
            .unwrap_or_default();

        // Convert input models to runtime points.
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
                    self.runtime
                        .index
                        .upsert_point_entry(&channel_name, &device, &rp, None);
                    added.push(Arc::clone(&rp));
                    to_push.push(rp);
                }
                // Atomically append all points under one entry lock to avoid lost updates.
                self.runtime
                    .index
                    .mutate_device_points(device_id, |v| v.extend(to_push.into_iter()));

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
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                self.runtime.index.mutate_device_points(device_id, |v| {
                    v.retain(|p| !rec.added_ids.iter().any(|id| *id == p.id()))
                });
                for id in rec.added_ids.iter() {
                    self.runtime.index.remove_point_entry_by_id(*id);
                }
            },
        )
        .await?;

        // Control-plane path: refresh aggregated counts (points totals) for observability snapshots.
        self.refresh_manager_snapshot_from_index().await;
        Ok(())
    }

    /// Replace (upsert) runtime points on a device by id; wait for driver and revert on failure.
    pub async fn replace_points(&self, device_id: i32, points: Vec<PointModel>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }

        // Snapshot channel_id and factory via helper.
        let (factory, channel_id) = match self
            .runtime
            .index
            .snapshot_driver_factory_for_device(device_id)
        {
            Some(v) => v,
            None => return Ok(()),
        };

        let (device, driver, _) = match self.runtime.index.snapshot_device_and_driver(device_id) {
            Some(v) => v,
            None => return Ok(()),
        };
        let channel_name = self
            .get_channel(device.channel_id())
            .map(|c| c.config.name().to_string())
            .unwrap_or_default();

        // Convert models.
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
                    self.runtime
                        .index
                        .upsert_point_entry(&channel_name, &device, rp, None);
                }
                self.runtime
                    .index
                    .mutate_device_points(device_id, |current| {
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
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                let replaced_old = rec.replaced_old.clone();
                self.runtime
                    .index
                    .mutate_device_points(device_id, |current| {
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
                    self.runtime.index.remove_point_entry_by_id(*id);
                }
                for (_id, old) in replaced_old.iter() {
                    self.runtime
                        .index
                        .upsert_point_entry(&channel_name, &device, old, None);
                }
            },
        )
        .await?;

        self.refresh_manager_snapshot_from_index().await;
        Ok(())
    }

    /// Remove runtime points by id and wait for driver; revert in-memory on failure.
    pub async fn remove_points(&self, device_id: i32, point_ids: Vec<i32>) -> NGResult<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        // Convert ids to a set to avoid O(n*m) scans when removing many points.
        let point_id_set: HashSet<i32> = point_ids.iter().copied().collect();

        let (device, driver, channel_id) =
            match self.runtime.index.snapshot_device_and_driver(device_id) {
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
                self.runtime
                    .index
                    .mutate_device_points(device_id, |current| {
                        let mut kept: Vec<Arc<dyn RuntimePoint>> =
                            Vec::with_capacity(current.len());
                        for p in current.drain(..) {
                            if point_id_set.contains(&p.id()) {
                                removed.push(Arc::clone(&p));
                            } else {
                                kept.push(p);
                            }
                        }
                        *current = kept;
                    });
                for p in removed.iter() {
                    self.runtime.index.remove_point_entry_by_id(p.id());
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
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                if !rec.removed.is_empty() {
                    self.runtime
                        .index
                        .mutate_device_points(device_id, |current| {
                            current.extend(rec.removed.into_iter())
                        });
                    // Restore metadata entries best-effort.
                    if let Some(points) = self.runtime.index.device_points_slice(device_id) {
                        for p in points.iter() {
                            if point_id_set.contains(&p.id()) {
                                self.runtime.index.upsert_point_entry(
                                    &channel_name,
                                    &device,
                                    p,
                                    None,
                                );
                            }
                        }
                    }
                }
            },
        )
        .await?;

        self.refresh_manager_snapshot_from_index().await;
        Ok(())
    }

    /// Add actions to a device and wait for driver; revert on failure.
    pub async fn add_actions(&self, device_id: i32, actions: Vec<ActionModel>) -> NGResult<()> {
        if actions.is_empty() {
            return Ok(());
        }

        // Snapshot factory via helper.
        let (factory, channel_id) = match self
            .runtime
            .index
            .snapshot_driver_factory_for_device(device_id)
        {
            Some(v) => v,
            None => return Ok(()),
        };

        // Convert models to runtime actions.
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

        let (device, driver, _) = match self.runtime.index.snapshot_device_and_driver(device_id) {
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
                self.runtime
                    .index
                    .mutate_device_actions(device_id, |v| v.extend(to_push.into_iter()));
                let added_ids: Vec<i32> = added.iter().map(|a| a.id()).collect();
                ActionsAddedRecord { added, added_ids }
            },
            |rec| RuntimeDelta::ActionsChanged {
                device: Arc::clone(&device),
                added: rec.added.clone(),
                updated: Vec::new(),
                removed: Vec::new(),
            },
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                self.runtime.index.mutate_device_actions(device_id, |v| {
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

        // Snapshot factory via helper.
        let (factory, channel_id) = match self
            .runtime
            .index
            .snapshot_driver_factory_for_device(device_id)
        {
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

        let (device, driver, _) = match self.runtime.index.snapshot_device_and_driver(device_id) {
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
                self.runtime
                    .index
                    .mutate_device_actions(device_id, |current| {
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
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                self.runtime
                    .index
                    .mutate_device_actions(device_id, |current| {
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

        // Convert ids to a set to avoid O(n*m) scans when removing many actions.
        let action_id_set: HashSet<i32> = action_ids.iter().copied().collect();

        struct ActionsRemovedRecord {
            removed: Vec<Arc<dyn RuntimeAction>>,
        }

        let (device, driver, channel_id) =
            match self.runtime.index.snapshot_device_and_driver(device_id) {
                Some(v) => v,
                None => return Ok(()),
            };

        let result = apply_with_revert(
            || {
                let mut removed = Vec::new();
                self.runtime
                    .index
                    .mutate_device_actions(device_id, |current| {
                        let mut kept: Vec<Arc<dyn RuntimeAction>> =
                            Vec::with_capacity(current.len());
                        for a in current.drain(..) {
                            if action_id_set.contains(&a.id()) {
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
            DeltaSink::with_broadcast(&driver, channel_id, &self.runtime.index.runtime_delta_tx),
            |rec| {
                if !rec.removed.is_empty() {
                    self.runtime
                        .index
                        .mutate_device_actions(device_id, |current| {
                            current.extend(rec.removed.into_iter())
                        });
                }
            },
        )
        .await;

        // Control-plane path: refresh aggregated counts (actions totals) for observability snapshots.
        //
        // IMPORTANT: refresh even on error (after revert) to keep hub snapshot consistent with
        // the final in-memory runtime index.
        self.refresh_manager_snapshot_from_index().await;

        result
    }
}

// ---------------------------------------------------------------------------
// Delta delivery infrastructure
// ---------------------------------------------------------------------------

/// Destination for `RuntimeDelta` delivery after an in-memory mutation.
///
/// Encapsulates the driver handle, channel id, and an optional broadcast sender.
/// This eliminates per-call-site boilerplate: callers construct a `DeltaSink` once
/// and pass it to `apply_with_revert`, which handles broadcast + best-effort driver
/// notification + unreachable tolerance uniformly.
struct DeltaSink<'a> {
    driver: &'a Arc<dyn Driver>,
    channel_id: i32,
    broadcast_tx: Option<&'a broadcast::Sender<RuntimeDelta>>,
}

impl<'a> DeltaSink<'a> {
    /// Driver-only sink (devices mutations that don't need northward broadcast).
    #[inline]
    fn new(driver: &'a Arc<dyn Driver>, channel_id: i32) -> Self {
        Self {
            driver,
            channel_id,
            broadcast_tx: None,
        }
    }

    /// Driver + northward broadcast sink (points and actions mutations).
    #[inline]
    fn with_broadcast(
        driver: &'a Arc<dyn Driver>,
        channel_id: i32,
        broadcast_tx: &'a broadcast::Sender<RuntimeDelta>,
    ) -> Self {
        Self {
            driver,
            channel_id,
            broadcast_tx: Some(broadcast_tx),
        }
    }

    /// Deliver a delta: broadcast to northward subscribers, then send to the driver.
    ///
    /// `DriverError::Unreachable` (mailbox closed / runtime stopped) is treated as
    /// non-fatal — the in-memory mutation stands and the driver will pick up the full
    /// state on its next start.
    async fn deliver(&self, delta: RuntimeDelta) -> NGResult<()> {
        if let Some(tx) = self.broadcast_tx {
            let _ = tx.send(delta.clone());
        }
        match self.driver.apply_runtime_delta(delta).await {
            Ok(()) => Ok(()),
            Err(e) if e.is_unreachable() => {
                tracing::debug!(
                    channel_id = self.channel_id,
                    error = %e,
                    "Driver unreachable, runtime delta deferred to next start",
                );
                Ok(())
            }
            Err(e) => Err(NGError::DriverError(e.to_string())),
        }
    }
}

/// Execute a memory change, build a delta, deliver it, and revert on failure.
///
/// # Type parameters
/// - `R`: record type produced by the memory mutation (carries revert data).
/// - `T`: delta payload type — either `RuntimeDelta` or `Option<RuntimeDelta>`.
///   When `Option`, a `None` payload skips delivery entirely.
///
/// # Flow
/// 1. `apply_mem()` — mutate in-memory indexes, return a record.
/// 2. `build(&record)` — construct the delta payload from the record.
/// 3. `sink.deliver(payload)` — broadcast + send to driver (best-effort for unreachable).
/// 4. On error → `revert_mem(record)` rolls back the in-memory changes.
async fn apply_with_revert<R, T, BuildFn, RevertFn>(
    apply_mem: impl FnOnce() -> R,
    build: BuildFn,
    sink: DeltaSink<'_>,
    revert_mem: RevertFn,
) -> NGResult<()>
where
    T: IntoDelta,
    BuildFn: FnOnce(&R) -> T,
    RevertFn: FnOnce(R),
{
    let record = apply_mem();
    let payload = build(&record);
    if let Some(delta) = payload.into_delta() {
        if let Err(e) = sink.deliver(delta).await {
            revert_mem(record);
            return Err(e);
        }
    }
    Ok(())
}

/// Trait to unify `RuntimeDelta` and `Option<RuntimeDelta>` as build outputs.
///
/// This lets `apply_with_revert` accept both forms without requiring callers to
/// wrap every non-optional delta in `Some(...)`.
trait IntoDelta {
    fn into_delta(self) -> Option<RuntimeDelta>;
}

impl IntoDelta for RuntimeDelta {
    #[inline]
    fn into_delta(self) -> Option<RuntimeDelta> {
        Some(self)
    }
}

impl IntoDelta for Option<RuntimeDelta> {
    #[inline]
    fn into_delta(self) -> Option<RuntimeDelta> {
        self
    }
}
