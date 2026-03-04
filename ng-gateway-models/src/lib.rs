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
    core::metrics::GatewayStatusSnapshot,
    domain::prelude::{
        AlgorithmInfo, AlgorithmPageParams, AlgorithmProbeInfo, AlgorithmTestInput,
        AlgorithmTestResult, AnalysisResult, Claims, EngineStatus, FrameAnalysisRequest, ModelInfo,
        ModelInstallRequest, ModelPageParams, ModelProbeInfo, NewAction, NewApp, NewAppSub,
        NewChannel, NewDevice, NewPipeline, NewPoint, PageResult, PipelineInfo, PipelinePageParams,
        ProcessorInfo, RuntimeSettingKey, UpdateAction, UpdateApp, UpdateAppSub, UpdateChannel,
        UpdateDevice, UpdateModel, UpdatePipeline, UpdatePoint,
    },
    entities::prelude::{AppModel, ChannelModel, DeviceModel},
    enums::common::Status,
    event::NGEvent,
    rbac::PermRule,
    web::PrometheusTextPayload,
};
use ::casbin::Error as CasbinError;
use actix_web::http::Method;
use async_trait::async_trait;
use bytes::Bytes;
use downcast_rs::{impl_downcast, DowncastSync};
use ng_gateway_error::{
    ai::AiEngineError,
    init::InitContextError,
    rbac::RBACError,
    storage::{CacheError, StorageError},
    NGResult,
};
use ng_gateway_sdk::{ConnectionState, NorthwardData};
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
impl_downcast!(sync AiEngineApi);
impl_downcast!(sync AiModelRegistry);
impl_downcast!(sync AiPipelineRegistry);
impl_downcast!(sync AiAlgorithmRegistry);
impl_downcast!(sync AiInferenceRuntime);

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

    /// Export Prometheus metrics in text exposition format.
    ///
    /// # Notes
    /// - This method is intentionally synchronous (CPU-only encoding).
    /// - Implementations MUST avoid heavy blocking work here; perform lightweight
    ///   scrape-time refresh only (system/queue, etc.).
    fn export_prometheus_metrics(&self) -> NGResult<PrometheusTextPayload>;

    /// Get a fully-serializable gateway status snapshot for REST/WS consumers.
    ///
    /// # Notes
    /// - This is intentionally part of the public `Gateway` trait so web layers do not need
    ///   to downcast the gateway implementation just to build observability payloads.
    async fn get_snapshot(&self) -> GatewayStatusSnapshot;

    /// Get the southward manager for accessing channel connection states
    fn southward_manager(&self) -> Arc<dyn SouthwardManager>;

    /// Get the northward manager for accessing app connection states
    fn northward_manager(&self) -> Arc<dyn NorthwardManager>;

    /// Get the realtime monitor hub for accessing device connection states
    fn realtime_monitor_hub(&self) -> Arc<dyn RealtimeMonitorHub>;

    /// Get a handle to the AI Processing Engine (if enabled).
    ///
    /// Returns `None` when the AI engine is disabled in configuration.
    /// Web API handlers use this to serve AI status, model, pipeline,
    /// and snapshot endpoints.
    fn ai_engine(&self) -> Option<Arc<dyn AiEngineApi>>;

    /// Apply runtime tuning changes for collector-related settings.
    ///
    /// # Notes
    /// - Settings are already mutated/persisted before this hook is called.
    /// - Implementations should be best-effort and avoid blocking the caller for long periods.
    async fn apply_collector_runtime_tuning(
        &self,
        changed: &[RuntimeSettingKey],
        max_concurrent_collections: usize,
        outbound_queue_capacity: usize,
    ) -> NGResult<()>;

    /// Apply runtime tuning changes for northward-related settings.
    async fn apply_northward_runtime_tuning(
        &self,
        changed: &[RuntimeSettingKey],
        queue_capacity: usize,
    ) -> NGResult<()>;
}

