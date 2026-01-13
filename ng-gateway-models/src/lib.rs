pub mod cache;
pub mod casbin;
pub mod constants;
pub mod core;
pub mod domain;
pub mod entities;
pub mod enums;
pub mod event;
mod idens;
pub mod initializer;
pub mod rbac;
pub mod settings;
pub mod web;

use crate::{
    cache::NGBaseCache,
    casbin::{CasbinCmd, CasbinResult},
    domain::prelude::{
        Claims, NewAction, NewApp, NewAppSub, NewChannel, NewDevice, NewPoint, UpdateAction,
        UpdateApp, UpdateAppSub, UpdateChannel, UpdateDevice, UpdatePoint,
    },
    entities::prelude::{AppModel, ChannelModel, DeviceModel},
    enums::common::Status,
    event::NGEvent,
    rbac::PermRule,
};
use ::casbin::Error as CasbinError;
use actix_web::http::Method;
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use ng_gateway_error::{
    init::InitContextError,
    rbac::RBACError,
    storage::{CacheError, StorageError},
    NGResult,
};
use ng_gateway_sdk::{NorthwardConnectionState, NorthwardData, SouthwardConnectionState};
use sea_orm::DatabaseConnection;
use settings::Settings;
use std::{sync::Arc, time::Duration};
use tokio::sync::broadcast;

// Implement downcast for core system traits
impl_downcast!(sync WebServer);
impl_downcast!(sync DbManager);
impl_downcast!(sync CacheProvider);
impl_downcast!(sync EventBus);
impl_downcast!(sync Gateway);
impl_downcast!(sync SouthwardManager);
impl_downcast!(sync NorthwardManager);
impl_downcast!(sync PermChecker);
impl_downcast!(sync CasbinService);
impl_downcast!(sync ChannelRuntimeCmd);
impl_downcast!(sync DeviceRuntimeCmd);
impl_downcast!(sync PointRuntimeCmd);
impl_downcast!(sync ActionRuntimeCmd);
impl_downcast!(sync DriverRuntimeCmd);
impl_downcast!(sync PluginRuntimeCmd);
impl_downcast!(sync AppRuntimeCmd);
impl_downcast!(sync AppSubRuntimeCmd);

pub const DEFAULT_ROOT_TREE_ID: i32 = 0;

/// Database management interface for the NG Gateway platform.
///
/// This trait defines the core database operations including initialization,
/// connection management, and cleanup.
#[async_trait]
pub trait DbManager: DowncastSync + Send + Sync + 'static {
    /// Initializes the database manager with the provided settings.
    ///
    /// # Arguments
    /// * `settings` - The platform configuration settings
    ///
    /// # Returns
    /// An Arc-wrapped instance of the database manager
    async fn init(settings: &Settings) -> NGResult<Arc<Self>, InitContextError>
    where
        Self: Sized;

    /// Retrieves a database connection from the connection pool.
    ///
    /// # Returns
    /// A SeaORM database connection or an error if the connection cannot be established
    fn get_connection(&self) -> NGResult<DatabaseConnection, StorageError>;

    /// Gracefully closes all database connections and performs cleanup.
    async fn close(&self) -> NGResult<()>;
}

/// Cache provider interface for distributed caching functionality.
///
/// Manages caching operations across the platform, supporting different
/// cache value types and implementations.
#[async_trait]
pub trait CacheProvider: DowncastSync + Send + Sync + 'static {
    /// Initializes the cache provider with the provided configuration.
    ///
    /// # Arguments
    /// * `settings` - The platform configuration settings
    ///
    /// # Returns
    /// An Arc-wrapped instance of the cache provider
    async fn init(settings: &Settings) -> NGResult<Arc<Self>, InitContextError>
    where
        Self: Sized;

    /// Create a cache instance for a specific value type.
    ///
    /// # Arguments
    /// * `cache_name` - The name of the cache
    /// * `max_capacity` - The maximum capacity of the cache
    /// * `ttl` - The time to live of the cache
    fn create_cache<V>(
        &mut self,
        cache_name: &str,
        max_capacity: Option<u64>,
        ttl: Option<Duration>,
    ) -> NGResult<(), CacheError>
    where
        Self: Sized,
        V: Clone + Send + Sync + 'static;

    /// Retrieves a cache instance for a specific value type.
    ///
    /// # Returns
    /// A type-specific cache implementation wrapped in Arc
    fn get_cache<V>(
        &self,
        cache_name: &str,
    ) -> NGResult<Arc<dyn NGBaseCache<Value = V> + Send + Sync>, CacheError>
    where
        Self: Sized,
        V: Clone + Send + Sync + 'static;
}

