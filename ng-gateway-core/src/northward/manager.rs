//! NorthwardManager: Centralized management for all northward apps
//!
//! Responsibilities:
//! - Plugin registry and factory management
//! - App actor lifecycle management
//! - Data routing (device -> apps via SubscriptionRouter)
//! - Topology initialization from DB models

use super::runtime_api::CoreNorthwardRuntimeApi;
use super::{
    actor::AppActor,
    extension::AppExtensionManager,
    router::SubscriptionRouter,
    subscription_sync::{compute_sync_plan, DeviceSyncStatus, SubscriptionSyncTracker, SyncPlan},
};
use crate::lifecycle::{start_with_policy, StartPolicy};
use crate::southward::manager::NGSouthwardManager;
use dashmap::DashMap;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{
    core::metrics::{
        AppState, NorthwardAppStats, NorthwardManagerMetrics, NorthwardManagerMetricsSnapshot,
    },
    entities::prelude::{AppModel, AppSubModel},
    enums::common::Status,
    NorthwardManager,
};
use ng_gateway_sdk::{
    AttributeData, DeviceConnectedData, DeviceDisconnectedData, NorthwardConnectionState,
    NorthwardData, NorthwardEvent, NorthwardInitContext, NorthwardRegistry, PointValue,
    TelemetryData,
};
use sea_orm::DatabaseConnection;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Northward manager for all apps and plugins
#[derive(Clone)]
pub struct NGNorthwardManager {
    /// Plugin registry: plugin_id -> Factory (passed from Gateway)
    plugin_registry: NorthwardRegistry,

    /// Active app actors: app_id -> AppActor
    app_actors: Arc<DashMap<i32, Arc<AppActor>>>,

    /// Subscription router for device -> apps resolution
    router: Arc<SubscriptionRouter>,

    /// Manager-level metrics
    metrics: Arc<NorthwardManagerMetrics>,

    /// Reference to southward manager for runtime snapshots.
    southward_manager: Arc<NGSouthwardManager>,

    /// Tracker storing per-app subscription reconciliation state.
    subscription_tracker: Arc<SubscriptionSyncTracker>,
}

impl NGNorthwardManager {
    /// Create a new NorthwardManager
    pub fn new(
        plugin_registry: NorthwardRegistry,
        southward_manager: Arc<NGSouthwardManager>,
    ) -> Arc<Self> {
        Arc::new(Self {
            plugin_registry,
            app_actors: Arc::new(DashMap::new()),
            router: Arc::new(SubscriptionRouter::new()),
            metrics: Arc::new(NorthwardManagerMetrics::default()),
            southward_manager,
            subscription_tracker: Arc::new(SubscriptionSyncTracker::new()),
        })
    }