// ── AI Sub-Trait: Model Registry ──────────────────────────────────
//
// Manages model lifecycle: probe, install, load/unload, update, query.
// Implementations own an in-memory cache backed by the DB as source of truth.

/// Model lifecycle and registry API.
///
/// Responsible for the full model lifecycle: probe (metadata extraction),
/// install (DB + file + cache), load/unload (inference backend), update,
/// and uninstall. Query methods serve both cached and paginated DB reads.
///
/// # Write-Through Cache
///
/// All mutations go to DB first, then update the in-memory cache.
/// On startup, the cache is hydrated from DB and validated against disk.
#[async_trait::async_trait]
pub trait AiModelRegistry: DowncastSync + Send + Sync + 'static {
    /// Probe a model artifact and extract all available metadata via runtime session.
    ///
    /// Creates a temporary inference session to extract precise tensor
    /// information (shapes, dtypes, names), then destroys the session.
    /// Does NOT persist the model or register it in the runtime.
    async fn probe_model(
        &self,
        file_path: &std::path::Path,
    ) -> Result<ModelProbeInfo, AiEngineError>;

    /// Install a model: probe, persist to DB, move file, cache in registry.
    ///
    /// Follows the strict transaction pipeline: probe → validate → DB insert
    /// → atomic file move → cache update. On failure, DB row is rolled back
    /// and the temp file is left for caller cleanup.
    async fn install_model(
        &self,
        file_path: &std::path::Path,
        user_meta: ModelInstallRequest,
    ) -> Result<ModelInfo, AiEngineError>;

    /// Uninstall a model: unload from backend, remove files, delete DB row, evict cache.
    async fn uninstall_model(&self, model_id: i32) -> Result<(), AiEngineError>;

    /// Update mutable model metadata (name, labels, preprocess/postprocess overrides).
    async fn update_model(&self, model: UpdateModel) -> Result<ModelInfo, AiEngineError>;

    /// Explicitly load a model into the inference backend.
    async fn load_model(&self, model_id: i32) -> Result<(), AiEngineError>;

    /// Explicitly unload a model from the inference backend (free memory).
    async fn unload_model(&self, model_id: i32) -> Result<(), AiEngineError>;

    /// List all registered models (from cache).
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiEngineError>;

    /// Get model info by identifier (from cache).
    async fn get_model(&self, model_id: i32) -> Result<Option<ModelInfo>, AiEngineError>;

    /// Paginated model query with filters (from DB).
    async fn page_models(
        &self,
        params: ModelPageParams,
    ) -> Result<PageResult<ModelInfo>, AiEngineError>;
}

// ── AI Sub-Trait: Pipeline Registry ──────────────────────────────
//
// Three-layer pipeline management:
//   1. Pipeline Definition (DB: pipeline + stages + alarm_rules)
//   2. Pipeline Binding (DB: pipeline_binding — channel ↔ pipeline)
//   3. Pipeline Runtime (in-memory: compiled pipeline + tracker + sampler)