/// Event bus interface for platform-wide event handling.
///
/// Provides publish-subscribe functionality for asynchronous event processing
/// across the platform.
#[async_trait]
pub trait EventBus: DowncastSync + Send + Sync + 'static {
    /// Initializes a new event bus instance.
    ///
    /// # Returns
    /// An Arc-wrapped instance of the event bus
    async fn init(settings: &Settings) -> Arc<Self>
    where
        Self: Sized;

    /// Registers an event handler for a specific event type.
    ///
    /// # Arguments
    /// * `handler` - The function to handle events of type E
    ///
    /// # Type Parameters
    /// * `E` - The event type
    /// * `F` - The handler function type
    async fn register_handler<E, F>(&self, handler: F)
    where
        Self: Sized,
        E: NGEvent + 'static,
        F: FnMut(&E) -> NGResult<()> + Send + Sync + 'static;

    /// Publishes an event to all registered handlers.
    ///
    /// # Arguments
    /// * `event` - The event to publish
    ///
    /// # Returns
    /// The number of handlers that received the event
    async fn publish<E>(&self, event: E) -> NGResult<usize>
    where
        Self: Sized,
        E: NGEvent + 'static;
}

/// Casbin service for handling Casbin commands.
///
/// Provides a service for handling Casbin commands.
#[async_trait]
pub trait CasbinService: DowncastSync + Send + Sync + 'static {
    /// Initializes the Casbin service.
    ///
    /// # Arguments
    /// * `db` - The database connection
    ///
    /// # Returns
    /// An Arc-wrapped instance of the service
    async fn init(db: DatabaseConnection) -> NGResult<Arc<Self>, InitContextError>
    where
        Self: Sized;

    /// Handles a Casbin command.
    ///
    /// # Arguments
    /// * `cmd` - The Casbin command
    ///
    /// # Returns
    /// A Casbin result or an error if the command couldn't be handled
    async fn handle_cmd(&self, cmd: CasbinCmd) -> NGResult<CasbinResult, CasbinError>
    where
        Self: Sized;
}

/// Web server interface for HTTP services.
///
/// Manages the platform's HTTP API endpoints and web services.
#[async_trait]
pub trait WebServer: DowncastSync + Send + Sync + 'static {
    /// Initializes the web server.
    ///
    /// # Arguments
    /// * `settings` - The platform configuration settings
    /// * `perm_checker` - The platform's permission checker
    /// * `gateway` - The platform's gateway
    async fn init(
        settings: &Settings,
        perm_checker: Arc<dyn PermChecker>,
        gateway: Arc<dyn Gateway>,
    ) -> NGResult<Arc<Self>, InitContextError>
    where
        Self: Sized;

    /// Gracefully stops the web server.
    async fn stop(&self) -> NGResult<()>;
}

/// Permission checker for validating access rights to API endpoints
///
/// This trait defines the interface for registering and checking permission rules
/// against incoming requests. Implementations of this trait manage authorization
/// rules for different API routes and evaluate whether a request should be allowed.
#[async_trait]
pub trait PermChecker: DowncastSync + Send + Sync + 'static {
    /// Initializes the permission checker.
    fn init() -> Arc<Self>
    where
        Self: Sized;

    /// Registers a permission rule for a specific HTTP method and path
    ///
    /// # Arguments
    /// * `method` - The HTTP method (e.g., "GET", "POST")
    /// * `path` - The API path to protect
    /// * `rule` - The permission rule to apply for authorization checks
    ///
    /// # Returns
    /// * `NGResult<(), RBACError>` - Success if the rule was registered, or an error if registration failed (e.g., duplicate rule)
    async fn register<R: PermRule + 'static>(
        &self,
        method: Method,
        path: String,
        rule: R,
    ) -> NGResult<(), RBACError>
    where
        Self: Sized;

    /// Checks if a request passes the registered permission rules
    ///
    /// # Arguments
    /// * `method` - The HTTP method (e.g., "GET", "POST")
    /// * `path` - The API path to protect
    /// * `claims` - The claims of the user
    ///
    /// # Returns
    /// * `NGResult<bool>` - True if permission is granted, False if denied,
    ///                      or an error if the check couldn't be performed
    async fn check(
        &self,
        method: &str,
        path: &str,
        claims: Arc<Claims>,
    ) -> NGResult<bool, RBACError>;
}

/// Trait for integrating with NGAppContext
#[async_trait::async_trait]
pub trait Gateway:
    DowncastSync
    + ChannelRuntimeCmd
    + DeviceRuntimeCmd
    + PointRuntimeCmd
    + ActionRuntimeCmd
    + DriverRuntimeCmd
    + PluginRuntimeCmd
    + AppRuntimeCmd
    + AppSubRuntimeCmd
    + Send
    + Sync
    + 'static
{
    /// Initialize the gateway from settings
    async fn init(
        settings: &Settings,
        db_manager: Arc<dyn DbManager>,
    ) -> NGResult<Arc<Self>, InitContextError>
    where
        Self: Sized;

    /// Stop the gateway
    async fn stop(&self) -> NGResult<()>;

    /// Get the southward manager for accessing channel connection states
    fn southward_manager(&self) -> Arc<dyn SouthwardManager>;

    /// Get the northward manager for accessing app connection states
    fn northward_manager(&self) -> Arc<dyn NorthwardManager>;

    /// Get the realtime monitor hub for accessing device connection states
    fn realtime_monitor_hub(&self) -> Arc<dyn RealtimeMonitorHub>;
}