    /// Initialize topology from DB models with global events channel
    ///
    /// Loads all apps with their subscriptions and starts them.
    /// Each app's events will be forwarded to the global events channel.
    ///
    /// # Arguments
    /// * `topology` - Vector of (app, subscriptions) tuples from database
    /// * `db` - Database connection for extension manager
    /// * `global_events_tx` - Global event channel sender for forwarding app events to Gateway
    pub async fn initialize_topology(
        &self,
        topology: Vec<(AppModel, Option<AppSubModel>)>,
        db: &DatabaseConnection,
        global_events_tx: &mpsc::Sender<(i32, NorthwardEvent)>,
    ) -> NGResult<()> {
        info!(
            "Initializing northward topology with {} apps",
            topology.len()
        );

        let mut started = 0;
        let mut failed = 0;

        let topology: Vec<(AppModel, Option<AppSubModel>)> = topology
            .into_iter()
            .filter(|(app, _)| app.status == Status::Enabled)
            .collect();

        for (app, sub) in topology {
            match self
                .prepare_app_runtime(&app, sub, db, CancellationToken::new())
                .await
            {
                Ok((actor, pending_plan)) => {
                    // Start the actor with AsyncFireAndForget policy (non-blocking)
                    match self
                        .start_app_with_policy(&actor, StartPolicy::AsyncFireAndForget)
                        .await
                    {
                        Ok(_) => {
                            // Start event bridge (forwards plugin events to global channel)
                            self.start_app_event_bridge(app.id, &actor, global_events_tx.clone());

                            self.app_actors.insert(app.id, Arc::clone(&actor));

                            if let Some(plan) = pending_plan {
                                if let Err(e) = self.execute_subscription_sync(app.id, plan).await {
                                    warn!(
                                        error = %e,
                                        app_id = app.id,
                                        "Failed to synchronize subscription state during bootstrap"
                                    );
                                }
                            }

                            started += 1;
                            info!("App {} started successfully (async)", app.id);
                        }
                        Err(e) => {
                            error!("Failed to start app {}: {}", app.id, e);
                            actor.stop().await;
                            self.router.remove(app.id);
                            failed += 1;
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to create app {} actor: {}", app.id, e);
                    failed += 1;
                }
            }
        }

        // Update metrics based on current registry (only successfully registered apps)
        self.refresh_metrics_from_registry();

        info!(
            "Northward topology initialized: {} started, {} failed",
            started, failed
        );

        Ok(())
    }

    /// Create an AppActor from DB model (aligned with southbound pattern)
    ///
    /// This function performs plugin initialization and actor creation:
    /// 1. Create plugin instance
    /// 2. Convert configuration
    /// 3. Create events channel for business events
    /// 4. Initialize plugin (spawns internal connection supervisor)
    /// 5. Create AppActor with initialized plugin
    ///
    /// **Key Design**:
    /// - Plugin's `init()` is async and spawns connection supervisor internally
    /// - Plugin manages its own connection lifecycle (connect, retry, reconnect)
    /// - AppActor subscribes to connection state via `subscribe_connection_state()`
    /// - Fully aligned with southbound Driver pattern
    ///
    /// This does NOT start the actor - call `actor.start()` separately to:
    /// - Spawn data worker task
    /// - Subscribe to plugin connection state
    /// - Spawn connection monitor task
    ///
    /// # Arguments
    /// * `app` - App model from database
    /// * `db` - Database connection for extension manager
    /// * `shutdown_token` - Cancellation token for graceful shutdown
    pub async fn create_app_actor(
        &self,
        app: &AppModel,
        db: &DatabaseConnection,
        shutdown_token: CancellationToken,
    ) -> NGResult<Arc<AppActor>> {
        // Get plugin factory
        let factory = self
            .plugin_registry
            .get(&app.plugin_id)
            .ok_or(NGError::Error(format!(
                "Plugin {} not found in registry",
                app.plugin_id
            )))?;

        // Convert config
        let config = factory
            .convert_plugin_config(app.config.clone())
            .map_err(|e| NGError::Error(format!("Failed to convert plugin config: {}", e)))?;

        // Create events channel for business events (RPC, Command, Attribute)
        let (events_tx, events_rx) = mpsc::channel(1024);

        // Create extension manager for this app (with database connection)
        let extension_manager = Arc::new(AppExtensionManager::new(app.id, db.clone()));

        // Create initialization context with all dependencies
        let init_ctx = NorthwardInitContext {
            extension_manager,
            app_id: app.id,
            app_name: app.name.clone(),
            config: Arc::clone(&config),
            events_tx,
            // Core runtime metadata API for high-throughput encoding paths.
            // Backed by southward runtime indexes (no DB access on hot path).
            runtime: Arc::new(CoreNorthwardRuntimeApi::new(Arc::clone(
                &self.southward_manager,
            ))),
            retry_policy: app.retry_policy,
        };

        // Create plugin instance with context (no I/O)
        let plugin = factory
            .create_plugin(init_ctx)
            .map_err(|e| NGError::Error(format!("Plugin creation failed: {}", e)))?;

        info!(
            "Plugin created for app {} (plugin_id: {})",
            app.id, app.plugin_id
        );

        // Create AppActor with initialized plugin
        let actor = AppActor::new(
            app.id,
            app.name.clone(),
            app.plugin_id,
            plugin,
            events_rx,
            config,
            app.queue_policy,
            shutdown_token,
        )?;

        Ok(Arc::new(actor))
    }

    // === Data routing ===

    /// Route data to subscribed apps (broadcast)
    ///
    /// Resolves which apps should receive the data based on device_id.
    /// Applies change detection filtering for ReportType::Change channels before routing.
    pub async fn route(&self, data: Arc<NorthwardData>) {
        let device_id = data.device_id();

        // Update device snapshot and filter changes based on ReportType
        let filtered_data = match self
            .southward_manager
            .update_and_filter_device_snapshot(data)
        {
            Some(d) => d,
            None => {
                // No changes detected, drop the data
                return;
            }
        };

        // Resolve which apps subscribe to this device (no allocations on hot path).
        let mut app_ids = self.router.resolve_iter(device_id).peekable();

        if app_ids.peek().is_none() {
            tracing::trace!("No apps subscribed to device {}", device_id);
            return;
        }

        // Send to all subscribed apps
        for app_id in app_ids {
            if let Some(actor) = self.app_actors.get(&app_id) {
                match actor.send_data(Arc::clone(&filtered_data)).await {
                    Ok(true) => {
                        self.metrics.increment_data_routed();
                    }
                    Ok(false) => {
                        // Dropped due to backpressure (queue full or timeout)
                        debug!("Data dropped by app {} (backpressure)", app_id);
                    }
                    Err(e) => {
                        error!("Failed to send data to app {}: {}", app_id, e);
                        self.metrics.increment_routing_errors();
                    }
                }
            } else {
                warn!("App {} not found in actors (stale subscription?)", app_id);
                self.metrics.increment_routing_errors();
            }
        }
    }

    /// Send data to a specific app (for RPC responses)
    ///
    /// This is for precise replies, not broadcast routing
    pub async fn send_to_app(&self, data: Arc<NorthwardData>, app_id: i32) {
        if let Some(actor) = self.app_actors.get(&app_id) {
            match actor.send_data(data).await {
                Ok(true) => {
                    debug!("Sent data to app {}", app_id);
                }
                Ok(false) => {
                    warn!("Data dropped by app {} (backpressure)", app_id);
                }
                Err(e) => {
                    error!("Failed to send data to app {}: {}", app_id, e);
                }
            }
        } else {
            error!("App {} not found", app_id);
        }
    }

    // === App lifecycle management ===

    /// Start an app actor with the specified policy using shared lifecycle helper.
    ///
    /// This delegates the common "start + wait" logic to the core lifecycle
    /// helper while keeping app-specific bookkeeping (metrics, router) inside
    /// the manager.
    #[inline]
    async fn start_app_with_policy(
        &self,
        actor: &Arc<AppActor>,
        policy: StartPolicy,
    ) -> NGResult<()> {
        let actor_for_start = Arc::clone(actor);
        let start_fn = move || async move { actor_for_start.start().await };

        let actor_for_wait = Arc::clone(actor);
        let wait_fn = move |timeout_ms: u64| async move {
            actor_for_wait.wait_for_connected(timeout_ms).await
        };

        start_with_policy(policy, start_fn, wait_fn).await
    }

    /// Prepare app runtime: create actor + update router + compute sync plan
    async fn prepare_app_runtime(
        &self,
        app: &AppModel,
        sub: Option<AppSubModel>,
        db: &DatabaseConnection,
        shutdown_token: CancellationToken,
    ) -> NGResult<(Arc<AppActor>, Option<SyncPlan>)> {
        // Create actor
        let actor = self.create_app_actor(app, db, shutdown_token).await?;

        let mut pending_plan: Option<SyncPlan> = None;

        // Update router and compute sync plan (if subscription exists)
        if let Some(sub) = sub {
            let previous = self.router.get_subscription_info(app.id);
            self.router.update(app.id, sub);
            if let Some(current) = self.router.get_subscription_info(app.id) {
                pending_plan = compute_sync_plan(
                    &self.subscription_tracker,
                    app.id,
                    previous.as_ref(),
                    &current,
                );
            }
        }

        Ok((actor, pending_plan))
    }

    /// Restart an app with new configuration and subscriptions
    ///
    /// This provides a high-level "restart" semantic:
    /// - If the app is currently running, stop and remove it (router + metrics)
    /// - Create a fresh `AppActor` with the latest configuration
    /// - Apply subscriptions and execute subscription synchronization plan
    /// - Start the app with `StartPolicy::SyncWaitConnected`
    ///
    /// It is safe to call this even when the app is currently stopped (no actor in registry).
    ///
    /// # Arguments
    /// * `app` - Latest application model from database
    /// * `sub` - Latest subscription model for this app (if any)
    /// * `db` - Database connection for extension manager
    /// * `global_events_tx` - Global event channel sender for forwarding app events
    /// * `shutdown_token` - Cancellation token for graceful shutdown
    /// * `timeout_ms` - Maximum time to wait for connection (default: 30000ms)
    pub async fn restart_app(
        &self,
        app: &AppModel,
        sub: Option<AppSubModel>,
        db: &DatabaseConnection,
        global_events_tx: mpsc::Sender<(i32, NorthwardEvent)>,
        shutdown_token: CancellationToken,
        timeout_ms: u64,
    ) -> NGResult<()> {
        self.stop_and_remove_app(app.id).await;
        self.create_and_start_app(app, sub, db, global_events_tx, shutdown_token, timeout_ms)
            .await
    }

    /// Create and start a new app at runtime
    ///
    /// This is used when a new app is added via API during gateway operation.
    /// The app's events will be automatically forwarded to the global events channel.
    ///
    /// Uses `StartPolicy::SyncWaitConnected` to wait for connection before returning,
    /// ensuring the app is ready to process data when this method returns.
    /// If connection fails, the error is propagated to the caller for user feedback.
    ///
    /// # Arguments
    /// * `app` - Application model from database
    /// * `subs` - Subscription models for this app
    /// * `db` - Database connection for extension manager
    /// * `global_events_tx` - Global event channel sender for forwarding app events
    /// * `shutdown_token` - Cancellation token for graceful shutdown
    /// * `timeout_ms` - Maximum time to wait for connection (default: 30000ms)
    pub async fn create_and_start_app(
        &self,
        app: &AppModel,
        sub: Option<AppSubModel>,
        db: &DatabaseConnection,
        global_events_tx: mpsc::Sender<(i32, NorthwardEvent)>,
        shutdown_token: CancellationToken,
        timeout_ms: u64,
    ) -> NGResult<()> {
        // Check if app already exists
        if self.app_actors.contains_key(&app.id) {
            return Err(NGError::Error(format!("App {} already exists", app.id)));
        }

        // Prepare runtime: create actor + update router + compute sync plan
        let (actor, pending_plan) = self
            .prepare_app_runtime(app, sub, db, shutdown_token)
            .await?;

        // Start actor with SyncWaitConnected policy (wait for connection)
        match self
            .start_app_with_policy(&actor, StartPolicy::SyncWaitConnected { timeout_ms })
            .await
        {
            Ok(_) => {
                info!("App {} connected successfully", app.id);
            }
            Err(e) => {
                error!("App {} failed to connect: {}", app.id, e);
                actor.stop().await;
                self.router.remove(app.id);
                // Don't insert into registry on sync start failure
                // Error is propagated to caller for user feedback
                return Err(e);
            }
        }

        // Start event bridge (forwards plugin events to global channel)
        self.start_app_event_bridge(app.id, &actor, global_events_tx);

        // Insert into registry
        self.app_actors.insert(app.id, Arc::clone(&actor));

        if let Some(plan) = pending_plan {
            self.execute_subscription_sync(app.id, plan).await?;
        }

        // Update metrics based on current registry
        self.refresh_metrics_from_registry();

        info!("App {} created and started", app.id);
        Ok(())
    }

    /// Execute subscription synchronization plan for the specified application.
    async fn execute_subscription_sync(&self, app_id: i32, plan: SyncPlan) -> NGResult<()> {
        if plan.is_empty() {
            return Ok(());
        }

        let SyncPlan {
            connect_filter,
            disconnect_ids,
        } = plan;

        let actor = self
            .app_actors
            .get(&app_id)
            .map(|entry| Arc::clone(entry.value()))
            .ok_or_else(|| NGError::Error(format!("App {} not found for sync", app_id)))?;

        if actor.state() != AppState::Running {
            return Err(NGError::InvalidStateError(format!(
                "App {} not running, cannot synchronize subscription",
                app_id
            )));
        }

        if let Some(filter) = connect_filter {
            let snapshots = self.southward_manager.list_connected_devices(filter);
            for snapshot in snapshots.into_iter() {
                let already_connected = self
                    .subscription_tracker
                    .get_state(app_id, snapshot.device_id)
                    .map(|state| state.status == DeviceSyncStatus::Connected)
                    .unwrap_or(false);
                if already_connected {
                    continue;
                }

                // 1. Send DeviceConnected event
                let payload = Arc::new(NorthwardData::DeviceConnected(DeviceConnectedData {
                    device_id: snapshot.device_id,
                    device_name: snapshot.device_name.to_string(),
                    device_type: snapshot.device_type.to_string(),
                }));

                match actor.send_data(payload).await {
                    Ok(true) => {
                        debug!(
                            app_id,
                            device_id = snapshot.device_id,
                            channel_id = snapshot.channel_id,
                            "Subscription sync emitted DeviceConnected"
                        );
                    }
                    Ok(false) => {
                        warn!(
                            app_id,
                            device_id = snapshot.device_id,
                            channel_id = snapshot.channel_id,
                            "Subscription sync dropped DeviceConnected due to queue policy"
                        );
                    }
                    Err(e) => return Err(e),
                }

                // 2. Send device data snapshot if available
                if let Some(data_snapshot) = self
                    .southward_manager
                    .get_device_snapshot(snapshot.device_id)
                {
                    // Send telemetry snapshot if present
                    if !data_snapshot.telemetry.is_empty() {
                        let values = data_snapshot
                            .telemetry
                            .iter()
                            .map(|(point_id, value)| {
                                let point_key = data_snapshot
                                    .point_key_by_id
                                    .get(point_id)
                                    .map(Arc::clone)
                                    .unwrap_or_else(|| Arc::<str>::from(point_id.to_string()));
                                PointValue {
                                    point_id: *point_id,
                                    point_key,
                                    value: value.clone(),
                                }
                            })
                            .collect::<Vec<PointValue>>();
                        let telemetry = Arc::new(NorthwardData::Telemetry(TelemetryData {
                            device_id: snapshot.device_id,
                            device_name: snapshot.device_name.to_string(),
                            timestamp: data_snapshot.last_update,
                            values,
                            metadata: HashMap::new(),
                        }));

                        match actor.send_data(telemetry).await {
                            Ok(true) => {
                                debug!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    "Subscription sync sent telemetry snapshot"
                                );
                            }
                            Ok(false) => {
                                warn!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    "Subscription sync dropped telemetry snapshot due to queue policy"
                                );
                            }
                            Err(e) => {
                                error!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    error = %e,
                                    "Failed to send telemetry snapshot"
                                );
                            }
                        }
                    }

                    // Send attributes snapshot if present
                    if !data_snapshot.client_attributes.is_empty()
                        || !data_snapshot.shared_attributes.is_empty()
                        || !data_snapshot.server_attributes.is_empty()
                    {
                        let client_attributes = data_snapshot
                            .client_attributes
                            .iter()
                            .map(|(point_id, value)| {
                                let point_key = data_snapshot
                                    .point_key_by_id
                                    .get(point_id)
                                    .map(Arc::clone)
                                    .unwrap_or_else(|| Arc::<str>::from(point_id.to_string()));
                                PointValue {
                                    point_id: *point_id,
                                    point_key,
                                    value: value.clone(),
                                }
                            })
                            .collect::<Vec<PointValue>>();
                        let shared_attributes = data_snapshot
                            .shared_attributes
                            .iter()
                            .map(|(point_id, value)| {
                                let point_key = data_snapshot
                                    .point_key_by_id
                                    .get(point_id)
                                    .map(Arc::clone)
                                    .unwrap_or_else(|| Arc::<str>::from(point_id.to_string()));
                                PointValue {
                                    point_id: *point_id,
                                    point_key,
                                    value: value.clone(),
                                }
                            })
                            .collect::<Vec<PointValue>>();
                        let server_attributes = data_snapshot
                            .server_attributes
                            .iter()
                            .map(|(point_id, value)| {
                                let point_key = data_snapshot
                                    .point_key_by_id
                                    .get(point_id)
                                    .map(Arc::clone)
                                    .unwrap_or_else(|| Arc::<str>::from(point_id.to_string()));
                                PointValue {
                                    point_id: *point_id,
                                    point_key,
                                    value: value.clone(),
                                }
                            })
                            .collect::<Vec<PointValue>>();
                        let attributes = Arc::new(NorthwardData::Attributes(AttributeData {
                            device_id: snapshot.device_id,
                            device_name: snapshot.device_name.to_string(),
                            client_attributes,
                            shared_attributes,
                            server_attributes,
                            timestamp: data_snapshot.last_update,
                        }));

                        match actor.send_data(attributes).await {
                            Ok(true) => {
                                debug!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    "Subscription sync sent attributes snapshot"
                                );
                            }
                            Ok(false) => {
                                warn!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    "Subscription sync dropped attributes snapshot due to queue policy"
                                );
                            }
                            Err(e) => {
                                error!(
                                    app_id,
                                    device_id = snapshot.device_id,
                                    error = %e,
                                    "Failed to send attributes snapshot"
                                );
                            }
                        }
                    }
                }

