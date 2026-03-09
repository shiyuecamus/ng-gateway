pub mod buffer;
pub mod codec;
pub mod downlink;
pub mod envelope;
pub mod extension;
pub mod log;
pub mod mapping;
pub(crate) mod model;
pub mod payload;
pub mod probe;
pub mod runtime_api;
pub mod supervised;
pub mod template;
pub(crate) mod types;

use crate::{
    supervision::{NoopObserverFactory, ObserverFactory},
    ConnectionState, ExtensionStore, NorthwardResult,
};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use envelope::EnvelopeKind;
use model::{
    AlarmData, AttributeData, ClientRpcResponse, Command, DeviceConnectedData,
    DeviceDisconnectedData, ServerRpcResponse, TelemetryData, WritePoint, WritePointResponse,
};
use runtime_api::NorthwardRuntimeApi;
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, sync::Arc};
use tokio::sync::{broadcast, mpsc, watch};

/// Initialization context for northward plugins
///
/// This context provides plugins with necessary dependencies during initialization:
/// - **ExtensionManager**: For persistent plugin-specific data (e.g., provision credentials)
/// - **App ID**: For logging and metrics
/// - **App Name**: For identification
/// - **Config**: Plugin-specific configuration
/// - **Events Channel**: Channel for sending business events (RPC, Command, Attribute)
/// - **Retry Policy**: Retry policy for connection management
///
/// # Design Philosophy
/// - Aligned with southbound Driver initialization pattern
/// - Provides clean dependency injection
/// - Extensible for future requirements (e.g., metrics, event bus)
///
/// # Example
/// ```ignore
/// async fn init(&mut self, ctx: NorthwardInitContext) -> NorthwardResult<()> {
///     // Downcast config to plugin-specific type
///     let config = ctx.config.downcast_arc::<MyPluginConfig>()?;
///     
///     // Check for existing credentials
///     if let Some(creds) = ctx.extension_store.get("provision_credentials").await? {
///         // Use existing credentials
///     } else {
///         // Perform provision and store credentials
///         ctx.extension_store.set("provision_credentials", &creds).await?;
///     }
///     
///     // Use retry policy for connection management
///     let supervisor = MySupervisor::new(config, ctx.retry_policy, ctx.events_tx);
///     supervisor.run().await?;
///     
///     Ok(())
/// }
/// ```
#[derive(Clone)]
pub struct NorthwardInitContext {
    /// Extension store for plugin-specific persistent data (host-owned storage).
    pub extension_store: Arc<dyn ExtensionStore>,
    /// App ID for logging and metrics
    pub app_id: i32,
    /// App name for identification
    pub app_name: String,
    /// Plugin-specific configuration
    pub config: Arc<dyn PluginConfig>,
    /// Channel for sending business events (RPC, Command, Attribute)
    pub events_tx: mpsc::Sender<NorthwardEvent>,
    /// Read-only runtime API for high-throughput encoding paths.
    ///
    /// Plugins should treat this as a stable interface and avoid depending on
    /// gateway core internal data structures.
    pub runtime: Arc<dyn NorthwardRuntimeApi>,
    /// Retry policy for connection management with exponential backoff
    pub retry_policy: crate::RetryPolicy,
    /// Host-provided supervision observer factory (low-frequency control plane).
    pub observer_factory: Arc<dyn ObserverFactory>,
}

impl NorthwardInitContext {
    /// Attach a disabled/no-op observer configuration.
    ///
    /// This is intended for tests and offline tools that do not run inside the gateway host.
    #[inline]
    pub fn with_noop_observer(mut self) -> Self {
        self.observer_factory = Arc::new(NoopObserverFactory);
        self
    }
}

