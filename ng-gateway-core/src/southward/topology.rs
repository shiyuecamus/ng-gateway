//! Southward topology initialization.
//!
//! This module owns the topology bootstrap paths for `NGSouthwardManager`:
//! - initialize channels with full topology context (devices/points/actions)
//! - populate runtime indexes from assembled topology
//! - create channel driver instances with correct init contexts
//!
//! # Performance notes
//! - Topology initialization is control-plane; allocations are acceptable but should still be bounded.
//! - The per-channel initialization is run concurrently, while per-channel device/point/action conversion
//!   is kept sequential to reuse `DriverFactory` and reduce contention.

use super::{
    super::southward::{ChannelInitEntry, DeviceInitTriple, SouthwardDataBus},
    observability::ChannelBoundTransportMeter,
    observer::SouthwardChannelObserverFactory,
    publisher::MpscNorthwardPublisher,
    ChannelInstance, NGSouthwardManager, DeviceInstance,
};
use chrono::Utc;
use futures::stream::{self, StreamExt};
use ng_gateway_common::metrics::southward::SouthwardChannelMetricHandles;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::entities::prelude::ChannelModel;
use ng_gateway_sdk::{
    ConnectionState, Driver, Phase, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimePoint,
    SouthwardInitContext,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tracing::{error, info};

impl NGSouthwardManager {
    /// Initialize manager from a fully assembled topology in a single high-performance pass.
    ///
    /// Best-effort behavior: channel/device/point/action failures are isolated and logged; others continue.
    /// Concurrency model: per-channel tasks run concurrently; within each task, devices/points/actions are processed sequentially
    /// to leverage a shared `DriverFactory` and avoid contention. This minimizes allocations and roundtrips.
    pub async fn initialize_topology(
        &self,
        topology: Vec<ChannelInitEntry>,
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> NGResult<()> {
        let successful_channels = Arc::new(AtomicUsize::new(0));
        let failed_channels = Arc::new(AtomicUsize::new(0));
        let total_devices_ok = Arc::new(AtomicUsize::new(0));
        let total_points_ok = Arc::new(AtomicUsize::new(0));
        let total_actions_ok = Arc::new(AtomicUsize::new(0));

        // Run per-channel concurrently while reusing extracted initializer APIs.
        stream::iter(topology)
            .for_each_concurrent(None, |(channel_config, dev_triples)| async {
                let outbound = Arc::clone(southward_data_bus);
                // Initialize channel with full topology context and commit.
                match self
                    .initialize_channel_with_topology(channel_config, &dev_triples, &outbound)
                    .await
                {
                    Ok(_) => {
                        successful_channels.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        error!(error = %e, "Failed to initialize channel");
                        failed_channels.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                }

                // Populate runtime indexes from topology without notifying driver (already configured via init context).
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

        // Refresh manager-level snapshot state in the hub after topology initialization.
        self.refresh_manager_snapshot_from_index().await;

        Ok(())
    }

    /// Initialize a single channel using full topology to build the driver's init context.
    /// Then insert the channel into indexes. Devices/points/actions will be populated into
    /// indexes separately without driver deltas.
    async fn initialize_channel_with_topology(
        &self,
        config: ChannelModel,
        dev_triples: &[DeviceInitTriple],
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> NGResult<()> {
        let instance = self
            .create_channel_instance_with_topology(&config, dev_triples, southward_data_bus)
            .await?;
        self.runtime
            .index
            .channels
            .insert(instance.config.id(), instance);
        Ok(())
    }

    /// Create a single channel instance with driver (uninitialized).
    ///
    /// The driver is created but not initialized. Call `start_channel` to connect it.
    pub async fn create_channel_instance(
        &self,
        config: &ChannelModel,
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> NGResult<ChannelInstance> {
        // Get driver factory by driver_id.
        let driver_factory = self
            .southward_registry
            .get(&config.driver_id)
            .map(|entry| entry.value().clone())
            .ok_or(NGError::DriverError(format!(
                "Unknown driver id: {}",
                config.driver_id
            )))?;

        // Build runtime channel first (needed for driver context).
        let config = driver_factory
            .convert_runtime_channel(config.clone().into())
            .map_err(|e| NGError::DriverError(e.to_string()))?;

        // Register metrics handles early so transport metering can be bound without lookups.
        let driver_label: Arc<str> = Arc::<str>::from(config.driver_id().to_string());
        let prom = self
            .metrics_hub
            .register_southward_channel_metrics(config.id(), Arc::clone(&driver_label))?;

        // Build init context with best-effort preload from current indexes.
        let ctx = self.build_channel_runtime_context(
            Arc::clone(&config),
            Arc::clone(&prom),
            southward_data_bus,
        );

        // Create driver (Box) and convert to Arc.
        let driver = driver_factory
            .create_driver(ctx)
            .map_err(|e| NGError::DriverError(e.to_string()))?;
        let driver: Arc<dyn Driver> = Arc::from(driver);

        // Defer connection to the unified start phase after devices/points are loaded.
        let connection_state = ConnectionState::arc_now(Phase::Disconnected, 0);

        let now = Utc::now();
        let status = config.status();
        Ok(ChannelInstance {
            driver,
            driver_factory,
            config,
            state: connection_state,
            status,
            prom,
            driver_label,
            created_at: now,
            last_activity: now,
        })
    }

    /// Create a channel instance where the driver is initialized with full topology context.
    pub async fn create_channel_instance_with_topology(
        &self,
        config: &ChannelModel,
        dev_triples: &[DeviceInitTriple],
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> NGResult<ChannelInstance> {
        // Get driver factory by driver_id.
        let driver_factory = self
            .southward_registry
            .get(&config.driver_id)
            .map(|entry| entry.value().clone())
            .ok_or(NGError::DriverError(format!(
                "Unknown driver id: {}",
                config.driver_id
            )))?;

        // Build runtime channel first (needed for driver context).
        let runtime_channel = driver_factory
            .convert_runtime_channel(config.clone().into())
            .map_err(|e| NGError::DriverError(e.to_string()))?;

        // Register metrics handles early so transport metering can be bound without lookups.
        let driver_label: Arc<str> = Arc::<str>::from(runtime_channel.driver_id().to_string());
        let prom = self
            .metrics_hub
            .register_southward_channel_metrics(config.id, Arc::clone(&driver_label))?;

        // Convert devices and points from the provided topology into runtime forms
        // to supply a complete init context without relying on runtime indexes.
        let mut devices: Vec<Arc<dyn RuntimeDevice>> = Vec::with_capacity(dev_triples.len());
        let mut points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>> =
            HashMap::with_capacity(dev_triples.len());
        for (dev, pts, _acts) in dev_triples.iter() {
            // Only include devices that belong to this channel.
            if dev.channel_id != config.id {
                continue;
            }
            if let Ok(rdev) = driver_factory.convert_runtime_device(dev.clone().into()) {
                let device_id = rdev.id();
                // Convert points for this device.
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

        let ctx = SouthwardInitContext {
            devices,
            points_by_device,
            runtime_channel: Arc::clone(&runtime_channel),
            publisher: Arc::new(MpscNorthwardPublisher::new(
                Arc::clone(southward_data_bus),
                Arc::clone(&prom),
            )),
            channel_id: runtime_channel.id(),
            transport_meter: Arc::new(ChannelBoundTransportMeter::new(Arc::clone(&prom))),
            observer_factory: Arc::new(SouthwardChannelObserverFactory::new(
                runtime_channel.id(),
                Arc::clone(&prom),
                Arc::clone(&self.metrics_hub),
                Arc::clone(&self.runtime.index),
                Arc::clone(southward_data_bus),
            )),
        };

        // Create driver and wrap.
        let driver = driver_factory
            .create_driver(ctx)
            .map_err(|e| NGError::DriverError(e.to_string()))?;
        let driver: Arc<dyn Driver> = Arc::from(driver);

        // Defer connection; status/state copied from channel config.
        let connection_state = ConnectionState::arc_now(Phase::Disconnected, 0);
        let now = Utc::now();
        let status = runtime_channel.status();
        Ok(ChannelInstance {
            driver,
            driver_factory,
            config: runtime_channel,
            state: connection_state,
            status,
            prom,
            driver_label,
            created_at: now,
            last_activity: now,
        })
    }

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
            // Get channel instance to obtain factory and driver binding.
            let channel = match self.runtime.index.channels.get(&dev.channel_id) {
                Some(c) => c,
                None => continue,
            };

            // Convert runtime device and bind to channel driver.
            let runtime_device = match channel.driver_factory.convert_runtime_device(dev.into()) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            let device_id = runtime_device.id();
            let device_name: Arc<str> = Arc::from(runtime_device.device_name());
            let instance = DeviceInstance {
                config: Arc::clone(&runtime_device),
                state: ng_gateway_sdk::DeviceState::Active,
                status: runtime_device.status(),
                driver: Arc::clone(&channel.driver),
                last_collection: None,
                last_data_change: None,
                created_at: Utc::now(),
            };

            // Insert device + mappings.
            self.runtime.index.devices.insert(device_id, instance);
            self.runtime
                .index
                .add_device_to_channel(channel.config.id(), device_id);
            self.runtime
                .index
                .device_name_index
                .insert(Arc::clone(&device_name), device_id);
            devices_ok += 1;

            // Convert and insert points.
            if !pts.is_empty() {
                let channel_name = channel.config.name();
                let mut rpoints = Vec::with_capacity(pts.len());
                for p in pts.into_iter() {
                    match channel.driver_factory.convert_runtime_point(p.into()) {
                        Ok(rp) => {
                            // Build unified point entry (point + meta) indexes.
                            self.runtime.index.upsert_point_entry(
                                channel_name,
                                &runtime_device,
                                &rp,
                                None,
                            );
                            rpoints.push(rp);
                        }
                        Err(e) => {
                            tracing::error!("Error converting point: {:?}", e);
                        }
                    }
                }
                if !rpoints.is_empty() {
                    points_ok += rpoints.len();
                    self.runtime.index.set_device_points(device_id, rpoints);
                }
            }

            // Convert and insert actions.
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
                    self.runtime.index.set_device_actions(device_id, ractions);
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
        prom: Arc<SouthwardChannelMetricHandles>,
        southward_data_bus: &Arc<SouthwardDataBus>,
    ) -> SouthwardInitContext {
        let channel_id = runtime_channel.id();
        // Collect device ids already bound to this channel in indexes (if any).
        let device_ids: Vec<i32> = self.runtime.index.channel_device_ids(channel_id);

        let mut devices = Vec::with_capacity(device_ids.len());
        let mut points_by_device = HashMap::with_capacity(device_ids.len());

        for id in device_ids.into_iter() {
            if let Some(dev) = self.runtime.index.devices.get(&id) {
                devices.push(Arc::clone(&dev.config));
                let points_vec = self
                    .runtime
                    .index
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
            publisher: Arc::new(MpscNorthwardPublisher::new(
                Arc::clone(southward_data_bus),
                Arc::clone(&prom),
            )),
            channel_id,
            transport_meter: Arc::new(ChannelBoundTransportMeter::new(Arc::clone(&prom))),
            observer_factory: Arc::new(SouthwardChannelObserverFactory::new(
                channel_id,
                Arc::clone(&prom),
                Arc::clone(&self.metrics_hub),
                Arc::clone(&self.runtime.index),
                Arc::clone(southward_data_bus),
            )),
        }
    }
}
