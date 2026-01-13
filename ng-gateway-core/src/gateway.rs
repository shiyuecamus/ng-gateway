//! Next-generation NG Gateway Core
//!
//! This is a complete rewrite of the gateway system with focus on:
//! - High performance and scalability
//! - Elegant architecture with clear separation of concerns
//! - Efficient resource utilization
//! - Real-time data processing capabilities
//! - Comprehensive monitoring and diagnostics

use crate::{
    collector::Collector,
    commands::{gateway_command_registry, GetStatusHandler},
    lifecycle::StartPolicy,
    northward::NGNorthwardManager,
    realtime::NGRealtimeMonitorHub,
    southward::NGSouthwardManager,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures::{StreamExt, TryStreamExt};
use ng_gateway_common::NGAppContext;
use ng_gateway_error::{init::InitContextError, storage::StorageError, NGError, NGResult};
use ng_gateway_models::{
    core::metrics::{GatewayMetrics, GatewayStatus, SystemInfo},
    domain::prelude::{
        ChangeAppStatus, ChangeChannelStatus, ChangeDeviceStatus, NewAction, NewApp, NewAppSub,
        NewChannel, NewDevice, NewPoint, UpdateAction, UpdateApp, UpdateAppSub, UpdateChannel,
        UpdateDevice, UpdatePoint,
    },
    entities::prelude::{
        ActionModel, AppModel, AppSubModel, ChannelModel, DeviceModel, PointModel,
    },
    enums::{
        common::{EntityType, OsArch, OsType, Status},
        core::GatewayState,
    },
    settings::Settings,
    ActionRuntimeCmd, AppRuntimeCmd, AppSubRuntimeCmd, ChannelRuntimeCmd, DbManager,
    DeviceRuntimeCmd, DriverRuntimeCmd, Gateway, NorthwardManager, PluginRuntimeCmd,
    PointRuntimeCmd, RealtimeMonitorHub, SouthwardManager,
};
use ng_gateway_repository::{
    get_db_connection, ActionRepository, AppRepository, AppSubRepository, ChannelRepository,
    DeviceRepository, DriverRepository, PluginRepository, PointRepository,
};
use ng_gateway_sdk::{
    validate_and_resolve_action_inputs, AccessMode, ClientRpcResponse, Command, DriverLoader,
    DriverRegistry, NorthwardData, NorthwardEvent, NorthwardLoader, TargetType, WritePoint,
    WritePointErrorKind, WritePointResponse,
};
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{self, Duration, Instant},
};
use sysinfo::{Disks, System};
use tokio::{
    fs,
    sync::{mpsc, RwLock, Semaphore},
    time::{interval, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

// --- Type aliases to reduce type complexity and improve readability ---
type DataSender = mpsc::Sender<Arc<NorthwardData>>;
type DataReceiver = mpsc::Receiver<Arc<NorthwardData>>;
type EventsSender = mpsc::Sender<(i32, NorthwardEvent)>;
type EventsReceiver = mpsc::Receiver<(i32, NorthwardEvent)>;
type Shared<T> = Arc<RwLock<T>>;
type SharedReceiver = Shared<Option<DataReceiver>>;
type SharedEventsReceiver = Shared<Option<EventsReceiver>>;

/// Per-channel write serialization primitives for control-plane write paths.
///
/// Design goals:
/// - O(1) lookup per write
/// - No global lock
/// - Fair-ish FIFO acquisition (Tokio semaphore)
#[derive(Default)]
struct WriteSerializers {
    per_channel: DashMap<i32, Arc<Semaphore>>,
}

impl WriteSerializers {
    #[inline]
    fn semaphore_for(&self, channel_id: i32) -> Arc<Semaphore> {
        self.per_channel
            .entry(channel_id)
            .or_insert_with(|| Arc::new(Semaphore::new(1)))
            .clone()
    }
}

#[allow(unused)]
/// Next-generation gateway implementation with high-performance architecture
pub struct NGGateway {
    /// Driver registry and loader
    driver_registry: DriverRegistry,
    driver_loader: DriverLoader,

    /// Northward plugin loader
    northward_loader: Arc<NorthwardLoader>,

    /// High-performance southward manager
    southward_manager: Arc<NGSouthwardManager>,

    /// High-performance northward manager
    northward_manager: Arc<NGNorthwardManager>,

    /// Realtime monitor hub for device-level data broadcasting
    monitor_hub: Arc<NGRealtimeMonitorHub>,

    /// High-performance collection engine
    collector: Arc<Collector>,

    /// Gateway state management
    state: Arc<RwLock<GatewayState>>,

    /// Performance metrics
    metrics: Arc<RwLock<GatewayMetrics>>,

    /// Data batch channel (bounded, single consumer)
    data_tx: DataSender,
    /// Data batch receiver (bounded, single consumer, taken on forwarding start)
    data_rx: SharedReceiver,

    /// Northward events channel (bounded, single consumer)
    northward_events_tx: EventsSender,
    /// Northward events receiver (bounded, single consumer, taken on event processor start)
    northward_events_rx: SharedEventsReceiver,

    /// Gateway start time for uptime calculation
    start_time: Option<DateTime<Utc>>,

    /// Forwarding task handle for graceful shutdown
    forwarding_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Metrics task handle for graceful shutdown
    metrics_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Northward events task handle for graceful shutdown
    northward_events_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Cancellation token for tasks
    shutdown_token: CancellationToken,
}

#[async_trait]
impl Gateway for NGGateway {
    #[instrument(name = "gateway-init", skip_all)]
    async fn init(
        settings: &Settings,
        db_manager: Arc<dyn DbManager>,
    ) -> NGResult<Arc<Self>, InitContextError> {
        info!("Initializing NGGateway with enhanced architecture");

        let conn = db_manager.get_connection().map_err(|e| {
            InitContextError::Primitive(format!("Failed to get database connection: {e}"))
        })?;

        let (os_type, os_arch) = (current_os_type(), current_os_arch());

        // Create bounded data batch channel (single consumer) to propagate backpressure
        let (data_tx, data_rx): (DataSender, DataReceiver) =
            mpsc::channel(settings.general.collector.outbound_queue_capacity);

        // Create bounded northward events channel (single consumer) for event routing
        let (northward_events_tx, northward_events_rx): (EventsSender, EventsReceiver) =
            mpsc::channel(settings.general.northward.queue_capacity);

        // Initialize driver system
        let driver_registry = Arc::new(DashMap::new());
        let driver_loader = DriverLoader::new(driver_registry.clone());

        // Load enabled drivers for current platform from DB and register
        let enabled = DriverRepository::find_by_platform(os_type, os_arch, Some(&conn))
            .await
            .map_err(|e| InitContextError::Primitive(format!("Failed to query drivers: {e}")))?;
        // Pre-filter: path existence and prepare id-path pairs
        let id_paths: Vec<(i32, String)> = enabled
            .into_iter()
            .filter_map(|m| {
                let path_str = m.path.clone();
                let p = Path::new(&path_str);
                if !p.exists() || !p.is_file() {
                    return None;
                }
                Some((m.id, path_str))
            })
            .collect();
        driver_loader.load_all(&id_paths).await;

        // Initialize southward manager
        let southward_manager = Arc::new(NGSouthwardManager::new(Arc::clone(&driver_registry)));

        // Initialize channels and devices
        let southward_config = Self::load_southward_config(&conn).await.map_err(|e| {
            InitContextError::Primitive(format!("Failed to load configuration: {e}"))
        })?;

        southward_manager
            .initialize_topology(southward_config, &data_tx)
            .await
            .map_err(|e| {
                InitContextError::Primitive(format!("Failed to initialize data manager: {e}"))
            })?;

        // Start channels (monitors are spawned inside)
        southward_manager
            .start_channels(&data_tx)
            .await
            .map_err(|e| InitContextError::Primitive(format!("Failed to start channels: {e}")))?;

        // Initialize northward system
        let northward_registry = Arc::new(DashMap::new());
        let northward_loader = Arc::new(NorthwardLoader::new(Arc::clone(&northward_registry)));

        // Load enabled northward plugins for current platform from DB
        let plugins = PluginRepository::find_by_platform(os_type, os_arch, Some(&conn))
            .await
            .map_err(|e| {
                InitContextError::Primitive(format!("Failed to query northward plugins: {e}"))
            })?;

        let to_load: Vec<(i32, String)> = plugins
            .into_iter()
            .filter_map(|m| {
                let path_str = m.path.clone();
                let p = Path::new(&path_str);
                if !p.exists() || !p.is_file() {
                    return None;
                }
                Some((m.id, path_str))
            })
            .collect();
        northward_loader.load_all(&to_load).await;

        // Initialize northward manager
        let northward_manager = NGNorthwardManager::new(
            Arc::clone(&northward_registry),
            Arc::clone(&southward_manager),
        );

        // Load and initialize northward topology with global events channel
        let northward_config = Self::load_northward_config(&conn).await.map_err(|e| {
            InitContextError::Primitive(format!("Failed to load northward configuration: {e}"))
        })?;

        northward_manager
            .initialize_topology(northward_config, &conn, &northward_events_tx)
            .await
            .map_err(|e| {
                InitContextError::Primitive(format!("Failed to initialize northward topology: {e}"))
            })?;

        let gateway = Arc::new(Self {
            northward_manager,
            northward_loader,
            driver_registry,
            driver_loader,
            southward_manager: Arc::clone(&southward_manager),
            collector: Arc::new(Collector::new(
                settings.general.collector,
                southward_manager,
                data_tx.clone(),
                CancellationToken::new(),
            )),
            state: Arc::new(RwLock::new(GatewayState::Uninitialized)),
            metrics: Arc::new(RwLock::new(GatewayMetrics::default())),
            data_tx,
            data_rx: Arc::new(RwLock::new(Some(data_rx))),
            northward_events_tx,
            northward_events_rx: Arc::new(RwLock::new(Some(northward_events_rx))),
            monitor_hub: Arc::new(NGRealtimeMonitorHub::new()),
            start_time: Some(Utc::now()),
            forwarding_task: Arc::new(RwLock::new(None)),
            metrics_task: Arc::new(RwLock::new(None)),
            northward_events_task: Arc::new(RwLock::new(None)),
            shutdown_token: CancellationToken::new(),
        });

        // Initialize the gateway
        *gateway.state.write().await = GatewayState::Initializing;

        // Register gateway commands with weak self
        Self::register_gateway_commands_with(&gateway).await;

        // Start collection engine
        gateway.collector.start().await.map_err(|e| {
            InitContextError::Primitive(format!("Failed to start collection engine: {e}"))
        })?;

        // Set up data forwarding
        gateway.setup_data_forwarding().await.map_err(|e| {
            InitContextError::Primitive(format!("Failed to setup data forwarding: {e}"))
        })?;

        // Start northward event processor
        gateway
            .start_northward_event_processor()
            .await
            .map_err(|e| {
                InitContextError::Primitive(format!(
                    "Failed to start northward event processor: {e}"
                ))
            })?;

        // Start metrics collection
        gateway.start_metrics_collection().await.map_err(|e| {
            InitContextError::Primitive(format!("Failed to start metrics collection: {e}"))
        })?;

        *gateway.state.write().await = GatewayState::Running;

        info!("Gateway initialization completed");
        Ok(gateway)
    }

    #[inline]
    #[instrument(name = "gateway-stop", skip_all)]
    async fn stop(&self) -> NGResult<()> {
        let current_state = self.state.read().await.clone();
        match current_state {
            GatewayState::Stopped => {
                warn!("Gateway is already stopped");
                return Ok(());
            }
            GatewayState::Stopping => {
                warn!("Gateway is already stopping");
                return Ok(());
            }
            _ => {}
        }

        *self.state.write().await = GatewayState::Stopping;

        // Cancel tasks early
        self.shutdown_token.cancel();

        // Wait for tasks to finish with timeout; abort if exceeding to prevent hang
        if let Some(handle) = self.forwarding_task.write().await.take() {
            let mut handle = handle;
            tokio::select! {
                _ = &mut handle => {}
                _ = sleep(time::Duration::from_secs(2)) => {
                    handle.abort();
                }
            }
        }
        if let Some(handle) = self.metrics_task.write().await.take() {
            let mut handle = handle;
            tokio::select! {
                _ = &mut handle => {}
                _ = sleep(time::Duration::from_secs(2)) => {
                    handle.abort();
                }
            }
        }
        if let Some(handle) = self.northward_events_task.write().await.take() {
            let mut handle = handle;
            tokio::select! {
                _ = &mut handle => {}
                _ = sleep(time::Duration::from_secs(2)) => {
                    handle.abort();
                }
            }
        }

        // Stop collection engine
        if let Err(e) = self.collector.stop().await {
            error!(error=%e, "Failed to stop collection engine");
        }

        // Shutdown channel manager
        if let Err(e) = self.southward_manager.shutdown().await {
            error!(error=%e, "Failed to shutdown channel manager");
        }

        // Shutdown northward manager
        if let Err(e) = self.northward_manager.shutdown().await {
            error!(error=%e, "Failed to shutdown northward manager");
        }

        *self.state.write().await = GatewayState::Stopped;

        info!("✅ NGGateway stopped successfully");
        Ok(())
    }

    #[inline]
    fn southward_manager(&self) -> Arc<dyn SouthwardManager> {
        Arc::clone(&self.southward_manager) as Arc<dyn SouthwardManager>
    }

    #[inline]
    fn northward_manager(&self) -> Arc<dyn NorthwardManager> {
        Arc::clone(&self.northward_manager) as Arc<dyn NorthwardManager>
    }

    #[inline]
    fn realtime_monitor_hub(&self) -> Arc<dyn RealtimeMonitorHub> {
        Arc::clone(&self.monitor_hub) as Arc<dyn RealtimeMonitorHub>
    }
}

#[async_trait]
impl ChannelRuntimeCmd for NGGateway {
    #[inline]
    async fn create_channel(&self, channel: NewChannel) -> NGResult<()> {
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;

        info!("Creating channel: {:?}", channel);

        let created =
            ChannelRepository::create::<DatabaseConnection>(channel.into_active_model(), None)
                .await?;

        match self
            .southward_manager
            .create_and_start_channel(
                &created,
                &self.data_tx,
                StartPolicy::SyncWaitConnected {
                    timeout_ms: settings.general.southward.driver_sync_start_timeout_ms,
                },
            )
            .await
        {
            Ok(_) => {
                // Register with collector after successful start
                self.collector.add_channel(created.id).await?;
            }
            Err(e) => {
                warn!("Failed to create and start channel: {:?}", e);
                ChannelRepository::update::<DatabaseConnection>(
                    ChangeChannelStatus {
                        id: created.id,
                        status: Status::Disabled,
                    }
                    .into_active_model(),
                    None,
                )
                .await?;
            }
        }
        Ok(())
    }

    #[inline]
    async fn update_channel(&self, channel: UpdateChannel) -> NGResult<()> {
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;

        let previous = match ChannelRepository::find_by_id(channel.id).await? {
            Some(channel) => channel,
            None => return Err(NGError::Error("Channel not found".to_string())),
        };

        let updated =
            ChannelRepository::update::<DatabaseConnection>(channel.into_active_model(), None)
                .await?;

        match updated.status {
            Status::Disabled => {
                self.southward_manager.stop_channel(updated.id, false).await;

                // Refresh instance to reflect metadata changes while disabled
                if let Err(e) = self
                    .southward_manager
                    .replace_channel_instance(&updated, &self.data_tx)
                    .await
                {
                    let _ = ChannelRepository::update::<DatabaseConnection>(
                        previous.into_active_model(),
                        None,
                    )
                    .await?;
                    return Err(e);
                }
            }
            Status::Enabled => {
                // Restart synchronously and update collector
                // Restart synchronously and update collector
                if let Err(e) = self
                    .southward_manager
                    .restart_channel(
                        &updated,
                        &self.data_tx,
                        settings.general.southward.driver_sync_start_timeout_ms,
                    )
                    .await
                {
                    let _ = ChannelRepository::update::<DatabaseConnection>(
                        previous.into_active_model(),
                        None,
                    )
                    .await?;
                    return Err(e);
                }
                if let Err(e) = self.collector.restart_channel(updated.id).await {
                    let _ = ChannelRepository::update::<DatabaseConnection>(
                        previous.into_active_model(),
                        None,
                    )
                    .await?;
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    #[inline]
    async fn delete_channel(&self, channel_id: i32) -> NGResult<()> {
        self.collector.remove_channel(channel_id).await;
        self.southward_manager.stop_channel(channel_id, true).await;
        ChannelRepository::delete_deep(channel_id)
            .await
            .map_err(NGError::StorageError)?;
        Ok(())
    }

    #[inline]
    async fn change_channel_status(&self, channel: ChannelModel, status: Status) -> NGResult<()> {
        match status {
            Status::Disabled => {
                self.collector.remove_channel(channel.id).await;
                self.southward_manager.stop_channel(channel.id, false).await;
                self.southward_manager
                    .change_channel_status(channel.id, Status::Disabled.into());
            }
            Status::Enabled => {
                let ctx = NGAppContext::instance().await;
                let settings = ctx.settings()?;

                self.collector.add_channel(channel.id).await?;
                self.southward_manager
                    .restart_channel(
                        &channel,
                        &self.data_tx,
                        settings.general.southward.driver_sync_start_timeout_ms,
                    )
                    .await?;
                self.southward_manager
                    .change_channel_status(channel.id, Status::Enabled.into());
            }
        }
        ChannelRepository::update::<DatabaseConnection>(
            ChangeChannelStatus {
                id: channel.id,
                status,
            }
            .into_active_model(),
            None,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl DeviceRuntimeCmd for NGGateway {
    #[inline]
    async fn create_devices(&self, devices: Vec<NewDevice>) -> NGResult<()> {
        if devices.is_empty() {
            return Ok(());
        }
        // DB insert many
        let created = DeviceRepository::create_many::<DatabaseConnection>(
            devices.into_iter().map(|d| d.into_active_model()).collect(),
            None,
        )
        .await?;

        let mut grouped: HashMap<i32, Vec<DeviceModel>> = HashMap::new();
        for dev in created.into_iter() {
            grouped.entry(dev.channel_id).or_default().push(dev);
        }
        for (channel_id, devices) in grouped.into_iter() {
            let ids: Vec<i32> = devices.iter().map(|d| d.id).collect();
            if let Err(e) = self
                .southward_manager
                .add_devices(channel_id, devices)
                .await
            {
                let _ = DeviceRepository::delete_by_ids::<DatabaseConnection>(ids, None).await;
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn update_devices(&self, devices: Vec<UpdateDevice>) -> NGResult<()> {
        if devices.is_empty() {
            return Ok(());
        }

        // Step 1: DB updates (sequential for simpler compensation)
        let updated: Vec<DeviceModel> = tokio_stream::iter(devices.into_iter())
            .map(|u| DeviceRepository::update::<DatabaseConnection>(u.into_active_model(), None))
            .buffer_unordered(16)
            .try_collect()
            .await?;

        let mut grouped: HashMap<i32, Vec<DeviceModel>> = HashMap::new();
        for dev in updated.into_iter() {
            grouped.entry(dev.channel_id).or_default().push(dev);
        }

        // Step 2: Apply to runtime per channel; compensate DB on any failure
        for (channel_id, devices) in grouped.into_iter() {
            let ids: Vec<i32> = devices.iter().map(|d| d.id).collect();
            if let Err(e) = self
                .southward_manager
                .replace_devices(channel_id, devices)
                .await
            {
                let _ = DeviceRepository::delete_by_ids::<DatabaseConnection>(ids, None).await;
                return Err(e);
            }
        }

        Ok(())
    }

    #[inline]
    async fn delete_devices(&self, ids: Vec<i32>) -> NGResult<()> {
        if ids.is_empty() {
            return Ok(());
        }

        // Also fetch points/actions under these devices for compensation
        let mut points_by_device: HashMap<i32, Vec<PointModel>> = HashMap::new();
        let mut actions_by_device: HashMap<i32, Vec<ActionModel>> = HashMap::new();
        // DB delete children then devices
        for id in ids.iter().copied() {
            let deleted_points =
                PointRepository::delete_by_device_id::<DatabaseConnection>(id, None).await?;
            let deleted_actions =
                ActionRepository::delete_by_device_id::<DatabaseConnection>(id, None).await?;
            points_by_device
                .entry(id)
                .or_default()
                .extend(deleted_points.into_iter());
            actions_by_device
                .entry(id)
                .or_default()
                .extend(deleted_actions.into_iter());
        }

        let deleted_devices =
            DeviceRepository::delete_by_ids::<DatabaseConnection>(ids.clone(), None).await?;
        // Group by channel and apply removals per channel; on failure, restore DB for that group
        let mut grouped_del: HashMap<i32, Vec<DeviceModel>> = HashMap::new();
        for dev in deleted_devices.into_iter() {
            grouped_del.entry(dev.channel_id).or_default().push(dev);
        }
        for (channel_id, models) in grouped_del.into_iter() {
            let ids_this: Vec<i32> = models.iter().map(|m| m.id).collect();
            if let Err(e) = self
                .southward_manager
                .remove_devices(channel_id, ids_this.clone(), true)
                .await
            {
                // recreate DB rows for that device group
                for dev in models.into_iter() {
                    if let Some(acts) = actions_by_device.get(&dev.id) {
                        let _ = ActionRepository::create_many::<DatabaseConnection>(
                            acts.clone()
                                .into_iter()
                                .map(|m| m.into_active_model())
                                .collect(),
                            None,
                        )
                        .await;
                    }
                    if let Some(pts) = points_by_device.get(&dev.id) {
                        let _ = PointRepository::create_many::<DatabaseConnection>(
                            pts.clone()
                                .into_iter()
                                .map(|m| m.into_active_model())
                                .collect(),
                            None,
                        )
                        .await;
                    }
                    let _ = DeviceRepository::create::<DatabaseConnection>(
                        dev.clone().into_active_model(),
                        None,
                    )
                    .await;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn change_device_status(&self, device: DeviceModel, status: Status) -> NGResult<()> {
        match self
            .southward_manager
            .change_device_status(&device, status.into())
            .await
        {
            Ok(_) => {
                DeviceRepository::update::<DatabaseConnection>(
                    ChangeDeviceStatus {
                        id: device.id,
                        status,
                    }
                    .into_active_model(),
                    None,
                )
                .await?;
                Ok(())
            }
            Err(e) => {
                return Err(e);
            }
        }
    }
}

#[async_trait]
impl PointRuntimeCmd for NGGateway {
    #[inline]
    async fn create_points(&self, points: Vec<NewPoint>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }
        // Step 1: DB insert without holding transaction
        let created = PointRepository::create_many::<DatabaseConnection>(
            points
                .into_iter()
                .map(|np| np.into_active_model())
                .collect(),
            None,
        )
        .await?;

        let mut grouped: HashMap<i32, Vec<PointModel>> = HashMap::new();
        for pm in created.into_iter() {
            grouped.entry(pm.device_id).or_default().push(pm);
        }

        // Step 2: Apply to runtime per device; compensate DB+runtime on any failure
        for (dev_id, rps) in grouped.into_iter() {
            let ids: Vec<i32> = rps.iter().map(|rp| rp.id).collect();
            if let Err(e) = self.southward_manager.add_points(dev_id, rps).await {
                let _ = PointRepository::delete_by_ids::<DatabaseConnection>(ids, None).await;
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn update_points(&self, points: Vec<UpdatePoint>) -> NGResult<()> {
        if points.is_empty() {
            return Ok(());
        }
        // Snapshot old models for compensation
        let olds: Vec<PointModel> =
            PointRepository::find_by_ids(points.iter().map(|p| p.id).collect()).await?;
        let mut old_grouped: HashMap<i32, Vec<PointModel>> = HashMap::new();
        for pm in olds.into_iter() {
            old_grouped.entry(pm.device_id).or_default().push(pm);
        }

        // Step 1: DB updates (sequential for simpler compensation)
        let updated = tokio_stream::iter(points.into_iter())
            .map(|up| PointRepository::update::<DatabaseConnection>(up.into_active_model(), None))
            .buffer_unordered(16)
            .try_collect::<Vec<PointModel>>()
            .await?;

        let mut grouped: HashMap<i32, Vec<PointModel>> = HashMap::new();
        for pm in updated.into_iter() {
            grouped.entry(pm.device_id).or_default().push(pm);
        }

        // Step 2: Apply to runtime per device; compensate DB+runtime on any failure
        for (dev_id, rps) in grouped.into_iter() {
            let ids: Vec<i32> = rps.iter().map(|m| m.id).collect();
            if let Err(e) = self.southward_manager.replace_points(dev_id, rps).await {
                for old in old_grouped
                    .into_iter()
                    .flat_map(|(_, olds)| olds)
                    .filter(|o| ids.contains(&o.id))
                {
                    let _ = PointRepository::update::<DatabaseConnection>(
                        old.into_active_model(),
                        None,
                    )
                    .await;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn delete_points(&self, ids: Vec<i32>) -> NGResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // Step 1: DB delete
        let deleted = PointRepository::delete_by_ids::<DatabaseConnection>(ids, None).await?;

        let mut grouped: HashMap<i32, Vec<PointModel>> = HashMap::new();
        for pm in deleted.into_iter() {
            grouped.entry(pm.device_id).or_default().push(pm);
        }

        // Step 2: Apply to runtime removals; compensate DB+runtime on any failure
        for (dev_id, models) in grouped.into_iter() {
            let ids: Vec<i32> = models.iter().map(|m| m.id).collect();
            if let Err(e) = self.southward_manager.remove_points(dev_id, ids).await {
                let _ = PointRepository::create_many::<DatabaseConnection>(
                    models.into_iter().map(|m| m.into_active_model()).collect(),
                    None,
                )
                .await;
                return Err(e);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ActionRuntimeCmd for NGGateway {
    #[inline]
    async fn create_actions(&self, actions: Vec<NewAction>) -> NGResult<()> {
        if actions.is_empty() {
            return Ok(());
        }
        // Step 1: DB insert without holding transaction
        let created: Vec<ActionModel> = ActionRepository::create_many::<DatabaseConnection>(
            actions
                .into_iter()
                .map(|na| na.into_active_model())
                .collect(),
            None,
        )
        .await?;

        let mut grouped: HashMap<i32, Vec<ActionModel>> = HashMap::new();
        for am in created.into_iter() {
            grouped.entry(am.device_id).or_default().push(am);
        }

        // Step 2: Apply to runtime per device; compensate DB on any failure
        for (dev_id, models) in grouped.into_iter() {
            let ids: Vec<i32> = models.iter().map(|m| m.id).collect();
            if let Err(e) = self.southward_manager.add_actions(dev_id, models).await {
                let _ = ActionRepository::delete_by_ids::<DatabaseConnection>(ids, None).await;
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn update_actions(&self, actions: Vec<UpdateAction>) -> NGResult<()> {
        if actions.is_empty() {
            return Ok(());
        }
        // Snapshot old models
        let olds: Vec<ActionModel> =
            ActionRepository::find_by_ids(actions.iter().map(|a| a.id).collect()).await?;
        let mut old_grouped: HashMap<i32, Vec<ActionModel>> = HashMap::new();
        for am in olds.into_iter() {
            old_grouped.entry(am.device_id).or_default().push(am);
        }

        // Step 1: DB updates (buffered)
        let updated: Vec<ActionModel> = tokio_stream::iter(actions.into_iter())
            .map(|up| ActionRepository::update::<DatabaseConnection>(up.into_active_model(), None))
            .buffer_unordered(16)
            .try_collect()
            .await?;

        let mut grouped: HashMap<i32, Vec<ActionModel>> = HashMap::new();
        for am in updated.into_iter() {
            grouped.entry(am.device_id).or_default().push(am);
        }

        // Step 2: Apply to runtime; compensate DB on failure
        for (dev_id, ras) in grouped.into_iter() {
            let ids: Vec<i32> = ras.iter().map(|am| am.id).collect();
            if let Err(e) = self.southward_manager.replace_actions(dev_id, ras).await {
                // revert DB for these ids
                for old in old_grouped
                    .iter()
                    .flat_map(|(_, v)| v)
                    .filter(|o| ids.contains(&o.id))
                {
                    let _ = ActionRepository::update::<DatabaseConnection>(
                        old.clone().into_active_model(),
                        None,
                    )
                    .await;
                }
                return Err(e);
            }
        }
        Ok(())
    }

    #[inline]
    async fn delete_actions(&self, ids: Vec<i32>) -> NGResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // Step 1: DB delete with returning
        let deleted = ActionRepository::delete_by_ids::<DatabaseConnection>(ids, None).await?;

        let mut grouped: HashMap<i32, Vec<ActionModel>> = HashMap::new();
        for am in deleted.into_iter() {
            grouped.entry(am.device_id).or_default().push(am);
        }

        // Step 2: Apply removals to runtime; compensate DB on failure
        for (dev_id, models) in grouped.into_iter() {
            let ids: Vec<i32> = models.iter().map(|m| m.id).collect();
            if let Err(e) = self.southward_manager.remove_actions(dev_id, ids).await {
                let _ = ActionRepository::create_many::<DatabaseConnection>(
                    models.into_iter().map(|m| m.into_active_model()).collect(),
                    None,
                )
                .await;
                return Err(e);
            }
        }
        Ok(())
    }

    async fn debug_action(
        &self,
        action_id: i32,
        params: serde_json::Value,
        timeout_ms: Option<u64>,
    ) -> NGResult<serde_json::Value> {
        // Fetch action and derive context
        let action =
            ActionRepository::find_by_id(action_id)
                .await?
                .ok_or(NGError::StorageError(StorageError::EntityNotFound(
                    EntityType::Action.to_string(),
                )))?;
        let device_id = action.device_id;

        // Execute with timeout via direct executor
        let ms = timeout_ms.unwrap_or(5000);
        let start = Instant::now();
        let result = match timeout(
            Duration::from_millis(ms),
            execute_action_direct(
                &self.southward_manager,
                device_id,
                action.command.as_str(),
                Some(params),
            ),
        )
        .await
        {
            Ok(inner) => inner?,
            Err(_) => return Err(NGError::Timeout(Duration::from_millis(ms))),
        };
        let elapsed = start.elapsed().as_millis();
        info!(action_id=%action_id, device_id=%device_id, elapsed_ms=%elapsed, "action-debug");
        Ok(result)
    }
}

#[async_trait]
impl DriverRuntimeCmd for NGGateway {
    async fn install_driver(&self, driver_id: i32, file_path: &Path) -> NGResult<()> {
        // Basic file checks
        if !file_path.exists() || !file_path.is_file() {
            return Err(NGError::Error("Driver file not found".to_string()));
        }

        let _ = self
            .driver_loader
            .load_driver_library(file_path, driver_id)
            .await
            .map_err(|e| NGError::DriverError(e.to_string()))?;
        Ok(())
    }

    #[inline]
    async fn uninstall_driver(&self, driver_id: i32, file_path: &Path) -> NGResult<()> {
        // Check if there are any channels associated with this driver
        if ChannelRepository::exists_by_driver_id(driver_id).await? {
            return Err(NGError::Error(
                "Associated southward channels exist, uninstallation not supported".to_string(),
            ));
        }

        // Unregister registry entry if exists
        self.driver_loader.unregister(driver_id).await;
        // Remove file best-effort
        let _ = fs::remove_file(file_path).await;
        Ok(())
    }
}

#[async_trait]
impl PluginRuntimeCmd for NGGateway {
    async fn install_plugin(&self, plugin_id: i32, file_path: &Path) -> NGResult<()> {
        // Basic file checks
        if !file_path.exists() || !file_path.is_file() {
            return Err(NGError::Error("Plugin file not found".to_string()));
        }

        let _ = self
            .northward_loader
            .load_library(file_path, plugin_id)
            .await
            .map_err(|e| NGError::Error(format!("Failed to load plugin: {}", e)))?;
        Ok(())
    }

    #[inline]
    async fn uninstall_plugin(&self, plugin_id: i32, _file_path: &Path) -> NGResult<()> {
        // Check if plugin is referenced by any apps
        if AppRepository::exists_by_plugin_id(plugin_id).await? {
            return Err(NGError::Error(
                "Associated northward apps exist, uninstallation not supported".to_string(),
            ));
        }

        // Unregister registry entry if exists
        self.northward_loader.unregister(plugin_id).await;
        // Remove file best-effort
        let _ = fs::remove_file(_file_path).await;
        Ok(())
    }
}

#[async_trait]
impl AppRuntimeCmd for NGGateway {
    #[inline]
    async fn create_app(&self, app: NewApp) -> NGResult<()> {
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;

        let conn = get_db_connection().await?;
        let created = AppRepository::create(app.into_active_model(), Some(&conn)).await?;

        if let Err(e) = self
            .northward_manager
            .create_and_start_app(
                &created,
                None,
                &conn,
                self.northward_events_tx.clone(),
                self.shutdown_token.child_token(),
                settings.general.northward.app_sync_start_timeout_ms,
            )
            .await
        {
            error!(app_id=%created.id, error=%e, "Failed to start northward app");
            // Update status to Disabled on failure
            let _ = AppRepository::delete(created.id, Some(&conn)).await?;
            return Err(e);
        }
        Ok(())
    }

    #[inline]
    async fn update_app(&self, app: UpdateApp) -> NGResult<()> {
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;

        let previous = match AppRepository::find_by_id(app.id).await? {
            Some(app) => app,
            None => return Err(NGError::Error("Northward app not found".to_string())),
        };

        let conn = get_db_connection().await?;
        let updated = AppRepository::update(app.into_active_model(), Some(&conn)).await?;

        match updated.status {
            Status::Disabled => self.northward_manager.stop_and_remove_app(updated.id).await,
            Status::Enabled => {
                // Restart with latest configuration and subscriptions (restart semantics)
                let sub = AppSubRepository::find_by_app_id(updated.id).await?;

                if let Err(e) = self
                    .northward_manager
                    .restart_app(
                        &updated,
                        sub,
                        &conn,
                        self.northward_events_tx.clone(),
                        self.shutdown_token.child_token(),
                        settings.general.northward.app_sync_start_timeout_ms,
                    )
                    .await
                {
                    let _ = AppRepository::update::<DatabaseConnection>(
                        previous.into_active_model(),
                        Some(&conn),
                    )
                    .await?;
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    #[inline]
    async fn delete_app(&self, app_id: i32) -> NGResult<()> {
        self.northward_manager.stop_and_remove_app(app_id).await;
        AppRepository::delete_deep(app_id)
            .await
            .map_err(NGError::StorageError)?;
        Ok(())
    }

    #[inline]
    async fn change_app_status(&self, app: AppModel, status: Status) -> NGResult<()> {
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;

        match status {
            Status::Disabled => {
                self.northward_manager.stop_and_remove_app(app.id).await;
            }
            Status::Enabled => {
                let conn = get_db_connection().await?;
                let subs = AppSubRepository::find_by_app_id(app.id).await?;

                self.northward_manager
                    .restart_app(
                        &app,
                        subs,
                        &conn,
                        self.northward_events_tx.clone(),
                        self.shutdown_token.child_token(),
                        settings.general.northward.app_sync_start_timeout_ms,
                    )
                    .await?;
            }
        }

        AppRepository::update::<DatabaseConnection>(
            ChangeAppStatus { id: app.id, status }.into_active_model(),
            None,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl AppSubRuntimeCmd for NGGateway {
    #[inline]
    async fn create_sub(&self, sub: NewAppSub) -> NGResult<()> {
        let created =
            AppSubRepository::create::<DatabaseConnection>(sub.into_active_model(), None).await?;

        self.northward_manager
            .update_subscription(created.app_id, created)
            .await?;

        Ok(())
    }

    #[inline]
    async fn update_sub(&self, sub: UpdateAppSub) -> NGResult<()> {
        let updated =
            AppSubRepository::update::<DatabaseConnection>(sub.into_active_model(), None).await?;

        self.northward_manager
            .update_subscription(updated.app_id, updated)
            .await?;

        Ok(())
    }

    #[inline]
    async fn delete_sub(&self, id: i32) -> NGResult<()> {
        let sub = AppSubRepository::find_by_id(id)
            .await?
            .ok_or(NGError::Error(
                "Northward subscription not found".to_string(),
            ))?;

        AppSubRepository::delete::<DatabaseConnection>(id, None).await?;
        self.northward_manager.remove_subscription(sub.app_id);
        Ok(())
    }
}

impl NGGateway {
    async fn load_northward_config(
        conn: &DatabaseConnection,
    ) -> NGResult<Vec<(AppModel, Option<AppSubModel>)>> {
        Ok(AppRepository::find_with_sub(Some(conn)).await?)
    }

    async fn load_southward_config(
        conn: &DatabaseConnection,
    ) -> NGResult<
        Vec<(
            ChannelModel,
            Vec<(DeviceModel, Vec<PointModel>, Vec<ActionModel>)>,
        )>,
    > {
        let channels_with_devices = ChannelRepository::find_with_devices(Some(conn)).await?;

        let all_device_ids: Vec<i32> = channels_with_devices
            .iter()
            .flat_map(|(_, devices)| devices.iter().map(|d| d.id))
            .collect();

        if all_device_ids.is_empty() {
            return Ok(channels_with_devices
                .into_iter()
                .map(|(chan, devices)| {
                    let devs = devices
                        .into_iter()
                        .map(|dev| (dev, Vec::new(), Vec::new()))
                        .collect();
                    (chan, devs)
                })
                .collect());
        }

        let points =
            PointRepository::find_by_device_ids(all_device_ids.clone(), Some(conn)).await?;
        let mut points_by_device: HashMap<i32, Vec<PointModel>> =
            HashMap::with_capacity(points.len());
        for p in points.into_iter() {
            points_by_device.entry(p.device_id).or_default().push(p);
        }

        let actions = ActionRepository::find_by_device_ids(all_device_ids, Some(conn)).await?;
        let mut actions_by_device: HashMap<i32, Vec<ActionModel>> =
            HashMap::with_capacity(actions.len());
        for a in actions.into_iter() {
            actions_by_device.entry(a.device_id).or_default().push(a);
        }

        let mut result = Vec::with_capacity(channels_with_devices.len());

        for (chan, devices) in channels_with_devices.into_iter() {
            let mut devs: Vec<(DeviceModel, Vec<PointModel>, Vec<ActionModel>)> =
                Vec::with_capacity(devices.len());
            for dev in devices.into_iter() {
                let pts = points_by_device.remove(&dev.id).unwrap_or_default();
                let acts = actions_by_device.remove(&dev.id).unwrap_or_default();
                devs.push((dev, pts, acts));
            }
            result.push((chan, devs));
        }

        Ok(result)
    }

    async fn register_gateway_commands_with(this: &Arc<Self>) {
        let registry = gateway_command_registry();
        registry.insert(
            "get_status".to_string(),
            GetStatusHandler::new(Arc::clone(this)),
        );
    }

    /// Set up data forwarding to northward system
    async fn setup_data_forwarding(&self) -> NGResult<()> {
        // Take the receiver once to start forwarding
        let mut data_rx_guard = self.data_rx.write().await;
        let mut data_rx = data_rx_guard.take().ok_or(NGError::InvalidStateError(
            "Data forwarding already started".to_string(),
        ))?;
        drop(data_rx_guard);
        let northward_manager = Arc::clone(&self.northward_manager);
        let monitor_hub = Arc::clone(&self.monitor_hub);
        let token = self.shutdown_token.clone();
        let handle = tokio::spawn(async move {
            info!("Starting high-performance data forwarding task");
            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        break;
                    }
                    maybe_data = data_rx.recv() => {
                        match maybe_data {
                            Some(data) => {
                                // 1) Broadcast raw data to realtime monitor hub
                                monitor_hub.broadcast(&data);
                                // 2) Forward to northward manager for snapshot/filter + app routing
                                northward_manager.route(data).await
                            }
                            None => {
                                warn!("Data forwarding channel closed");
                                break;
                            }
                        }
                    }
                }
            }
        });
        self.forwarding_task.write().await.replace(handle);

        Ok(())
    }

    /// Start unified northward event processor
    ///
    /// This creates a single task that processes ALL northward events
    /// from all apps through the shared global event channel.
    ///
    /// Benefits:
    /// - Automatically supports runtime app addition/removal
    /// - Simple and efficient single-channel design
    /// - Symmetric with southward data forwarding
    async fn start_northward_event_processor(&self) -> NGResult<()> {
        // Take the global events receiver (can only be done once)
        let mut events_rx_guard = self.northward_events_rx.write().await;
        let mut events_rx = events_rx_guard.take().ok_or(NGError::InvalidStateError(
            "Northward events processor already started".to_string(),
        ))?;
        drop(events_rx_guard);

        let northward_manager = Arc::clone(&self.northward_manager);
        let southward_manager = Arc::clone(&self.southward_manager);
        let write_serializers = Arc::new(WriteSerializers::default());
        let token = self.shutdown_token.clone();

        let handle = tokio::spawn(async move {
            info!("🚀 Unified northward event processor started");

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        info!("Northward event processor shutting down");
                        break;
                    }
                    maybe_event = events_rx.recv() => {
                        match maybe_event {
                            Some((app_id, event)) => {
                                // Process event through unified handler
                                Self::handle_northward_event(
                                    app_id,
                                    event,
                                    &northward_manager,
                                    &southward_manager,
                                    &write_serializers,
                                ).await;
                            }
                            None => {
                                warn!("All northward event senders dropped");
                                break;
                            }
                        }
                    }
                }
            }

            info!("✅ Northward event processor stopped");
        });

        self.northward_events_task.write().await.replace(handle);

        Ok(())
    }

    /// Handle a single northward event (unified logic, aligned with new event design)
    ///
    /// This method processes business events from northward plugins.
    /// Connection lifecycle events are now handled via `watch::Receiver<NorthwardConnectionState>`
    /// in AppActor's connection monitor task.
    ///
    /// # Event Types (Business Events Only)
    /// - `CommandReceived`: Platform sends command to device (route to southbound)
    /// - `WritePoint`: Platform/plugin requests a point write (validate + per-channel serialize + dispatch + async reply)
    /// - `RpcResponseReceived`: Platform responds to RPC request (route to originator)
    ///
    /// # Arguments
    /// * `app_id` - The ID of the app that sent the event
    /// * `event` - The business event to process
    /// * `northward_manager` - Reference to northward manager for sending responses
    /// * `southward_manager` - Reference to southward manager for command execution
    async fn handle_northward_event(
        app_id: i32,
        event: NorthwardEvent,
        northward_manager: &Arc<NGNorthwardManager>,
        southward_manager: &Arc<NGSouthwardManager>,
        write_serializers: &Arc<WriteSerializers>,
    ) {
        match event {
            NorthwardEvent::CommandReceived(cmd) => {
                // Fast-path expiration check
                if cmd.is_expired() {
                    warn!(
                        app_id = app_id,
                        command_id = %cmd.command_id,
                        "Command expired, dropping"
                    );
                    return;
                }

                info!(
                    app_id = app_id,
                    command_id = %cmd.command_id,
                    command_key = %cmd.key,
                    target_type = ?cmd.target_type,
                    "Processing command from northward app"
                );

                // Process command based on target type
                let response = match cmd.target_type {
                    TargetType::Gateway => Self::handle_gateway_commands(cmd).await,
                    TargetType::SubDevice => {
                        Self::handle_subdevice_commands(cmd, southward_manager).await
                    }
                };

                // Send response back to the app
                northward_manager
                    .send_to_app(Arc::new(NorthwardData::RpcResponse(response)), app_id)
                    .await;
            }

            NorthwardEvent::WritePoint(req) => {
                // Spawn writepoint handling so control-plane event processing isn't blocked by I/O.
                // This enables true parallelism across channels while still serializing writes
                // within the same channel via `WriteSerializers`.
                let northward_manager = Arc::clone(northward_manager);
                let southward_manager = Arc::clone(southward_manager);
                let write_serializers = Arc::clone(write_serializers);
                tokio::spawn(async move {
                    Self::handle_write_point(
                        app_id,
                        req,
                        &northward_manager,
                        &southward_manager,
                        &write_serializers,
                    )
                    .await;
                });
            }

            NorthwardEvent::RpcResponseReceived(response) => {
                info!(
                    app_id = app_id,
                    request_id = %response.request_id,
                    success = response.result.is_some(),
                    "RPC response received from northward app"
                );
                // TODO: Implement RPC response handling
                // This could be used for request-response patterns
                // where the gateway sends RPC to the platform and waits for response
            }
        }
    }

    async fn handle_write_point(
        app_id: i32,
        req: WritePoint,
        northward_manager: &Arc<NGNorthwardManager>,
        southward_manager: &Arc<NGSouthwardManager>,
        write_serializers: &Arc<WriteSerializers>,
    ) {
        // Single lookup for both meta + runtime point (hot-path optimization).
        let Some((meta, point)) = southward_manager.get_point_entry(req.point_id) else {
            let resp = WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                0,
                WritePointErrorKind::NotFound,
                format!("point {} not found", req.point_id),
                Utc::now(),
            );
            northward_manager
                .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                .await;
            return;
        };

        // From here, we can always include device_id in response.
        let device_id = meta.device_id;

        // Access mode validation
        if !matches!(meta.access_mode, AccessMode::Write | AccessMode::ReadWrite) {
            let resp = WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                device_id,
                WritePointErrorKind::NotWriteable,
                "point is not writeable",
                Utc::now(),
            );
            northward_manager
                .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                .await;
            return;
        }

        // Datatype validation
        if !req.value.validate_datatype(meta.data_type) {
            let resp = WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                device_id,
                WritePointErrorKind::TypeMismatch,
                format!(
                    "type mismatch: expected {:?}, got {:?}",
                    meta.data_type,
                    req.value.data_type()
                ),
                Utc::now(),
            );
            northward_manager
                .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                .await;
            return;
        }

        // Range validation (numeric only)
        if let (Some(min), Some(max)) = (meta.min_value, meta.max_value) {
            if let Ok(n) = f64::try_from(&req.value) {
                if n < min || n > max {
                    let resp = WritePointResponse::failed(
                        req.request_id.clone(),
                        req.point_id,
                        device_id,
                        WritePointErrorKind::OutOfRange,
                        format!("out of range: {n} not in [{min}, {max}]"),
                        Utc::now(),
                    );
                    northward_manager
                        .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                        .await;
                    return;
                }
            }
        }

        // Channel connection validation
        if !southward_manager.is_channel_connected(meta.channel_id) {
            let resp = WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                device_id,
                WritePointErrorKind::NotConnected,
                format!("channel {} not connected", meta.channel_id),
                Utc::now(),
            );
            northward_manager
                .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                .await;
            return;
        }

        // Serialize writes per channel.
        let sem = write_serializers.semaphore_for(meta.channel_id);
        let start = Instant::now();
        let permit = match req.timeout_ms {
            Some(ms) => {
                let total = Duration::from_millis(ms);
                match timeout(total, sem.clone().acquire_owned()).await {
                    Ok(Ok(p)) => Some((p, total)),
                    Ok(Err(_)) => None,
                    Err(_) => {
                        let resp = WritePointResponse::failed(
                            req.request_id.clone(),
                            req.point_id,
                            device_id,
                            WritePointErrorKind::QueueTimeout,
                            format!("queue timeout after {ms}ms"),
                            Utc::now(),
                        );
                        northward_manager
                            .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                            .await;
                        return;
                    }
                }
            }
            None => match sem.clone().acquire_owned().await {
                Ok(p) => Some((p, Duration::from_millis(0))),
                Err(_) => None,
            },
        };

        let Some((permit, total_timeout)) = permit else {
            let resp = WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                device_id,
                WritePointErrorKind::DriverError,
                "failed to acquire channel write lock",
                Utc::now(),
            );
            northward_manager
                .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                .await;
            return;
        };

        // Remaining budget (best-effort): subtract queue wait time from overall timeout.
        let remaining_timeout_ms = match req.timeout_ms {
            Some(_ms) => {
                let elapsed = start.elapsed();
                let total = total_timeout;
                let rem = total.checked_sub(elapsed).unwrap_or_default();
                let rem_ms = rem.as_millis().min(u64::MAX as u128) as u64;
                Some(rem_ms.max(1))
            }
            None => None,
        };

        // Snapshot device + point and release dashmap refs before awaiting.
        let device = match southward_manager.get_device(device_id) {
            Some(d) => d,
            None => {
                drop(permit);
                let resp = WritePointResponse::failed(
                    req.request_id.clone(),
                    req.point_id,
                    device_id,
                    WritePointErrorKind::NotFound,
                    format!("device {} not found", device_id),
                    Utc::now(),
                );
                northward_manager
                    .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
                    .await;
                return;
            }
        };
        let driver = Arc::clone(&device.driver);
        let device_cfg = Arc::clone(&device.config);
        drop(device);

        // Execute driver write (still serialized by permit).
        let write_res = driver
            .write_point(device_cfg, point, req.value.clone(), remaining_timeout_ms)
            .await;

        drop(permit);

        let resp = match write_res {
            Ok(r) => WritePointResponse::success(
                req.request_id,
                req.point_id,
                device_id,
                r.applied_value.or_else(|| Some(req.value.clone())),
                Utc::now(),
            ),
            Err(e) => WritePointResponse::failed(
                req.request_id.clone(),
                req.point_id,
                device_id,
                WritePointErrorKind::DriverError,
                e.to_string(),
                Utc::now(),
            ),
        };

        // Control-plane replies must not be dropped; give it a bounded send timeout.
        northward_manager
            .send_to_app(Arc::new(NorthwardData::WritePointResponse(resp)), app_id)
            .await;
    }

    async fn handle_gateway_commands(cmd: Command) -> ClientRpcResponse {
        let registry = gateway_command_registry();
        if let Some(handler) = registry.get(cmd.key.as_str()) {
            match handler.value().handle(&cmd).await {
                Ok(value) => ClientRpcResponse::success(
                    cmd.command_id,
                    cmd.target_type,
                    0,
                    cmd.device_name,
                    value,
                ),
                Err(e) => ClientRpcResponse::error(
                    cmd.command_id,
                    cmd.target_type,
                    0,
                    cmd.device_name,
                    format!("{e}"),
                ),
            }
        } else {
            ClientRpcResponse::error(
                cmd.command_id,
                cmd.target_type,
                0,
                cmd.device_name,
                "Command not found".to_string(),
            )
        }
    }

    async fn handle_subdevice_commands(
        cmd: Command,
        southward_manager: &Arc<NGSouthwardManager>,
    ) -> ClientRpcResponse {
        let device_id = cmd.device_id.or_else(|| {
            cmd.device_name
                .as_ref()
                .and_then(|name| southward_manager.find_device_id_by_name(name.as_str()))
        });
        if let Some(id) = device_id {
            match execute_action_direct(southward_manager, id, cmd.key.as_str(), cmd.params).await {
                Ok(result) => ClientRpcResponse::success(
                    cmd.command_id,
                    cmd.target_type,
                    id,
                    cmd.device_name,
                    result,
                ),
                Err(err) => {
                    error!(error=%err, "Failed to execute action");
                    ClientRpcResponse::error(
                        cmd.command_id,
                        cmd.target_type,
                        id,
                        cmd.device_name,
                        format!("Failed to execute action: {err}"),
                    )
                }
            }
        } else {
            ClientRpcResponse::error(
                cmd.command_id,
                cmd.target_type,
                cmd.device_id.unwrap_or_default(),
                cmd.device_name,
                "Device not found".to_string(),
            )
        }
    }

    /// Start metrics collection
    async fn start_metrics_collection(&self) -> NGResult<()> {
        let metrics = Arc::clone(&self.metrics);
        let southward_manager = Arc::clone(&self.southward_manager);
        let collector: Arc<Collector> = Arc::clone(&self.collector);

        let token = self.shutdown_token.clone();
        let handle = tokio::spawn(async move {
            let mut interval = interval(std::time::Duration::from_millis(10000));

            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        break;
                    }
                    _ = interval.tick() => {
                        let southward_manager_metrics = southward_manager.get_metrics().await;
                        let collection_engine_metrics = collector.get_metrics().await;

                        let mut metrics_guard = metrics.write().await;

                        // Get data manager metrics
                        metrics_guard.total_channels = southward_manager_metrics.total_channels;
                        metrics_guard.connected_channels = southward_manager_metrics.connected_channels;
                        metrics_guard.total_devices = southward_manager_metrics.total_devices;
                        metrics_guard.active_devices = southward_manager_metrics.active_devices;
                        metrics_guard.total_data_points = southward_manager_metrics.total_data_points;

                        // Get collection engine metrics
                        metrics_guard.total_collections = collection_engine_metrics.total_collections;
                        metrics_guard.successful_collections =
                            collection_engine_metrics.successful_collections;
                        metrics_guard.failed_collections = collection_engine_metrics.failed_collections;
                        metrics_guard.timeout_collections = collection_engine_metrics.timeout_collections;
                        metrics_guard.average_collection_time_ms =
                            collection_engine_metrics.average_collection_time_ms;
                        metrics_guard.active_tasks = collection_engine_metrics.active_tasks;

                        // Update timestamp
                        metrics_guard.last_update = Some(Utc::now());
                        drop(metrics_guard);
                    }
                }
            }
        });
        self.metrics_task.write().await.replace(handle);

        Ok(())
    }

    #[inline]
    /// Get comprehensive gateway status
    pub async fn get_status(&self) -> GatewayStatus {
        let state = self.state.read().await.clone();
        let metrics = self.get_updated_metrics().await;
        let northward_metrics = self.northward_manager.get_status().await;
        let southward_metrics = self.southward_manager.get_metrics().await;
        let collector_metrics = self.collector.get_metrics().await;
        let system_info = Self::get_system_info();

        GatewayStatus {
            state,
            metrics,
            northward_metrics,
            version: env!("CARGO_PKG_VERSION").to_string(),
            southward_metrics,
            collector_metrics,
            system_info,
        }
    }

    /// Get real-time system information
    fn get_system_info() -> SystemInfo {
        let mut sys = System::new_all();
        sys.refresh_all();

        // Get OS information
        let os_type = System::name().unwrap_or_else(|| "Unknown".to_string());
        let os_arch = System::cpu_arch();
        let hostname = System::host_name();

        // Get CPU information
        let cpu_cores = sys.cpus().len();
        let cpu_usage_percent = sys.global_cpu_usage() as f64;

        // Get memory information
        let total_memory = sys.total_memory();
        let used_memory = sys.used_memory();
        let memory_usage_percent = if total_memory > 0 {
            (used_memory as f64 / total_memory as f64) * 100.0
        } else {
            0.0
        };

        // Get disk information
        let disks = Disks::new_with_refreshed_list();
        let (total_disk, used_disk) = disks.list().iter().fold((0u64, 0u64), |acc, disk| {
            let total = disk.total_space();
            let available = disk.available_space();
            let used = total.saturating_sub(available);
            (acc.0 + total, acc.1 + used)
        });
        let disk_usage_percent = if total_disk > 0 {
            (used_disk as f64 / total_disk as f64) * 100.0
        } else {
            0.0
        };

        SystemInfo {
            os_type,
            os_arch,
            hostname,
            cpu_cores,
            total_memory,
            used_memory,
            memory_usage_percent,
            cpu_usage_percent,
            total_disk,
            used_disk,
            disk_usage_percent,
        }
    }

    #[inline]
    /// Get updated metrics
    async fn get_updated_metrics(&self) -> GatewayMetrics {
        let mut metrics = self.metrics.write().await;

        // Update uptime
        if let Some(start_time) = self.start_time {
            metrics.uptime = Utc::now() - start_time;
        }

        metrics.clone()
    }

    #[inline]
    /// Get current gateway state
    pub async fn get_state(&self) -> GatewayState {
        self.state.read().await.clone()
    }

    #[inline]
    /// Check if gateway is running
    pub async fn is_running(&self) -> bool {
        matches!(*self.state.read().await, GatewayState::Running)
    }

    #[inline]
    /// Get channel manager reference
    pub fn get_southward_manager(&self) -> &Arc<NGSouthwardManager> {
        &self.southward_manager
    }

    #[inline]
    /// Get northward manager reference
    pub fn get_northward_manager(&self) -> &Arc<NGNorthwardManager> {
        &self.northward_manager
    }

    #[inline]
    /// Get collection engine reference
    pub fn get_collector(&self) -> &Collector {
        &self.collector
    }

    #[inline]
    /// Get driver registry reference
    pub fn get_driver_registry(&self) -> &DriverRegistry {
        &self.driver_registry
    }

    /// Pause the gateway (stop collection but keep connections)
    pub async fn pause(&self) -> NGResult<()> {
        info!("Pausing gateway");

        *self.state.write().await = GatewayState::Pausing;

        // Stop collection engine but keep managers running
        if let Err(e) = self.collector.stop().await {
            error!(error=%e, "Failed to stop collection engine");
        }

        *self.state.write().await = GatewayState::Paused;

        info!("Gateway paused successfully");
        Ok(())
    }

    /// Resume the gateway from paused state
    pub async fn resume(&self) -> NGResult<()> {
        info!("Resuming gateway");

        let current_state = self.state.read().await.clone();
        if !matches!(current_state, GatewayState::Paused) {
            return Err(NGError::InvalidStateError(format!(
                "Cannot resume gateway from state: {:?}",
                current_state
            )));
        }

        // Restart collection engine
        if let Err(e) = self.collector.start().await {
            error!(error=%e, "Failed to start collection engine");
        }

        *self.state.write().await = GatewayState::Running;

        info!("Gateway resumed successfully");
        Ok(())
    }
}

// Execute action directly with validation and connection checks.
// Shared by northward command handling and web debug API.
async fn execute_action_direct(
    southward_manager: &Arc<NGSouthwardManager>,
    device_id: i32,
    action_key: &str,
    params: Option<serde_json::Value>,
) -> NGResult<serde_json::Value> {
    // Ensure device is loaded in runtime
    let device = southward_manager
        .get_device(device_id)
        .ok_or(StorageError::EntityNotFound(EntityType::Device.to_string()))?;

    // Check channel connection status
    let device_model = DeviceRepository::find_by_id(device_id)
        .await?
        .ok_or(StorageError::EntityNotFound(EntityType::Device.to_string()))?;
    if !southward_manager.is_channel_connected(device_model.channel_id) {
        return Err(NGError::StorageError(StorageError::EntityNotFound(
            EntityType::Channel.to_string(),
        )));
    }

    // Locate runtime action (by command)
    let runtime_actions = southward_manager.get_device_actions(device_id);
    let runtime_action = runtime_actions
        .iter()
        .find(|a| a.command() == action_key)
        .cloned()
        .ok_or(StorageError::EntityNotFound(EntityType::Action.to_string()))?;

    // Validate + resolve parameters into typed NGValue list (single gateway entrypoint).
    let inputs = runtime_action.input_parameters();
    let typed_params = validate_and_resolve_action_inputs(&inputs, &params)
        .map_err(|err| NGError::DriverError(err.to_string()))?;

    // Execute
    device
        .driver
        .execute_action(Arc::clone(&device.config), runtime_action, typed_params)
        .await
        .map(|r| r.payload.unwrap_or(serde_json::Value::Null))
        .map_err(|e| NGError::DriverError(e.to_string()))
}

#[inline]
fn current_os_type() -> OsType {
    if cfg!(target_os = "windows") {
        OsType::Windows
    } else if cfg!(target_os = "macos") {
        OsType::Mac
    } else if cfg!(target_os = "linux") {
        OsType::Linux
    } else {
        OsType::Unknown
    }
}

#[inline]
fn current_os_arch() -> OsArch {
    if cfg!(target_arch = "x86_64") {
        OsArch::X86_64
    } else if cfg!(target_arch = "aarch64") {
        OsArch::Arm64
    } else if cfg!(target_arch = "arm") {
        OsArch::Arm
    } else {
        OsArch::Unknown
    }
}