                // Mark as connected after sending all data
                self.subscription_tracker.mark_connected(app_id, &snapshot);
            }
        }

        for device_id in disconnect_ids.into_iter() {
            let Some(state) = self.subscription_tracker.get_state(app_id, device_id) else {
                continue;
            };

            if state.status != DeviceSyncStatus::Connected {
                continue;
            }

            let payload = Arc::new(NorthwardData::DeviceDisconnected(DeviceDisconnectedData {
                device_id,
                device_name: state.device_name.to_string(),
                device_type: state.device_type.to_string(),
            }));

            match actor.send_data(payload).await {
                Ok(true) => {
                    self.subscription_tracker
                        .mark_disconnected(app_id, device_id);
                    debug!(
                        app_id,
                        device_id,
                        channel_id = state.channel_id,
                        "Subscription sync emitted DeviceDisconnected"
                    );
                }
                Ok(false) => {
                    warn!(
                        app_id,
                        device_id,
                        channel_id = state.channel_id,
                        "Subscription sync dropped DeviceDisconnected due to queue policy"
                    );
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Stop and remove an app
    pub async fn stop_and_remove_app(&self, app_id: i32) {
        // Remove from router first
        self.remove_subscription(app_id);

        if let Some((_, actor)) = self.app_actors.remove(&app_id) {
            let _ = actor.stop().await;

            // Update metrics based on current registry
            self.refresh_metrics_from_registry();

            info!("App {} stopped and removed", app_id);
        }
    }

    /// Recalculate manager-level metrics based on the current app registry.
    ///
    /// This recalculates the manager-level metrics based on the current app registry.
    /// This should be called after app creation, restart, or stop to keep the metrics
    /// consistent with the actual runtime state.
    fn refresh_metrics_from_registry(&self) {
        let total = self.app_actors.len() as u64;
        let active = self
            .app_actors
            .iter()
            .filter(|e| e.value().state() == AppState::Running)
            .count() as u64;
        self.metrics.set_total_apps(total);
        self.metrics.set_active_apps(active);
    }

    /// Get all app IDs
    pub fn get_app_ids(&self) -> Vec<i32> {
        self.app_actors.iter().map(|e| *e.key()).collect()
    }

    /// Get app actor by ID
    pub fn get_app(&self, app_id: i32) -> Option<Arc<AppActor>> {
        self.app_actors.get(&app_id).map(|e| e.value().clone())
    }

    /// Get manager status
    pub async fn get_status(&self) -> NorthwardManagerMetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Get app statistics by ID
    pub async fn get_app_stats(&self, app_id: i32) -> Option<NorthwardAppStats> {
        let actor = self.app_actors.get(&app_id)?;
        let actor = actor.value();

        let conn_state = actor.get_connection_state();
        let is_connected = matches!(
            conn_state,
            ng_gateway_sdk::NorthwardConnectionState::Connected
        );

        Some(NorthwardAppStats {
            app_id: actor.app_id(),
            plugin_id: actor.plugin_id(),
            name: actor.app_name(),
            state: actor.state(),
            is_connected,
            metrics: actor.metrics.snapshot(),
        })
    }

    /// Get all app statistics
    pub async fn get_all_app_stats(&self) -> Vec<NorthwardAppStats> {
        let mut stats = Vec::new();

        for entry in self.app_actors.iter() {
            let app_id = *entry.key();
            if let Some(stat) = self.get_app_stats(app_id).await {
                stats.push(stat);
            }
        }

        stats
    }

    /// Start lightweight event bridge for a specific app
    ///
    /// This spawns a simple forwarding task that takes events from the app's
    /// internal channel and forwards them to the Gateway's global event channel.
    ///
    /// The bridge task automatically terminates when the app is stopped (events_rx closes).
    ///
    /// # Arguments
    /// * `app_id` - Application ID for event tagging
    /// * `actor` - Application actor reference
    /// * `global_events_tx` - Global event channel for forwarding to Gateway
    fn start_app_event_bridge(
        &self,
        app_id: i32,
        actor: &Arc<AppActor>,
        global_events_tx: mpsc::Sender<(i32, NorthwardEvent)>,
    ) {
        let Some(mut app_events_rx) = actor.take_events_rx() else {
            warn!(
                "Failed to take events_rx for app {} (already taken or unavailable)",
                app_id
            );
            return;
        };

        let metrics = Arc::clone(&self.metrics);

        // Spawn lightweight bridge task
        tokio::spawn(async move {
            info!("Event bridge started for app {}", app_id);

            // Simple forwarding loop
            while let Some(event) = app_events_rx.recv().await {
                // Update metrics
                metrics.increment_events_received();

                // Forward to global channel (with app_id tag)
                if let Err(e) = global_events_tx.send((app_id, event)).await {
                    error!(
                        "Failed to forward event from app {} to global channel: {}",
                        app_id, e
                    );
                    break;
                }
            }

            info!("Event bridge stopped for app {} (channel closed)", app_id);
        });
    }

    /// Update subscription in the router
    ///
    /// This replaces the previous subscription for this app in the router.
    /// The subscription should already be persisted in the database.
    pub async fn update_subscription(&self, app_id: i32, sub: AppSubModel) -> NGResult<()> {
        let previous = self.router.get_subscription_info(app_id);
        self.router.update(app_id, sub);

        if let Some(current) = self.router.get_subscription_info(app_id) {
            if let Some(plan) = compute_sync_plan(
                &self.subscription_tracker,
                app_id,
                previous.as_ref(),
                &current,
            ) {
                self.execute_subscription_sync(app_id, plan).await?;
            }
        }

        info!("Updated subscription for app {}", app_id);
        Ok(())
    }

    /// Remove all subscriptions for an app
    ///
    /// This removes all subscriptions for this app from the router.
    /// The subscriptions should already be deleted from the database.
    pub fn remove_subscription(&self, app_id: i32) {
        self.router.remove(app_id);
        self.subscription_tracker.clear_app(app_id);
        info!("Removed subscription for app {}", app_id);
    }

    /// Shutdown all apps
    pub async fn shutdown(&self) -> NGResult<()> {
        info!(
            "Shutting down northward manager with {} apps",
            self.app_actors.len()
        );

        let app_ids: Vec<i32> = self.app_actors.iter().map(|e| *e.key()).collect();

        for app_id in app_ids {
            self.stop_and_remove_app(app_id).await;
        }

        // Clear router
        self.router.clear();

        info!("Northward manager shutdown complete");
        Ok(())
    }
}

// Implement the trait for accessing connection states
impl NorthwardManager for NGNorthwardManager {
    fn get_app_connection_state(&self, app_id: i32) -> Option<NorthwardConnectionState> {
        self.app_actors
            .get(&app_id)
            .map(|actor| actor.value().get_connection_state())
    }
}