/// Define and export a northward plugin factory and metadata for dynamic loading.
///
/// This macro generates the required C ABI symbols so the gateway can perform
/// version/ABI gating and retrieve static metadata bytes with zero allocations.
///
/// It supports an optional `channel_capacity` argument (default: 1024) to configure
/// the backpressure buffer size for the plugin's internal actor loop.
///
/// Usage example in an external northward plugin crate:
///
/// ```ignore
/// use ng_gateway_sdk::{NorthwardPluginFactory, PluginConfigSchemas, ng_northward_factory};
///
/// fn build_metadata() -> PluginConfigSchemas { /* ... */ }
///
/// pub struct MyFactory;
/// impl NorthwardPluginFactory for MyFactory { /* ... */ }
///
/// // Standard usage (default buffer = 1024)
/// ng_plugin_factory!(
///     name = "ThingsBoard",
///     description = "ThingsBoard northbound adapter",
///     plugin_type = "thingsboard",
///     component = MyConnector,
///     metadata_fn = build_metadata
/// );
///
/// // High-throughput usage (custom buffer)
/// ng_plugin_factory!(
///     name = "Kafka",
///     plugin_type = "kafka",
///     component = MyConnector,
///     metadata_fn = build_metadata,
///     channel_capacity = 10000
/// );
/// ```
#[macro_export]
macro_rules! ng_plugin_factory {
    // Final form (component + model_convert): with description.
    (name = $name:expr, description = $description:expr, plugin_type = $plugin_type:expr, component = $component:ty, metadata_fn = $metadata_fn:path, model_convert = $model_convert:ty $(, channel_capacity = $cap:expr)? $(,)?) => {
        // Generated, per-library factory to avoid exposing generic extension points.
        struct __NgComponentPluginFactory {
            model_convert: $model_convert,
        }

        impl __NgComponentPluginFactory {
            /// Create a new factory instance.
            ///
            /// # Notes
            /// This MUST be low-frequency and MUST NOT perform any I/O.
            #[inline]
            fn new() -> Self {
                Self {
                    model_convert: <$model_convert as ::core::default::Default>::default(),
                }
            }
        }

        impl ::core::default::Default for __NgComponentPluginFactory {
            #[inline]
            fn default() -> Self {
                Self::new()
            }
        }

        impl $crate::PluginFactory for __NgComponentPluginFactory {
            fn create_plugin(
                &self,
                ctx: $crate::NorthwardInitContext,
            ) -> $crate::NorthwardResult<Box<dyn $crate::Plugin>> {
                // Compile-time contract checks (clear error messages for implementers).
                fn __assert_handle_is_northward_handle<H: $crate::NorthwardHandle>() {}
                __assert_handle_is_northward_handle::<<$component as $crate::supervision::Connector>::Handle>();

                // Compile-time contract check:
                // `Connector::InitContext` MUST be exactly `NorthwardInitContext`.
                fn __assert_init_ctx_is_northward_init_context<C>()
                where
                    C: $crate::supervision::Connector<InitContext = $crate::NorthwardInitContext>,
                {
                }
                __assert_init_ctx_is_northward_init_context::<$component>();

                use $crate::export::tracing::info_span;
                let span = info_span!(
                    "northward-plugin",
                    app_id = ctx.app_id,
                    plugin_type = $plugin_type
                );

                // NOTE: `new(ctx)` MUST be sync and MUST NOT perform I/O.
                let observer = ctx.observer_factory.create_northward(
                    $crate::supervision::NorthwardObserverLabels {
                        app_id: ctx.app_id,
                        plugin_kind: ::std::sync::Arc::<str>::from($plugin_type),
                    }
                );

                let retry_policy = ctx.retry_policy;
                let connector = <$component as $crate::supervision::Connector>::new(ctx)?;

                let params = $crate::supervision::SupervisorParams {
                    retry_policy,
                    reconnect_queue: 8,
                };
                let (loop_, _state_rx) = $crate::supervision::SupervisorLoop::new_with_span(
                    connector,
                    params,
                    observer,
                    span,
                );

                let plugin = $crate::SupervisedPlugin::new(loop_);
                Ok(Box::new(plugin))
            }

            fn convert_plugin_config(
                &self,
                config: $crate::export::serde_json::Value,
            ) -> $crate::NorthwardResult<std::sync::Arc<dyn $crate::PluginConfig>> {
                <$model_convert as $crate::supervision::converter::NorthwardModelConverter>::convert_plugin_config(
                    &self.model_convert,
                    config,
                )
            }
        }

        $crate::ng_plugin_factory!(
            @core name = $name,
            description = Some($description),
            plugin_type = $plugin_type,
            factory_ty = __NgComponentPluginFactory,
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)?
        );
    };

    // Final form (component + model_convert): NO description.
    (name = $name:expr, plugin_type = $plugin_type:expr, component = $component:ty, metadata_fn = $metadata_fn:path, model_convert = $model_convert:ty $(, channel_capacity = $cap:expr)? $(,)?) => {
        // Generated, per-library factory to avoid exposing generic extension points.
        struct __NgComponentPluginFactory {
            model_convert: $model_convert,
        }

        impl __NgComponentPluginFactory {
            #[inline]
            fn new() -> Self {
                Self {
                    model_convert: <$model_convert as ::core::default::Default>::default(),
                }
            }
        }

        impl ::core::default::Default for __NgComponentPluginFactory {
            #[inline]
            fn default() -> Self {
                Self::new()
            }
        }

        impl $crate::PluginFactory for __NgComponentPluginFactory {
            fn create_plugin(
                &self,
                ctx: $crate::NorthwardInitContext,
            ) -> $crate::NorthwardResult<Box<dyn $crate::Plugin>> {
                fn __assert_handle_is_northward_handle<H: $crate::NorthwardHandle>() {}
                __assert_handle_is_northward_handle::<<$component as $crate::supervision::Connector>::Handle>();

                // Compile-time contract check:
                // `Connector::InitContext` MUST be exactly `NorthwardInitContext`.
                fn __assert_init_ctx_is_northward_init_context<C>()
                where
                    C: $crate::supervision::Connector<InitContext = $crate::NorthwardInitContext>,
                {
                }
                __assert_init_ctx_is_northward_init_context::<$component>();

                use $crate::export::tracing::info_span;
                let span = info_span!(
                    "northward-plugin",
                    app_id = ctx.app_id,
                    plugin_type = $plugin_type
                );

                let observer = ctx.observer_factory.create_northward(
                    $crate::supervision::NorthwardObserverLabels {
                        app_id: ctx.app_id,
                        plugin_kind: ::std::sync::Arc::<str>::from($plugin_type),
                    }
                );

                let retry_policy = ctx.retry_policy;
                let connector = <$component as $crate::supervision::Connector>::new(ctx)?;

                let params = $crate::supervision::SupervisorParams {
                    retry_policy,
                    reconnect_queue: 8,
                };
                let (loop_, _state_rx) = $crate::supervision::SupervisorLoop::new_with_span(
                    connector,
                    params,
                    observer,
                    span,
                );

                let plugin = $crate::SupervisedPlugin::new(loop_);
                Ok(Box::new(plugin))
            }

            fn convert_plugin_config(
                &self,
                config: $crate::export::serde_json::Value,
            ) -> $crate::NorthwardResult<std::sync::Arc<dyn $crate::PluginConfig>> {
                <$model_convert as $crate::supervision::model_convert::NorthwardModelConverter>::convert_plugin_config(
                    &self.model_convert,
                    config,
                )
            }
        }

        $crate::ng_plugin_factory!(
            @core name = $name,
            description = None,
            plugin_type = $plugin_type,
            factory_ty = __NgComponentPluginFactory,
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)?
        );
    };

    // Core implementation with optional description and explicit factory ctor
    (@core name = $name:expr, description = $desc_opt:expr, plugin_type = $plugin_type:expr, factory_ty = $factory:ty, metadata_fn = $metadata_fn:path, channel_capacity = $cap:expr) => {
        #[no_mangle]
        pub extern "C" fn ng_plugin_api_version() -> u32 {
            $crate::sdk::sdk_api_version()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_sdk_version() -> *const ::std::os::raw::c_char {
            static SDK_VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $crate::ffi::cstring_sanitized($crate::sdk::SDK_VERSION))
            };
            SDK_VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_version() -> *const ::std::os::raw::c_char {
            static VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $crate::ffi::cstring_sanitized(env!("CARGO_PKG_VERSION")))
            };
            VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_type() -> *const ::std::os::raw::c_char {
            static TYPE_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $crate::ffi::cstring_sanitized($plugin_type))
            };
            TYPE_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_name() -> *const ::std::os::raw::c_char {
            static NAME_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $crate::ffi::cstring_sanitized($name))
            };
            NAME_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_description() -> *const ::std::os::raw::c_char {
            static DESC_STR: $crate::export::once_cell::sync::Lazy<Option<::std::ffi::CString>> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $desc_opt.map($crate::ffi::cstring_sanitized))
            };
            match DESC_STR.as_ref() {
                Some(c) => c.as_ptr(),
                None => ::std::ptr::null(),
            }
        }

        // Lazily materialize metadata JSON bytes inside the plugin to avoid
        // allocations across the FFI boundary. Host copies immediately.
        #[doc(hidden)]
        pub static NG_PLUGIN_METADATA_JSON: $crate::export::once_cell::sync::Lazy<Vec<u8>> = {
            use $crate::export::once_cell::sync::Lazy;
            use $crate::export::serde_json;
            Lazy::new(|| {
                let md: $crate::PluginConfigSchemas = $metadata_fn();
                // MUST NOT panic across FFI boundaries; return empty metadata on serialization error.
                serde_json::to_vec(&md).unwrap_or_else(|_| ::std::vec::Vec::new())
            })
        };

        /// Expose pointer and length to metadata JSON bytes. Ownership stays in plugin.
        #[no_mangle]
        pub unsafe extern "C" fn ng_plugin_metadata_json_ptr(
            out_ptr: *mut *const u8,
            out_len: *mut usize,
        ) {
            $crate::ffi::write_slice_ptr_len(out_ptr, out_len, &NG_PLUGIN_METADATA_JSON);
        }

        /// Register host log sink into this plugin library.
        ///
        /// # Returns
        /// - 0: ok
        /// - 1: abi mismatch
        #[no_mangle]
        pub extern "C" fn ng_plugin_set_log_sink(sink: $crate::log::LogSinkV1) -> u32 {
            $crate::northward::log::set_log_sink(sink)
        }

        /// Best-effort dynamic max log level control for this plugin library.
        ///
        /// Level mapping:
        /// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
        #[no_mangle]
        pub extern "C" fn ng_plugin_set_max_level(level: u8) -> u32 {
            $crate::northward::log::set_max_level(level)
        }

        #[no_mangle]
        pub extern "C" fn create_plugin_factory() -> *mut dyn $crate::PluginFactory {
            let inner: Box<dyn $crate::PluginFactory> =
                Box::new(<$factory as ::core::default::Default>::default());
            let rt_handle = NG_RUNTIME.as_ref().map(|rt| rt.handle().clone());
            let wrapper: Box<dyn $crate::PluginFactory> =
                Box::new($crate::ffi::RuntimeAwarePluginFactory::new(inner, $cap, rt_handle));
            Box::into_raw(wrapper)
        }

        #[doc(hidden)]
        pub static NG_RUNTIME: $crate::export::once_cell::sync::Lazy<Option<tokio::runtime::Runtime>> = {
            use $crate::export::once_cell::sync::Lazy;
            Lazy::new(|| $crate::ffi::build_runtime(concat!($plugin_type, "-plugin")))
        };

        /// Initialize tracing for this northward plugin library.
        #[no_mangle]
        pub extern "C" fn ng_plugin_init_tracing(debug: bool) {
            // Install the plugin->host bridge subscriber (NOT a local fmt subscriber).
            //
            // This ensures plugin logs enter the unified host logging pipeline and are
            // governed by runtime log-control policies (global/channel/app overrides).
            let handle = NG_RUNTIME
                .as_ref()
                .map(|rt| rt.handle().clone())
                .or(tokio::runtime::Handle::try_current().ok());
            if let Some(h) = handle {
                $crate::northward::log::init_plugin_tracing(h, debug);
            }
        }
    };
}