/// Trait for accessing southward channel connection states
pub trait SouthwardManager: DowncastSync + Send + Sync + 'static {
    /// Get the connection state for a channel
    ///
    /// Returns `None` if the channel is not found in the runtime manager.
    fn get_channel_connection_state(&self, channel_id: i32) -> Option<SouthwardConnectionState>;
}

/// Trait for accessing northward app connection states
pub trait NorthwardManager: DowncastSync + Send + Sync + 'static {
    /// Get the connection state for an app
    ///
    /// Returns `None` if the app is not found in the runtime manager.
    fn get_app_connection_state(&self, app_id: i32) -> Option<NorthwardConnectionState>;
}

/// Trait for accessing realtime monitor hub
pub trait RealtimeMonitorHub: DowncastSync + Send + Sync + 'static {
    /// Subscribe to realtime data for a specific device
    fn subscribe(&self, device_id: i32) -> broadcast::Receiver<Arc<NorthwardData>>;
    /// Broadcast realtime data for a specific device
    fn broadcast(&self, data: &Arc<NorthwardData>);
}

#[async_trait::async_trait]
pub trait ChannelRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Create a new channel in the gateway
    async fn create_channel(&self, channel: NewChannel) -> NGResult<()>;

    /// Update an existing channel in the gateway
    async fn update_channel(&self, channel: UpdateChannel) -> NGResult<()>;

    /// Delete an existing channel in the gateway
    async fn delete_channel(&self, channel_id: i32) -> NGResult<()>;

    /// Change the status of an existing channel in the gateway
    async fn change_channel_status(&self, channel: ChannelModel, status: Status) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait DeviceRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Batch create devices (DB insert + runtime add with compensation)
    async fn create_devices(&self, devices: Vec<NewDevice>) -> NGResult<()>;

    /// Batch update devices (DB update + runtime replace with compensation)
    async fn update_devices(&self, devices: Vec<UpdateDevice>) -> NGResult<()>;

    /// Batch delete devices (DB delete + runtime remove with compensation)
    async fn delete_devices(&self, ids: Vec<i32>) -> NGResult<()>;

    /// Change the status of an existing device in the gateway
    async fn change_device_status(&self, device: DeviceModel, status: Status) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait PointRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Batch create points (DB insert + runtime update, transactional)
    async fn create_points(&self, points: Vec<NewPoint>) -> NGResult<()>;

    /// Batch update points (DB update + runtime delta, transactional with compensation)
    async fn update_points(&self, points: Vec<UpdatePoint>) -> NGResult<()>;

    /// Batch delete points (DB delete + runtime remove, transactional with compensation)
    async fn delete_points(&self, ids: Vec<i32>) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait ActionRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Batch create actions (DB insert + runtime update, transactional)
    async fn create_actions(&self, actions: Vec<NewAction>) -> NGResult<()>;

    /// Batch update actions (DB update + runtime delta, transactional with compensation)
    async fn update_actions(&self, actions: Vec<UpdateAction>) -> NGResult<()>;

    /// Batch delete actions (DB delete + runtime remove, transactional with compensation)
    async fn delete_actions(&self, ids: Vec<i32>) -> NGResult<()>;

    /// Execute an action synchronously for debugging via Web API
    async fn debug_action(
        &self,
        action_id: i32,
        params: serde_json::Value,
        timeout_ms: Option<u64>,
    ) -> NGResult<serde_json::Value>;
}

#[async_trait::async_trait]
pub trait DriverRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Install a new driver from uploaded file path (already stored in controlled dir)
    async fn install_driver(&self, driver_id: i32, file_path: &std::path::Path) -> NGResult<()>;

    /// Uninstall a driver (force stop related channels, remove file and DB)
    async fn uninstall_driver(&self, driver_id: i32, file_path: &std::path::Path) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait PluginRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Install a new northward plugin from uploaded file path (already stored in controlled dir)
    async fn install_plugin(&self, plugin_id: i32, file_path: &std::path::Path) -> NGResult<()>;

    /// Uninstall a northward plugin (if no apps reference it, remove file and DB)
    async fn uninstall_plugin(&self, plugin_id: i32, file_path: &std::path::Path) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait AppRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Create a new northward app in the gateway
    async fn create_app(&self, app: NewApp) -> NGResult<()>;

    /// Update an existing northward app in the gateway
    async fn update_app(&self, app: UpdateApp) -> NGResult<()>;

    /// Delete an existing northward app in the gateway
    async fn delete_app(&self, app_id: i32) -> NGResult<()>;

    /// Change the status of an existing northward app in the gateway
    async fn change_app_status(&self, app: AppModel, status: Status) -> NGResult<()>;
}

#[async_trait::async_trait]
pub trait AppSubRuntimeCmd: DowncastSync + Send + Sync + 'static {
    /// Create a new northward subscription in the gateway
    async fn create_sub(&self, sub: NewAppSub) -> NGResult<()>;

    /// Update an existing northward subscription in the gateway
    async fn update_sub(&self, sub: UpdateAppSub) -> NGResult<()>;

    /// Delete an existing northward subscription in the gateway
    async fn delete_sub(&self, id: i32) -> NGResult<()>;
}
