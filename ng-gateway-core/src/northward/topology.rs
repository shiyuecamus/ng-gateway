//! Northward topology initialization helpers.
//!
//! This module extracts topology/bootstrap responsibilities from `northward/manager.rs` so the
//! manager can focus on hot-path routing and lifecycle orchestration.
//!
//! # Notes
//! - This is a control-plane path (low frequency).
//! - Avoid holding `DashMap` guards across `.await`.

use super::{
    super::lifecycle::StartPolicy,
    actor::{AppActor, AppActorParams, AppIo, NorthwardAppObserverFactory},
    bus::NorthwardEventsBus,
    extension::AppExtensionManager,
    manager::NGNorthwardManager,
    runtime_api::CoreNorthwardRuntimeApi,
    subscription_sync::{compute_sync_plan, SyncPlan},
};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{
    entities::prelude::{AppModel, AppSubModel},
    enums::common::Status,
};
use ng_gateway_sdk::NorthwardInitContext;
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

impl NGNorthwardManager {
    /// Initialize topology from DB models with global events channel.
    ///
    /// Loads all apps with their subscriptions and starts them.
    /// Each app's events will be forwarded to the global events channel.
    ///
    /// # Arguments
    /// * `topology` - Vector of (app, subscriptions) tuples from database
    /// * `db` - Database connection for extension manager
    /// * `northward_events_bus` - Global bus for forwarding app events to Gateway
    pub async fn initialize_topology(
        &self,
        topology: Vec<(AppModel, Option<AppSubModel>)>,
        db: &DatabaseConnection,
        northward_events_bus: &Arc<NorthwardEventsBus>,
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
                            self.start_app_event_bridge(
                                app.id,
                                &actor,
                                Arc::clone(northward_events_bus),
                            );

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

    /// Create an AppActor from DB model (aligned with southbound pattern).
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

        // Build per-app I/O first so the supervision Observer can flush buffers on Connected.
        let io = AppIo::new(&self.metrics_hub, app.id, app.plugin_id, app.queue_policy)?;
        let observer_factory = Arc::new(NorthwardAppObserverFactory::new(
            app.id,
            &io,
            app.queue_policy,
        ));

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
            observer_factory,
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
        Ok(Arc::new(AppActor::new(AppActorParams {
            app_id: app.id,
            app_name: app.name.clone(),
            plugin_id: app.plugin_id,
            plugin,
            events_rx,
            config,
            queue_policy: app.queue_policy,
            shutdown_token,
            io,
        })))
    }

    /// Prepare app runtime: create actor + update router + compute sync plan.
    pub(super) async fn prepare_app_runtime(
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
}