impl_downcast!(sync NorthwardPublisher);
impl_downcast!(sync PluginFactory);
impl_downcast!(sync Plugin);
impl_downcast!(sync PluginConfig);

/// Publisher interface used by drivers to send northbound data efficiently.
///
/// Implementations should be non-blocking and back pressure-aware. Prefer
/// batched publishing to reduce per-item overhead on hot paths.
pub trait NorthwardPublisher: DowncastSync + Send + Sync + Debug {
    /// Try to publish a single item without blocking. Implementations should
    /// propagate back pressure via an error instead of awaiting.
    fn try_publish(&self, data: Arc<NorthwardData>) -> NorthwardResult<()>;
}

/// Factory trait for creating northward plugin instances
pub trait PluginFactory: DowncastSync + Send + Sync {
    /// Create a new northward plugin instance with initialization context (synchronous, no I/O)
    ///
    /// Implementations must:
    /// - Validate and capture all required dependencies from `ctx`
    /// - Construct internal state and channels
    /// - NOT perform any blocking or network I/O (that belongs in `Plugin::start`)
    ///
    /// Returns a plugin that is "ready but not connected".
    fn create_plugin(&self, ctx: NorthwardInitContext) -> NorthwardResult<Box<dyn Plugin>>;

    /// Convert a channel model to a runtime channel
    fn convert_plugin_config(
        &self,
        config: serde_json::Value,
    ) -> NorthwardResult<Arc<dyn PluginConfig>>;
}