/// Pipeline definition, binding, and runtime management API.
///
/// A pipeline has three lifecycle layers:
/// - **Definition**: the blueprint with stages and alarm rules (persisted in DB).
/// - **Binding**: which channel uses which pipeline (persisted in DB).
/// - **Runtime**: the compiled, optimized form running in-memory for inference.
///
/// Mutation flow: DB write → cache update → re-compile affected runtime bindings.
#[async_trait::async_trait]
pub trait AiPipelineRegistry: DowncastSync + Send + Sync + 'static {
    /// Create a new pipeline definition (stages + alarm rules).
    async fn create_pipeline(&self, pipeline: NewPipeline) -> Result<PipelineInfo, AiEngineError>;

    /// Update an existing pipeline definition.
    ///
    /// All channels bound to this pipeline will be re-compiled automatically.
    async fn update_pipeline(
        &self,
        pipeline: UpdatePipeline,
    ) -> Result<PipelineInfo, AiEngineError>;

    /// Delete a pipeline definition and all associated bindings.
    async fn delete_pipeline(&self, pipeline_id: i32) -> Result<(), AiEngineError>;

    /// Get a pipeline definition by ID.
    async fn get_pipeline(&self, pipeline_id: i32) -> Result<Option<PipelineInfo>, AiEngineError>;

    /// List all pipeline definitions (from cache).
    async fn list_pipelines(&self) -> Result<Vec<PipelineInfo>, AiEngineError>;

    /// Paginated pipeline query with filters (from DB).
    async fn page_pipelines(
        &self,
        params: PipelinePageParams,
    ) -> Result<PageResult<PipelineInfo>, AiEngineError>;

    /// Bind a pipeline to a channel. Compiles and activates at runtime.
    ///
    /// If the channel already has a binding, the old one is replaced.
    async fn bind_pipeline(&self, channel_id: i32, pipeline_id: i32) -> Result<(), AiEngineError>;

    /// Unbind and deactivate the pipeline for a channel.
    async fn unbind_pipeline(&self, channel_id: i32) -> Result<(), AiEngineError>;

    /// Get the active pipeline info for a channel (from runtime cache).
    fn get_channel_pipeline(&self, channel_id: i32) -> Option<PipelineInfo>;
}

// ── AI Sub-Trait: Algorithm Registry ─────────────────────────────
//
// Manages WASM algorithm lifecycle: probe, install, uninstall, test, query.

/// WASM algorithm lifecycle and registry API.
///
/// Algorithms are custom WASM modules that extend pipeline processing with
/// frame transforms or result processors. Metadata is always sourced from
/// the WASM custom section `ng.ai.manifest.v1`.
///
/// Installation flow: probe → validate → persist → compile → register.
#[async_trait::async_trait]
pub trait AiAlgorithmRegistry: DowncastSync + Send + Sync + 'static {
    /// Probe a WASM algorithm artifact and extract metadata from custom section.
    async fn probe_algorithm(&self, wasm_bytes: Bytes)
        -> Result<AlgorithmProbeInfo, AiEngineError>;

    /// Install and register a WASM algorithm artifact.
    ///
    /// Installation MUST use metadata extracted from WASM custom section and
    /// MUST NOT trust caller-provided metadata payloads.
    async fn install_algorithm(&self, wasm_bytes: Bytes) -> Result<AlgorithmInfo, AiEngineError>;

    /// Uninstall a registered algorithm and remove its files.
    async fn uninstall_algorithm(&self, algorithm_id: i32) -> Result<(), AiEngineError>;

    /// List all registered WASM algorithms (from cache).
    async fn list_algorithms(&self) -> Result<Vec<AlgorithmInfo>, AiEngineError>;

    /// Get a single algorithm by identifier (from cache).
    async fn get_algorithm(
        &self,
        algorithm_id: i32,
    ) -> Result<Option<AlgorithmInfo>, AiEngineError>;

    /// Paginated algorithm query with filters (from DB).
    async fn page_algorithms(
        &self,
        params: AlgorithmPageParams,
    ) -> Result<PageResult<AlgorithmInfo>, AiEngineError>;

    /// Test an algorithm with mock data.
    async fn test_algorithm(
        &self,
        algorithm_id: i32,
        test_input: AlgorithmTestInput,
    ) -> Result<AlgorithmTestResult, AiEngineError>;
}

// ── AI Sub-Trait: Inference Runtime ──────────────────────────────
//
// Owns the inference hot path: frame analysis, backpressure, status.

