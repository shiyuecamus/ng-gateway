pub(crate) mod codec;
pub(crate) mod loader;
pub(crate) mod model;
pub(crate) mod types;
pub(crate) mod validation;
pub mod wire;

use crate::{DriverResult, NGValue, NorthwardData};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use model::{
    ActionModel, ChannelModel, ConnectionPolicy, DeviceModel, DriverHealth, PointModel,
    SouthwardInitContext,
};
use std::{fmt::Debug, sync::Arc};
use tokio::sync::watch::Receiver;
use types::{
    AccessMode, CollectionType, DataPointType, DataType, ReportType, SouthwardConnectionState,
    Status,
};

/// Driver-layer execute result (Driver -> Gateway).
#[derive(Debug, Clone)]
pub struct ExecuteResult {
    pub outcome: ExecuteOutcome,
    /// Optional structured payload (low-frequency control plane).
    pub payload: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteOutcome {
    Completed,
    Queued,
}

/// Define and export a driver factory and metadata (UiSchema) for dynamic loading.
///
/// This macro generates the required C ABI symbols so the gateway can perform
/// version/ABI gating and retrieve static metadata bytes with zero allocations.
///
/// It supports an optional `channel_capacity` argument (default: 100) to configure
/// the buffer size for the driver's internal actor command queue.
///
/// Usage example in an external driver crate (no proc-macro dependency required):
///
/// ```ignore
/// use ng_gateway_sdk::{SouthwardDriverFactory, DriverSchemas, ng_driver_define_factory};
///
/// fn build_metadata() -> DriverSchemas { /* ... */ }
///
/// pub struct MyFactory;
/// impl SouthwardDriverFactory for MyFactory { /* ... */ }
///
/// // Standard usage (default buffer = 100)
/// ng_driver_factory!(
///     name = "Modbus",
///     description = "Modbus protocol driver",
///     driver_type = "modbus",
///     factory = MyFactory,
///     metadata_fn = build_metadata
/// );
///
/// // High-concurrency usage (custom buffer)
/// ng_driver_factory!(
///     name = "Advanced Driver",
///     driver_type = "advanced",
///     factory = MyFactory,
///     metadata_fn = build_metadata,
///     channel_capacity = 500
/// );
/// ```
#[macro_export]
macro_rules! ng_driver_factory {
    // Private core to eliminate duplication. Accepts Option description.
    (@core name = $name:expr, description = $desc_opt:expr, driver_type = $driver_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path, channel_capacity = $cap:expr) => {
        #[no_mangle]
        pub extern "C" fn ng_driver_api_version() -> u32 {
            $crate::sdk::sdk_api_version()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_sdk_version() -> *const ::std::os::raw::c_char {
            static SDK_VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($crate::sdk::SDK_VERSION).unwrap())
            };
            SDK_VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_version() -> *const ::std::os::raw::c_char {
            static VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new(env!("CARGO_PKG_VERSION")).unwrap())
            };
            VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_type() -> *const ::std::os::raw::c_char {
            static TYPE_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($driver_type).unwrap())
            };
            TYPE_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_name() -> *const ::std::os::raw::c_char {
            static NAME_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| ::std::ffi::CString::new($name).unwrap())
            };
            NAME_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_description() -> *const ::std::os::raw::c_char {
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
        pub static NG_DRIVER_METADATA_JSON: $crate::export::once_cell::sync::Lazy<Vec<u8>> = {
            use $crate::export::once_cell::sync::Lazy;
            use $crate::export::serde_json;
            Lazy::new(|| {
                let md: $crate::DriverSchemas = $metadata_fn();
                serde_json::to_vec(&md).expect("serialize driver metadata")
            })
        };

        /// Expose pointer and length to metadata JSON bytes. Ownership stays in plugin.
        #[no_mangle]
        pub unsafe extern "C" fn ng_driver_metadata_json_ptr(
            out_ptr: *mut *const u8,
            out_len: *mut usize,
        ) {
            if out_ptr.is_null() || out_len.is_null() {
                return;
            }
            // Safe because we only write to provided pointers and the source slice is static.
            *out_ptr = NG_DRIVER_METADATA_JSON.as_ptr();
            *out_len = NG_DRIVER_METADATA_JSON.len();
        }

        // Internal message definition: Convert function calls to messages
        enum DriverMessage {
            Collect {
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                points: std::sync::Arc<[std::sync::Arc<dyn $crate::RuntimePoint>]>,
                reply: tokio::sync::oneshot::Sender<$crate::DriverResult<Vec<$crate::NorthwardData>>>,
            },
            Execute {
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                action: std::sync::Arc<dyn $crate::RuntimeAction>,
                parameters: Vec<(std::sync::Arc<dyn $crate::RuntimeParameter>, $crate::NGValue)>,
                reply: tokio::sync::oneshot::Sender<$crate::DriverResult<$crate::ExecuteResult>>,
            },
            Write {
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                point: std::sync::Arc<dyn $crate::RuntimePoint>,
                value: $crate::NGValue,
                timeout_ms: Option<u64>,
                reply: tokio::sync::oneshot::Sender<$crate::DriverResult<$crate::WriteResult>>,
            },
            ApplyDelta {
                delta: $crate::RuntimeDelta,
                reply: tokio::sync::oneshot::Sender<$crate::DriverResult<()>>,
            },
        }

        // Wrapper types to ensure runtime context isolation
        struct RuntimeAwareDriver {
            inner: std::sync::Arc<Box<dyn $crate::Driver>>,
            tx: tokio::sync::mpsc::Sender<DriverMessage>,
            cancel_token: $crate::export::tokio_util::sync::CancellationToken,
            // Only used during start() to take ownership of the receiver
            rx: std::sync::Mutex<Option<tokio::sync::mpsc::Receiver<DriverMessage>>>,
        }

        impl std::fmt::Debug for RuntimeAwareDriver {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("RuntimeAwareDriver")
                 .field("inner", &"dyn Driver")
                 .finish()
            }
        }

        #[async_trait::async_trait]
        impl $crate::Driver for RuntimeAwareDriver {
            async fn start(&self) -> $crate::DriverResult<()> {
                let inner = self.inner.clone();
                let cancel_token = self.cancel_token.clone();

                let mut rx = {
                    let mut rx_opt = self.rx.lock().unwrap();
                    rx_opt.take().ok_or_else(|| $crate::DriverError::ExecutionError("Driver already started".to_string()))?
                };

                let handle = NG_RUNTIME.handle();
                let (tx_res, rx_res) = tokio::sync::oneshot::channel();

                handle.spawn(async move {
                    if let Err(e) = inner.start().await {
                        let _ = tx_res.send(Err(e));
                        return;
                    }
                    let _ = tx_res.send(Ok(()));

                    // Driver Actor Loop: Strictly serialized execution, naturally thread-safe
                    use $crate::export::tracing::debug;
                    debug!("Driver actor loop started");

                    loop {
                        tokio::select! {
                            _ = cancel_token.cancelled() => {
                                debug!("Driver cancelled, stopping actor loop");
                                break;
                            }
                            maybe_msg = rx.recv() => {
                                match maybe_msg {
                                    Some(msg) => {
                                        match msg {
                                            DriverMessage::Collect { device, points, reply } => {
                                                let res = inner.collect_data(device, points).await;
                                                let _ = reply.send(res);
                                            }
                                            DriverMessage::Execute { device, action, parameters, reply } => {
                                                let res = inner.execute_action(device, action, parameters).await;
                                                let _ = reply.send(res);
                                            }
                                            DriverMessage::Write { device, point, value, timeout_ms, reply } => {
                                                let res = inner.write_point(device, point, value, timeout_ms).await;
                                                let _ = reply.send(res);
                                            }
                                            DriverMessage::ApplyDelta { delta, reply } => {
                                                let res = inner.apply_runtime_delta(delta).await;
                                                let _ = reply.send(res);
                                            }
                                        }
                                    }
                                    None => break,
                                }
                            }
                        }
                    }
                    debug!("Driver actor loop stopped");
                    let _ = inner.stop().await;
                });

                match rx_res.await {
                    Ok(res) => res,
                    Err(_) => Err($crate::DriverError::ExecutionError("Driver start task cancelled".to_string())),
                }
            }

            async fn stop(&self) -> $crate::DriverResult<()> {
                self.cancel_token.cancel();
                Ok(())
            }

            async fn collect_data(
                &self,
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                data_points: std::sync::Arc<[std::sync::Arc<dyn $crate::RuntimePoint>]>,
            ) -> $crate::DriverResult<Vec<$crate::NorthwardData>> {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let msg = DriverMessage::Collect { device, points: data_points, reply: reply_tx };

                self.tx.send(msg).await.map_err(|_| $crate::DriverError::ExecutionError("Driver runtime closed".to_string()))?;

                reply_rx.await.map_err(|_| $crate::DriverError::ExecutionError("Driver task cancelled".to_string()))?
            }

            async fn execute_action(
                &self,
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                action: std::sync::Arc<dyn $crate::RuntimeAction>,
                parameters: Vec<(std::sync::Arc<dyn $crate::RuntimeParameter>, $crate::NGValue)>,
            ) -> $crate::DriverResult<$crate::ExecuteResult> {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let msg = DriverMessage::Execute { device, action, parameters, reply: reply_tx };

                self.tx.send(msg).await.map_err(|_| $crate::DriverError::ExecutionError("Driver runtime closed".to_string()))?;

                reply_rx.await.map_err(|_| $crate::DriverError::ExecutionError("Driver task cancelled".to_string()))?
            }

            async fn write_point(
                &self,
                device: std::sync::Arc<dyn $crate::RuntimeDevice>,
                point: std::sync::Arc<dyn $crate::RuntimePoint>,
                value: $crate::NGValue,
                timeout_ms: Option<u64>,
            ) -> $crate::DriverResult<$crate::WriteResult> {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let msg = DriverMessage::Write { device, point, value, timeout_ms, reply: reply_tx };

                self.tx.send(msg).await.map_err(|_| $crate::DriverError::ExecutionError("Driver runtime closed".to_string()))?;

                reply_rx.await.map_err(|_| $crate::DriverError::ExecutionError("Driver task cancelled".to_string()))?
            }

            async fn apply_runtime_delta(&self, delta: $crate::RuntimeDelta) -> $crate::DriverResult<()> {
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                let msg = DriverMessage::ApplyDelta { delta, reply: reply_tx };

                self.tx.send(msg).await.map_err(|_| $crate::DriverError::ExecutionError("Driver runtime closed".to_string()))?;

                reply_rx.await.map_err(|_| $crate::DriverError::ExecutionError("Driver task cancelled".to_string()))?
            }

            fn subscribe_connection_state(&self) -> tokio::sync::watch::Receiver<$crate::SouthwardConnectionState> {
                self.inner.subscribe_connection_state()
            }

            async fn health_check(&self) -> $crate::DriverResult<$crate::DriverHealth> {
                self.inner.health_check().await
            }
        }

        struct RuntimeAwareFactory {
            inner: Box<dyn $crate::DriverFactory>,
        }

        impl $crate::DriverFactory for RuntimeAwareFactory {
            fn create_driver(&self, ctx: $crate::SouthwardInitContext) -> $crate::DriverResult<Box<dyn $crate::Driver>> {
                let inner_driver = self.inner.create_driver(ctx)?;
                let (tx, rx) = tokio::sync::mpsc::channel($cap);
                let cancel_token = $crate::export::tokio_util::sync::CancellationToken::new();

                Ok(Box::new(RuntimeAwareDriver {
                    inner: std::sync::Arc::new(inner_driver),
                    tx,
                    cancel_token,
                    rx: std::sync::Mutex::new(Some(rx)),
                }))
            }

            fn convert_runtime_channel(
                &self,
                channel: $crate::ChannelModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeChannel>> {
                self.inner.convert_runtime_channel(channel)
            }

            fn convert_runtime_device(&self, device: $crate::DeviceModel) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeDevice>> {
                self.inner.convert_runtime_device(device)
            }

            fn convert_runtime_point(&self, point: $crate::PointModel) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimePoint>> {
                self.inner.convert_runtime_point(point)
            }

            fn convert_runtime_action(&self, action: $crate::ActionModel) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeAction>> {
                self.inner.convert_runtime_action(action)
            }
        }

        #[no_mangle]
        pub extern "C" fn create_driver_factory() -> *mut dyn $crate::DriverFactory {
            let inner: Box<dyn $crate::DriverFactory> = Box::new(($ctor)());
            let wrapper: Box<dyn $crate::DriverFactory> = Box::new(RuntimeAwareFactory { inner });
            Box::into_raw(wrapper)
        }

        #[doc(hidden)]
        pub static NG_RUNTIME: $crate::export::once_cell::sync::Lazy<tokio::runtime::Runtime> = {
            use $crate::export::once_cell::sync::Lazy;
            Lazy::new(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name(concat!($driver_type, "-driver"))
                    .build()
                    .expect("build driver runtime")
            })
        };

        /// Initialize tracing for this driver library.
        ///
        /// This function sets up a driver-specific tracing configuration. When
        /// debug mode is enabled, it uses `Level::DEBUG` and a pretty formatter
        /// with file and line numbers. Otherwise, it configures `Level::INFO`
        /// and hides file and line number details to reduce overhead.
        ///
        /// # Arguments
        ///
        /// * `is_debug` - When `Some(true)`, enables debug-level and pretty output;
        ///                otherwise defaults to non-debug INFO level.
        #[no_mangle]
        pub extern "C" fn ng_driver_init_tracing(debug: bool) {
            use $crate::export::tracing::Level;
            // Only depend on fmt to avoid requiring optional features like env-filter
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

    // Public API: with explicit constructor and description
    (name = $name:expr, description = $description:expr, driver_type = $driver_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_driver_factory!(
            @core name = $name,
            description = Some($description),
            driver_type = $driver_type,
            factory = $factory,
            factory_ctor = $ctor,
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };

    // Public API: default constructor and description (preferred order)
    (name = $name:expr, description = $description:expr, driver_type = $driver_type:expr, factory = $factory:ty, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_driver_factory!(
            @core name = $name,
            description = Some($description),
            driver_type = $driver_type,
            factory = $factory,
            factory_ctor = || <$factory as ::core::default::Default>::default(),
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };

    // Public API: with explicit constructor and NO description
    (name = $name:expr, driver_type = $driver_type:expr, factory = $factory:ty, factory_ctor = $ctor:expr, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_driver_factory!(
            @core name = $name,
            description = None,
            driver_type = $driver_type,
            factory = $factory,
            factory_ctor = $ctor,
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };

    // Public API: default constructor and NO description
    (name = $name:expr, driver_type = $driver_type:expr, factory = $factory:ty, metadata_fn = $metadata_fn:path $(, channel_capacity = $cap:expr)?) => {
        $crate::ng_driver_factory!(
            @core name = $name,
            description = None,
            driver_type = $driver_type,
            factory = $factory,
            factory_ctor = || <$factory as ::core::default::Default>::default(),
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };
}

impl_downcast!(sync DriverFactory);
impl_downcast!(sync Driver);
impl_downcast!(sync RuntimeChannel);
impl_downcast!(sync RuntimeDevice);
impl_downcast!(sync RuntimePoint);
impl_downcast!(sync RuntimeAction);
impl_downcast!(sync RuntimeParameter);
impl_downcast!(sync DriverConfig);

/// Runtime change events that notify drivers of model updates at run-time.
///
/// These deltas are scoped to a single channel instance and are delivered in-order
/// (the delivery mechanism should ensure serialization per channel).
#[derive(Debug, Clone)]
pub enum RuntimeDelta {
    /// Device-level lifecycle and status changes
    DevicesChanged {
        added: Vec<Arc<dyn RuntimeDevice>>,
        updated: Vec<Arc<dyn RuntimeDevice>>,
        removed: Vec<Arc<dyn RuntimeDevice>>,
        status_changed: Vec<(Arc<dyn RuntimeDevice>, Status)>,
    },

    /// Points changed for a device (Removed passes full runtime points)
    PointsChanged {
        device: Arc<dyn RuntimeDevice>,
        added: Vec<Arc<dyn RuntimePoint>>,
        updated: Vec<Arc<dyn RuntimePoint>>,
        removed: Vec<Arc<dyn RuntimePoint>>,
    },

    /// Actions changed for a device (Removed passes full runtime actions)
    ActionsChanged {
        device: Arc<dyn RuntimeDevice>,
        added: Vec<Arc<dyn RuntimeAction>>,
        updated: Vec<Arc<dyn RuntimeAction>>,
        removed: Vec<Arc<dyn RuntimeAction>>,
    },
}

/// Factory trait for creating driver instances
#[async_trait]
pub trait DriverFactory: DowncastSync + Send + Sync {
    /// Create a new driver instance with initialization context (synchronous, no I/O).
    ///
    /// Implementations must:
    /// - Validate and capture all required dependencies from `ctx`
    /// - Construct internal state and channels
    /// - NOT perform any blocking or network I/O (that belongs in `Driver::start`)
    ///
    /// Returns a driver that is "ready but not connected".
    fn create_driver(&self, ctx: SouthwardInitContext) -> DriverResult<Box<dyn Driver>>;

    /// Convert a channel model to a runtime channel
    fn convert_runtime_channel(
        &self,
        channel: ChannelModel,
    ) -> DriverResult<Arc<dyn RuntimeChannel>>;

    /// Convert a device model to a runtime device
    fn convert_runtime_device(&self, device: DeviceModel) -> DriverResult<Arc<dyn RuntimeDevice>>;

    /// Convert a point model to a runtime point
    fn convert_runtime_point(&self, point: PointModel) -> DriverResult<Arc<dyn RuntimePoint>>;

    /// Convert an action model to a runtime action
    fn convert_runtime_action(&self, action: ActionModel) -> DriverResult<Arc<dyn RuntimeAction>>;
}

/// Core driver trait that all protocol drivers must implement
///
/// This trait defines the essential operations for any communication driver,
/// providing a unified interface for data collection, command execution,
/// and connection management.
#[async_trait]
pub trait Driver: DowncastSync + Send + Sync {
    /// Start the driver (asynchronous).
    ///
    /// Spawn workers, establish connections to field buses/devices, and begin periodic tasks.
    async fn start(&self) -> DriverResult<()>;

    /// Stop the driver and release resources.
    ///
    /// This method uses `&self` because implementations should use internal
    /// synchronization mechanisms (e.g., `RwLock`) to manage mutable state
    /// during shutdown. This allows stopping a driver that is already wrapped
    /// in `Arc` without requiring mutable access.
    async fn stop(&self) -> DriverResult<()>;

    /// Collect data from specified points
    async fn collect_data(
        &self,
        device: Arc<dyn RuntimeDevice>,
        data_points: Arc<[Arc<dyn RuntimePoint>]>,
    ) -> DriverResult<Vec<NorthwardData>>;

    /// Execute an action/command
    async fn execute_action(
        &self,
        device: Arc<dyn RuntimeDevice>,
        action: Arc<dyn RuntimeAction>,
        parameters: Vec<(Arc<dyn RuntimeParameter>, NGValue)>,
    ) -> DriverResult<ExecuteResult>;

    /// Write a single point (control-plane).
    ///
    /// Drivers must implement this to provide protocol-native write semantics.
    async fn write_point(
        &self,
        device: Arc<dyn RuntimeDevice>,
        point: Arc<dyn RuntimePoint>,
        value: NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult>;

    /// Subscribe to connection state updates.
    ///
    /// Implementations must return a `watch::Receiver<ConnectionState>` which reflects
    /// the driver's current connectivity. The receiver should be valid and must never block.
    fn subscribe_connection_state(&self) -> Receiver<SouthwardConnectionState>;

    /// Apply runtime delta (default no-op)
    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }

    /// Get driver health status
    async fn health_check(&self) -> DriverResult<DriverHealth>;
}

/// Driver-layer write result (Driver -> Gateway).
#[derive(Debug, Clone)]
pub struct WriteResult {
    pub outcome: WriteOutcome,
    pub applied_value: Option<NGValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Applied,
    Queued,
}

pub trait DriverConfig: DowncastSync + Send + Sync + Debug {}

/// Channel trait for channel-specific runtime settings
pub trait RuntimeChannel: DowncastSync + Send + Sync + Debug {
    /// Get the channel's unique identifier
    fn id(&self) -> i32;
    /// Get the channel's name
    fn name(&self) -> &str;
    /// Get the channel's driver id
    fn driver_id(&self) -> i32;
    /// Get the channel's collection type
    fn collection_type(&self) -> CollectionType;
    /// Get the channel's report type
    fn report_type(&self) -> ReportType;
    /// Get the channel's period
    fn period(&self) -> Option<u32>;
    /// Get the channel's status
    fn status(&self) -> Status;
    /// Get the channel's connection policy
    fn connection_policy(&self) -> &ConnectionPolicy;
    /// Get the channel's configuration
    fn config(&self) -> &dyn DriverConfig;
}

pub trait RuntimeDevice: DowncastSync + Send + Sync + Debug {
    /// Get the device's unique identifier
    fn id(&self) -> i32;

    /// Get the device's name
    fn device_name(&self) -> &str;

    /// Get the device's key
    fn device_type(&self) -> &str;

    /// Get the device's channel ID
    fn channel_id(&self) -> i32;

    /// Get the device's status
    fn status(&self) -> Status;
}

/// Data point trait for protocol-specific data definitions
pub trait RuntimePoint: DowncastSync + Send + Sync + Debug {
    /// Get the data point's unique identifier
    fn id(&self) -> i32;

    /// Get the data point's device ID
    fn device_id(&self) -> i32;

    /// Get the data point's name
    fn name(&self) -> &str;

    /// Get the data point's key
    fn key(&self) -> &str;

    /// Get the data point's type
    fn r#type(&self) -> DataPointType;

    /// Get the data point's data type
    fn data_type(&self) -> DataType;

    /// Get the data point's access mode (Read, Write, ReadWrite)
    fn access_mode(&self) -> AccessMode;

    /// Get the data point's unit
    fn unit(&self) -> Option<&str>;

    /// Get the data point's minimum value
    fn min_value(&self) -> Option<f64>;

    /// Get the data point's maximum value
    fn max_value(&self) -> Option<f64>;

    /// Get the data point's scale
    fn scale(&self) -> Option<f64>;
}

pub trait RuntimeParameter: DowncastSync + Send + Sync + Debug {
    /// Get the parameter's name
    fn name(&self) -> &str;

    /// Get the parameter's key
    fn key(&self) -> &str;

    /// Get the parameter's data type
    fn data_type(&self) -> DataType;

    /// Get the parameter's required status
    fn required(&self) -> bool;

    /// Get the parameter's default value
    fn default_value(&self) -> Option<serde_json::Value>;

    /// Get the parameter's max value
    fn max_value(&self) -> Option<f64>;

    /// Get the parameter's min value
    fn min_value(&self) -> Option<f64>;
}

/// Action trait for protocol-specific RPC commands
pub trait RuntimeAction: DowncastSync + Send + Sync + Debug {
    /// Get the action's unique identifier
    fn id(&self) -> i32;

    /// Get the action's name
    fn name(&self) -> &str;

    /// Get the action's device ID
    fn device_id(&self) -> i32;

    /// Get the action's command
    fn command(&self) -> &str;

    /// Get input parameters for this action
    fn input_parameters(&self) -> Vec<Arc<dyn RuntimeParameter>>;
}