pub trait PluginConfig: DowncastSync + Send + Sync + Debug {}

/// Northward plugin trait (host-facing ABI contract).
///
/// # Final architecture (recommended)
/// In the final supervision architecture, **external plugin crates do NOT implement**
/// this trait directly. Instead, plugin authors implement `supervision::Connector/Session/Handle`
/// plus an inherent `fn new(ctx: NorthwardInitContext)`, and the SDK macro generates a
/// `PluginFactory` that returns `SupervisedPlugin<C>`, which implements this `Plugin` trait.
///
/// This keeps connection governance (retry/budget/state publication/handle publication)
/// inside the SDK supervision loop and makes behavior consistent across plugins.
///
/// # Transitional notes
/// The trait remains public for ABI compatibility during migration.
#[async_trait]
pub trait Plugin: DowncastSync + Send + Sync {
    /// Start the plugin (asynchronous). Spawn supervisors and establish connections.
    ///
    /// This method should:
    /// - Spawn the connection supervisor task (non-blocking to caller aside from strategy waits)
    /// - Perform provisioning if needed
    /// - Attempt initial connection and manage retries according to `retry_policy`
    /// - Update connection state via `watch::Sender`
    /// - Send business events via `events_tx`
    async fn start(&self) -> NorthwardResult<()>;