/// Inference runtime API — the real-time analysis hot path.
///
/// Manages frame submission, backpressure, latest-result cache,
/// engine status, and built-in processor discovery.
///
/// # Backpressure
///
/// When the engine cannot accept more frames, [`analyze_frame`] returns
/// [`AiEngineError::Backpressure`]. Callers should drop the frame and
/// continue (best-effort semantics for real-time video).
#[async_trait::async_trait]
pub trait AiInferenceRuntime: DowncastSync + Send + Sync + 'static {
    /// Submit a video frame for AI analysis.
    async fn analyze_frame(
        &self,
        request: FrameAnalysisRequest,
    ) -> Result<AnalysisResult, AiEngineError>;

    /// Check if the engine has capacity to accept a new frame (non-blocking).
    fn has_capacity(&self, channel_id: &i32) -> bool;

    /// Get the latest analysis result for a channel (for snapshot API).
    async fn get_latest_result(
        &self,
        channel_id: i32,
    ) -> Result<Option<AnalysisResult>, AiEngineError>;

    /// Get an aggregated engine status snapshot for monitoring/API.
    async fn get_engine_status(&self) -> Result<EngineStatus, AiEngineError>;

    /// List built-in preprocessors with their metadata and parameters.
    fn list_preprocessors(&self) -> Vec<ProcessorInfo>;

    /// List built-in postprocessors with their metadata and parameters.
    fn list_postprocessors(&self) -> Vec<ProcessorInfo>;
}

// ── AI Facade Trait ──────────────────────────────────────────────
//
// The unified entry point that composes all sub-traits.
// Gateway and web API layers interact with this single trait.

/// The unified AI Processing Engine API — gateway-wide entry point.
///
/// This is a composition trait that provides access to all AI subsystem
/// capabilities through a single `Arc<dyn AiEngineApi>`. Implementations
/// delegate to the specialized registries and runtime.
///
/// # Thread Safety
///
/// All methods are `&self` and internally synchronized. Implementations must be
/// safe to call from multiple driver instances concurrently.
pub trait AiEngineApi: DowncastSync + Send + Sync + 'static {
    /// Access the model registry sub-API.
    fn models(&self) -> &dyn AiModelRegistry;

    /// Access the pipeline registry sub-API.
    fn pipelines(&self) -> &dyn AiPipelineRegistry;

    /// Access the algorithm registry sub-API.
    fn algorithms(&self) -> &dyn AiAlgorithmRegistry;

    /// Access the inference runtime sub-API.
    fn runtime(&self) -> &dyn AiInferenceRuntime;
}

/// Trait for accessing southward channel connection states
pub trait SouthwardManager: DowncastSync + Send + Sync + 'static {
    /// Get the connection state for a channel
    ///
    /// Returns `None` if the channel is not found in the runtime manager.
    fn get_channel_connection_state(&self, channel_id: i32) -> Option<Arc<ConnectionState>>;
}

/// Trait for accessing northward app connection states
pub trait NorthwardManager: DowncastSync + Send + Sync + 'static {
    /// Get the connection state for an app
    ///
    /// Returns `None` if the app is not found in the runtime manager.
    fn get_app_connection_state(&self, app_id: i32) -> Option<Arc<ConnectionState>>;
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

    /// Write a value to a specific point identified by device ID and point key.
    ///
    /// This is the web-API entry point for control-plane writes. The gateway
    /// implementation resolves the point from the runtime index, validates
    /// access mode / data type / range, converts logical → wire value, and
    /// delegates to the underlying driver with per-channel serialization.
    ///
    /// The `value` is accepted as raw `serde_json::Value` because JSON has no
    /// concept of narrow numeric types (f32 vs f64, i8 vs i64, etc.).  The
    /// implementation uses `NGValue::try_from_json_scalar` with the point's
    /// declared `DataType` to produce the correctly-typed `NGValue`.
    ///
    /// # Arguments
    /// * `device_id`  - Target device identifier (primary key).
    /// * `point_key`  - Point key within the device (e.g. `"p1"`).
    /// * `value`      - Raw JSON value; converted to `NGValue` internally.
    /// * `timeout_ms` - Optional overall timeout in milliseconds (queue wait + driver I/O).
    async fn write_point(
        &self,
        device_id: i32,
        point_key: String,
        value: serde_json::Value,
        timeout_ms: Option<u64>,
    ) -> NGResult<()>;
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
