pub mod codec;
pub mod downlink;
pub mod envelope;
pub mod extension;
pub(crate) mod loader;
pub mod mapping;
pub(crate) mod model;
pub mod payload;
pub mod runtime_api;
pub mod template;
pub(crate) mod types;

use crate::{NorthwardError, NorthwardResult};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use envelope::EnvelopeKind;
use model::{
    AlarmData, AttributeData, ClientRpcResponse, Command, DeviceConnectedData,
    DeviceDisconnectedData, ServerRpcResponse, TelemetryData, WritePoint, WritePointResponse,
};
use runtime_api::NorthwardRuntimeApi;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::{fmt::Debug, time::Duration};
use tokio::sync::{broadcast, mpsc, watch};
use types::NorthwardConnectionState;

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
///     if let Some(creds) = ctx.extension_manager.get("provision_credentials").await? {
///         // Use existing credentials
///     } else {
///         // Perform provision and store credentials
///         ctx.extension_manager.set("provision_credentials", &creds).await?;
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
    /// Extension manager for plugin-specific persistent data
    pub extension_manager: Arc<dyn extension::ExtensionManager>,
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
///     factory = MyFactory,
///     metadata_fn = build_metadata
/// );
///
/// // High-throughput usage (custom buffer)
/// ng_plugin_factory!(
///     name = "Kafka",
///     plugin_type = "kafka",
///     factory = MyFactory,
///     metadata_fn = build_metadata,
///     channel_capacity = 10000
/// );
/// ```
#[macro_export]
macro_rules! ng_plugin_factory {
    // Core implementation with optional description and explicit factory ctor
    (@core name = $name:expr, description = $desc_opt:expr, plugin_type = $plugin_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path, channel_capacity = $cap:expr) => {
        #[no_mangle]
        pub extern "C" fn ng_plugin_api_version() -> u32 {
            $crate::sdk::sdk_api_version()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_sdk_version() -> *const ::std::os::raw::c_char {
            static SDK_VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($crate::sdk::SDK_VERSION).unwrap())
            };
            SDK_VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_version() -> *const ::std::os::raw::c_char {
            static VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new(env!("CARGO_PKG_VERSION")).unwrap())
            };
            VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_type() -> *const ::std::os::raw::c_char {
            static TYPE_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($plugin_type).unwrap())
            };
            TYPE_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_name() -> *const ::std::os::raw::c_char {
            static NAME_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($name).unwrap())
            };
            NAME_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_plugin_description() -> *const ::std::os::raw::c_char {
            static DESC_STR: $crate::export::once_cell::sync::Lazy<Option<::std::ffi::CString>> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $desc_opt.map(|d| ::std::ffi::CString::new(d).unwrap()))
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
                serde_json::to_vec(&md).expect("serialize northward metadata")
            })
        };

        /// Expose pointer and length to metadata JSON bytes. Ownership stays in plugin.
        #[no_mangle]
        pub unsafe extern "C" fn ng_plugin_metadata_json_ptr(
            out_ptr: *mut *const u8,
            out_len: *mut usize,
        ) {
            if out_ptr.is_null() || out_len.is_null() {
                return;
            }
            *out_ptr = NG_PLUGIN_METADATA_JSON.as_ptr();
            *out_len = NG_PLUGIN_METADATA_JSON.len();
        }

        // Define wrapper types to ensure runtime context isolation
        struct RuntimeAwarePlugin {
            inner: std::sync::Arc<Box<dyn $crate::Plugin>>,
            tx: tokio::sync::mpsc::Sender<std::sync::Arc<$crate::NorthwardData>>,
            cancel_token: $crate::export::tokio_util::sync::CancellationToken,
            // Only used during start() to take ownership of the receiver
            rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<std::sync::Arc<$crate::NorthwardData>>>>,
        }

        impl std::fmt::Debug for RuntimeAwarePlugin {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("RuntimeAwarePlugin")
                 .field("inner", &"dyn Plugin")
                 .field("channel_capacity", &self.tx.capacity())
                 .finish()
            }
        }

        #[async_trait::async_trait]
        impl $crate::Plugin for RuntimeAwarePlugin {
            async fn start(&self) -> $crate::NorthwardResult<()> {
                let inner = self.inner.clone();
                let cancel_token = self.cancel_token.clone();

                // 1. Take ownership of the receiver (once only)
                let mut rx = {
                    let mut rx_opt = self.rx.lock().unwrap();
                    rx_opt.take().ok_or_else(|| $crate::NorthwardError::RuntimeError {
                        reason: "Plugin already started".to_string()
                    })?
                };

                let handle = NG_RUNTIME.handle();
                let (tx_res, rx_res) = tokio::sync::oneshot::channel();

                handle.spawn(async move {
                    // A. Initialize internal plugin
                    if let Err(e) = inner.start().await {
                        let _ = tx_res.send(Err(e));
                        return;
                    }
                    // Notify Host that start is successful
                    let _ = tx_res.send(Ok(()));

                    // B. Start Actor Consumer Loop
                    use $crate::export::tracing::{debug, warn};
                    debug!("Plugin actor loop started");

                    loop {
                        tokio::select! {
                            // 1. Priority: Handle cancellation
                            _ = cancel_token.cancelled() => {
                                debug!("Plugin cancelled, stopping actor loop");
                                break;
                            }
                            // 2. Handle incoming data
                            maybe_msg = rx.recv() => {
                                match maybe_msg {
                                    Some(data) => {
                                        // High-performance streaming processing
                                        if let Err(e) = inner.process_data(data).await {
                                             warn!("Error processing northward data: {}", e);
                                        }
                                    }
                                    None => {
                                        // Sender closed (Host dropped the plugin)
                                        debug!("Plugin data channel closed");
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    debug!("Plugin actor loop stopped");
                    // C. Graceful shutdown: Ensure inner.stop is called
                    let _ = inner.stop().await;
                });

                // Wait for inner.start() to complete
                match rx_res.await {
                    Ok(res) => res,
                    Err(_) => Err($crate::NorthwardError::RuntimeError { reason: "Plugin start task cancelled".to_string() }),
                }
            }

            fn subscribe_connection_state(&self) -> tokio::sync::watch::Receiver<$crate::NorthwardConnectionState> {
                self.inner.subscribe_connection_state()
            }

            async fn process_data(&self, data: std::sync::Arc<$crate::NorthwardData>) -> $crate::NorthwardResult<()> {
                // Non-blocking enqueue with backpressure
                match self.tx.try_send(data) {
                    Ok(_) => Ok(()),
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        Err($crate::NorthwardError::RuntimeError { reason: "Plugin buffer full - backpressure applied".to_string() })
                    },
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        Err($crate::NorthwardError::NotConnected)
                    }
                }
            }

            async fn stop(&self) -> $crate::NorthwardResult<()> {
                // Signal cancellation to the actor loop
                self.cancel_token.cancel();
                Ok(())
            }

            async fn ping(&self) -> $crate::NorthwardResult<std::time::Duration> {
                let inner = self.inner.clone();
                let handle = NG_RUNTIME.handle();
                let (tx, rx) = tokio::sync::oneshot::channel();
                handle.spawn(async move {
                    let res = inner.ping().await;
                    let _ = tx.send(res);
                });
                match rx.await {
                    Ok(res) => res,
                    Err(_) => Err($crate::NorthwardError::RuntimeError { reason: "Plugin ping task cancelled".to_string() }),
                }
            }
        }

        struct RuntimeAwareFactory {
            inner: Box<dyn $crate::PluginFactory>,
        }

        impl $crate::PluginFactory for RuntimeAwareFactory {
            fn create_plugin(&self, ctx: $crate::NorthwardInitContext) -> $crate::NorthwardResult<Box<dyn $crate::Plugin>> {
                let inner_plugin = self.inner.create_plugin(ctx)?;

                // Create bounded channel for backpressure with user-defined or default capacity
                let (tx, rx) = tokio::sync::mpsc::channel($cap);
                let cancel_token = $crate::export::tokio_util::sync::CancellationToken::new();

                Ok(Box::new(RuntimeAwarePlugin {
                    inner: std::sync::Arc::new(inner_plugin),
                    tx,
                    cancel_token,
                    rx: std::sync::Mutex::new(Some(rx)),
                }))
            }

            fn convert_plugin_config(
                &self,
                config: $crate::export::serde_json::Value,
            ) -> $crate::NorthwardResult<std::sync::Arc<dyn $crate::PluginConfig>> {
                self.inner.convert_plugin_config(config)
            }
        }

        #[no_mangle]
        pub extern "C" fn create_plugin_factory() -> *mut dyn $crate::PluginFactory {
            let inner: Box<dyn $crate::PluginFactory> = Box::new(($ctor)());
            let wrapper: Box<dyn $crate::PluginFactory> = Box::new(RuntimeAwareFactory { inner });
            Box::into_raw(wrapper)
        }

        #[doc(hidden)]
        pub static NG_RUNTIME: $crate::export::once_cell::sync::Lazy<tokio::runtime::Runtime> = {
            use $crate::export::once_cell::sync::Lazy;
            Lazy::new(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name(concat!($plugin_type, "-plugin"))
                    .build()
                    .expect("build plugin runtime")
            })
        };

        /// Initialize tracing for this northward plugin library.
        #[no_mangle]
        pub extern "C" fn ng_plugin_init_tracing(debug: bool) {
            use $crate::export::tracing::Level;
            use $crate::export::tracing_subscriber::fmt;

            if debug {
                let _ = fmt()
                    .pretty()
                    .with_line_number(true)
                    .with_file(true)
                    .with_max_level(Level::DEBUG)
                    .try_init();
            } else {
                let _ = fmt()
                    .with_line_number(false)
                    .with_file(false)
                    .with_max_level(Level::INFO)
                    .try_init();
            };
        }
    };

    // Public API: explicit constructor and description
    (name = $name:expr, description = $description:expr, plugin_type = $plugin_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_plugin_factory!(
            @core name = $name,
            description = Some($description),
            plugin_type = $plugin_type,
            factory = $factory,
            factory_ctor = $ctor,
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)? // Magic to use $cap if present, else 1024
        );
    };

    // Public API: default constructor and description
    (name = $name:expr, description = $description:expr, plugin_type = $plugin_type:expr, factory = $factory:ty, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_plugin_factory!(
            @core name = $name,
            description = Some($description),
            plugin_type = $plugin_type,
            factory = $factory,
            factory_ctor = || <$factory as ::core::default::Default>::default(),
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)?
        );
    };

    // Public API: explicit constructor and NO description
    (name = $name:expr, plugin_type = $plugin_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_plugin_factory!(
            @core name = $name,
            description = None,
            plugin_type = $plugin_type,
            factory = $factory,
            factory_ctor = $ctor,
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)?
        );
    };

    // Public API: default constructor and NO description
    (name = $name:expr, plugin_type = $plugin_type:expr, factory = $factory:ty, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_plugin_factory!(
            @core name = $name,
            description = None,
            plugin_type = $plugin_type,
            factory = $factory,
            factory_ctor = || <$factory as ::core::default::Default>::default(),
            metadata_fn = $metadata_fn,
            channel_capacity = 1024 $(+ $cap * 0 + $cap)?
        );
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

/// Northward plugin trait - self-supervised protocol adapter (aligned with southbound Driver)
///
/// # Design Philosophy
///
/// This trait is **fully aligned with southbound Driver design**:
/// - Plugin manages its own connection lifecycle (connect, reconnect, disconnect)
/// - Plugin spawns internal supervisor task for connection monitoring
/// - External components subscribe to connection state via `watch::Receiver`
/// - Plugin sends business events (RPC, Command, Attribute) via event channel
///
/// ## Responsibility Separation
///
/// **Plugin responsibilities (Self-Supervised)**:
/// - Initialize and spawn internal connection supervisor task
/// - Manage connection lifecycle (connect, exponential backoff retry, disconnect)
/// - Maintain internal `watch::Sender<NorthwardConnectionState>` for state broadcasting
/// - Send business events (RPC responses, commands, attributes) via `events_tx`
/// - Encode/decode protocol-specific messages
/// - Implement platform-specific authentication and provisioning
///
/// **AppActor responsibilities (Observer)**:
/// - Subscribe to plugin connection state via `subscribe_connection_state()`
/// - Route data to plugin via `process_data()`
/// - Forward business events to Gateway
/// - Manage plugin lifecycle (start/stop)
///
/// ## Comparison with Southbound Driver
///
/// | Aspect | Southbound Driver | Northward Plugin |
/// |--------|------------------|------------------|
/// | Connection Management | Driver internal supervisor | Plugin internal supervisor |
/// | State Broadcasting | `watch::Sender<SouthwardConnectionState>` | `watch::Sender<NorthwardConnectionState>` |
/// | State Subscription | `subscribe_connection_state()` | `subscribe_connection_state()` |
/// | External Monitor | `ChannelMonitor` subscribes | `AppActor` subscribes |
/// | Business Events | Device data via `NorthwardPublisher` | RPC/Command via `events_tx` |
///
/// ## Benefits
///
/// 1. **Architecture Consistency**: North and south use identical patterns
/// 2. **Plugin Autonomy**: Each plugin controls its own reconnection strategy
/// 3. **Decoupling**: AppActor doesn't need to know connection details
/// 4. **Flexibility**: Different plugins can use different retry policies
/// 5. **Simplicity**: No redundant `is_connected()` checks needed
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
    ///             NorthwardConnectionState::Connected => { /* handle */ }
    ///             NorthwardConnectionState::Disconnected => { /* handle */ }
    ///             _ => {}
    ///         }
    ///     }
    /// });
    /// ```
    fn subscribe_connection_state(&self) -> watch::Receiver<NorthwardConnectionState>;

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

    /// Optional: Health check / ping
    ///
    /// Returns the latency if connection is healthy.
    /// Default implementation returns NotConnected error.
    async fn ping(&self) -> NorthwardResult<Duration> {
        Err(NorthwardError::NotConnected)
    }
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
/// - Connection lifecycle is managed via `watch::Receiver<NorthwardConnectionState>`
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
///   → conn_state_tx.send(NorthwardConnectionState::Connected)
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