    /// Subscribe to connection state changes (aligned with Driver::subscribe_connection_state)
    ///
    /// Returns a `watch::Receiver` that reflects the plugin's current connection state.
    /// AppActor subscribes via this method to monitor connection lifecycle.
    ///
    /// # Returns
    /// A cloneable receiver for connection state updates
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // In AppActor
    /// let state_rx = plugin.subscribe_connection_state();
    /// tokio::spawn(async move {
    ///     while state_rx.changed().await.is_ok() {
    ///         let state = state_rx.borrow().clone();
    ///         match state {
    ///             ConnectionState::Connected => { /* handle */ }
    ///             ConnectionState::Disconnected => { /* handle */ }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// ```
    fn subscribe_connection_state(&self) -> watch::Receiver<Arc<ConnectionState>>;

    /// Process outbound data using internal connection
    ///
    /// This method is called by AppActor when southbound data needs to be
    /// sent to the platform. It should:
    /// - Check if connection is available (via internal state)
    /// - Convert data to platform-specific format
    /// - Send data using internal connection
    /// - Return quickly (non-blocking)
    ///
    /// # Arguments
    /// * `data` - Internal data to send (telemetry, attributes, etc.)
    ///
    /// # Returns
    /// * `Ok(())` - Data sent successfully
    /// * `Err(NorthwardError::NotConnected)` - Connection not available
    /// * `Err(...)` - Send failed
    ///
    /// # Examples
    ///
    /// ```ignore
    /// async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()> {
    ///     let client = self.client.read().await;
    ///     let client = client.as_ref().ok_or(NorthwardError::NotConnected)?;
    ///     
    ///     let payload = self.encode(data)?;
    ///     client.publish("topic", payload).await?;
    ///     
    ///     Ok(())
    /// }
    /// ```
    async fn process_data(&self, data: Arc<NorthwardData>) -> NorthwardResult<()>;

    /// Stop the plugin and cancel connection supervisor
    ///
    /// This method should:
    /// - Cancel the shutdown token (stops supervisor task)
    /// - Disconnect gracefully
    /// - Clean up internal resources
    /// - Be idempotent (safe to call multiple times)
    ///
    /// # Returns
    /// * `Ok(())` - Stopped successfully
    /// * `Err(...)` - Stop failed (usually safe to ignore)
    ///
    /// # Examples
    ///
    /// ```ignore
    /// async fn stop(&self) -> NorthwardResult<()> {
    ///     self.shutdown_token.cancel();
    ///     
    ///     if let Some(client) = self.client.write().await.take() {
    ///         client.disconnect().await?;
    ///     }
    ///     
    ///     Ok(())
    /// }
    /// ```
    async fn stop(&self) -> NorthwardResult<()>;
}

/// Northward data types
/// Gateway -> Northward
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NorthwardData {
    /// Device Connected
    DeviceConnected(DeviceConnectedData),
    /// Device Disconnected
    DeviceDisconnected(DeviceDisconnectedData),
    /// Telemetry data from devices
    Telemetry(TelemetryData),
    /// Attribute updates from devices
    Attributes(AttributeData),
    /// Alarm notifications
    Alarm(AlarmData),
    /// RPC responses
    RpcResponse(ClientRpcResponse),
    /// Write-point response (async RPC reply)
    WritePointResponse(WritePointResponse),
}

impl NorthwardData {
    /// Return the stable `EnvelopeKind` discriminator for this data variant.
    ///
    /// This mapping is **authoritative** across all northward plugins.
    #[inline]
    pub fn envelope_kind(&self) -> EnvelopeKind {
        match self {
            NorthwardData::DeviceConnected(_) => EnvelopeKind::DeviceConnected,
            NorthwardData::DeviceDisconnected(_) => EnvelopeKind::DeviceDisconnected,
            NorthwardData::Telemetry(_) => EnvelopeKind::Telemetry,
            NorthwardData::Attributes(_) => EnvelopeKind::Attributes,
            NorthwardData::Alarm(_) => EnvelopeKind::Alarm,
            NorthwardData::RpcResponse(_) => EnvelopeKind::RpcResponse,
            NorthwardData::WritePointResponse(_) => EnvelopeKind::WritePointResponse,
        }
    }
}

impl NorthwardData {
    pub fn device_id(&self) -> i32 {
        match self {
            NorthwardData::DeviceConnected(data) => data.device_id,
            NorthwardData::DeviceDisconnected(data) => data.device_id,
            NorthwardData::Telemetry(data) => data.device_id,
            NorthwardData::Attributes(data) => data.device_id,
            NorthwardData::Alarm(data) => data.device_id,
            NorthwardData::RpcResponse(data) => data.device_id,
            NorthwardData::WritePointResponse(data) => data.device_id,
        }
    }
}

/// Type alias for command receiver channel
pub type EventReceiver = broadcast::Receiver<NorthwardEvent>;

/// Business events emitted by northward plugins (aligned with southbound design)
///
/// **Design Philosophy**:
/// - Connection lifecycle is managed via `watch::Receiver<ConnectionState>`
/// - This enum only contains **business events** that need Gateway-level processing
/// - Plugins send these events via `events_tx` provided during initialization
///
/// **Event Flow**:
/// ```text
/// Plugin (business logic)
///   → events_tx.send(NorthwardEvent::RpcResponseReceived(...))
///   → AppActor event bridge
///   → Gateway event handler
///   → Route to southbound devices
/// ```
///
/// **Connection State Flow** (separate channel):
/// ```text
/// Plugin (supervisor task)
///   → conn_state_tx.send(ConnectionState::Connected)
///   → AppActor subscribes via subscribe_connection_state()
///   → AppActor updates internal state
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NorthwardEvent {
    // Platform-to-Gateway business events (require Gateway-level processing)
    /// RPC response received from platform
    ///
    /// This event is sent when the platform responds to a client RPC request.
    /// Gateway should route this response back to the originating device.
    RpcResponseReceived(ServerRpcResponse),

    /// Command received from platform
    ///
    /// This event is sent when the platform sends a command to a device.
    /// Gateway should route this command to the target device via southbound.
    CommandReceived(Command),

    /// Write-point request from platform (control-plane)
    ///
    /// This event is sent when a northward plugin needs to write a point through Gateway.
    /// Gateway will validate + serialize (per-channel) + dispatch to southward driver and reply
    /// via `NorthwardData::WritePointResponse`.
    WritePoint(WritePoint),
}

impl NorthwardEvent {
    /// Return the stable `EnvelopeKind` discriminator for this event variant.
    ///
    /// This mapping is **authoritative** across all northward plugins.
    #[inline]
    pub fn envelope_kind(&self) -> EnvelopeKind {
        match self {
            NorthwardEvent::RpcResponseReceived(_) => EnvelopeKind::RpcResponseReceived,
            NorthwardEvent::CommandReceived(_) => EnvelopeKind::CommandReceived,
            NorthwardEvent::WritePoint(_) => EnvelopeKind::WritePoint,
        }
    }
}
