pub(crate) mod codec;
pub mod log;
pub(crate) mod model;
pub mod probe;
pub mod supervised;
pub mod transport;
pub(crate) mod types;
pub(crate) mod validation;
pub mod wire;

use crate::{ConnectionState, DriverResult, NGValue, NorthwardData, Transform};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use model::{
    ActionModel, ChannelModel, ConnectionPolicy, DeviceModel, PointModel, SouthwardInitContext,
};
use std::{fmt, fmt::Debug, sync::Arc};
use tokio::sync::watch;
use types::{AccessMode, CollectionType, DataPointType, DataType, ReportType, Status};

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
///     component = MyConnector,
///     metadata_fn = build_metadata
/// );
///
/// // High-concurrency usage (custom buffer)
/// ng_driver_factory!(
///     name = "Advanced Driver",
///     driver_type = "advanced",
///     component = MyConnector,
///     metadata_fn = build_metadata,
///     channel_capacity = 500
/// );
/// ```
#[macro_export]
macro_rules! ng_driver_factory {
    // Final form (component + model_convert): with description.
    (name = $name:expr, description = $description:expr, driver_type = $driver_type:expr, component = $component:ty, metadata_fn = $metadata_fn:path, model_convert = $model_convert:ty $(, channel_capacity = $cap:expr)? $(, collect_max_inflight = $collect_max_inflight:expr)? $(,)?) => {
        // Generated, per-library factory to avoid exposing generic extension points.
        struct __NgComponentDriverFactory {
            model_convert: $model_convert,
        }

        impl __NgComponentDriverFactory {
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

        impl ::core::default::Default for __NgComponentDriverFactory {
            #[inline]
            fn default() -> Self {
                Self::new()
            }
        }

        impl $crate::DriverFactory for __NgComponentDriverFactory {
            fn create_driver(
                &self,
                ctx: $crate::SouthwardInitContext,
            ) -> $crate::DriverResult<Box<dyn $crate::Driver>> {
                // Compile-time contract checks (clear error messages for implementers).
                fn __assert_handle_is_southward_handle<H: $crate::SouthwardHandle>() {}
                __assert_handle_is_southward_handle::<<$component as $crate::supervision::Connector>::Handle>();

                // Compile-time contract check:
                // `Connector::InitContext` MUST be exactly `SouthwardInitContext`.
                fn __assert_init_ctx_is_southward_init_context<C>()
                where
                    C: $crate::supervision::Connector<InitContext = $crate::SouthwardInitContext>,
                {
                }
                __assert_init_ctx_is_southward_init_context::<$component>();

                use $crate::export::tracing::info_span;
                let span = info_span!(
                    "southward-driver",
                    channel_id = ctx.channel_id,
                    driver_type = $driver_type
                );

                // NOTE: `Connector::new(ctx)` MUST be sync and MUST NOT perform I/O.
                let observer = ctx.observer_factory.create_southward(
                    $crate::supervision::SouthwardObserverLabels {
                        channel_id: ctx.channel_id,
                        driver_kind: ::std::sync::Arc::<str>::from($driver_type),
                    }
                );

                // IMPORTANT:
                // Southward retry/backoff policy is configured per-channel via
                // `runtime_channel.connection_policy().backoff`.
                //
                // If we used `SupervisorParams::default()` here, any DB/UI config
                // like `maxAttempts` would be ignored and drivers would retry forever.
                let retry_policy = ctx.runtime_channel.connection_policy().backoff.clone();

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

                let collect_max_inflight: usize = 1usize;
                $(let collect_max_inflight: usize = $collect_max_inflight;)?
                let driver = $crate::SupervisedDriver::new_with_collect_max_inflight(loop_, collect_max_inflight);
                Ok(Box::new(driver))
            }

            fn convert_runtime_channel(
                &self,
                channel: $crate::ChannelModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeChannel>> {
                <$model_convert as $crate::supervision::converter::SouthwardModelConverter>::convert_runtime_channel(
                    &self.model_convert,
                    channel,
                )
            }

            fn convert_runtime_device(
                &self,
                device: $crate::DeviceModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeDevice>> {
                <$model_convert as $crate::supervision::converter::SouthwardModelConverter>::convert_runtime_device(
                    &self.model_convert,
                    device,
                )
            }

            fn convert_runtime_point(
                &self,
                point: $crate::PointModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimePoint>> {
                <$model_convert as $crate::supervision::converter::SouthwardModelConverter>::convert_runtime_point(
                    &self.model_convert,
                    point,
                )
            }

            fn convert_runtime_action(
                &self,
                action: $crate::ActionModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeAction>> {
                <$model_convert as $crate::supervision::converter::SouthwardModelConverter>::convert_runtime_action(
                    &self.model_convert,
                    action,
                )
            }
        }

        $crate::ng_driver_factory!(
            @core name = $name,
            description = Some($description),
            driver_type = $driver_type,
            factory_ty = __NgComponentDriverFactory,
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };

    // Final form (component + model_convert): NO description.
    (name = $name:expr, driver_type = $driver_type:expr, component = $component:ty, metadata_fn = $metadata_fn:path, model_convert = $model_convert:ty $(, channel_capacity = $cap:expr)? $(, collect_max_inflight = $collect_max_inflight:expr)? $(,)?) => {
        // Reuse the same generated factory, but export NULL description.
        struct __NgComponentDriverFactory {
            model_convert: $model_convert,
        }

        impl __NgComponentDriverFactory {
            #[inline]
            fn new() -> Self {
                Self {
                    model_convert: <$model_convert as ::core::default::Default>::default(),
                }
            }
        }

        impl ::core::default::Default for __NgComponentDriverFactory {
            #[inline]
            fn default() -> Self {
                Self::new()
            }
        }

        impl $crate::DriverFactory for __NgComponentDriverFactory {
            fn create_driver(
                &self,
                ctx: $crate::SouthwardInitContext,
            ) -> $crate::DriverResult<Box<dyn $crate::Driver>> {
                fn __assert_handle_is_southward_handle<H: $crate::SouthwardHandle>() {}
                __assert_handle_is_southward_handle::<<$component as $crate::supervision::Connector>::Handle>();

                // Compile-time contract check:
                // `Connector::InitContext` MUST be exactly `SouthwardInitContext`.
                fn __assert_init_ctx_is_southward_init_context<C>()
                where
                    C: $crate::supervision::Connector<InitContext = $crate::SouthwardInitContext>,
                {
                }
                __assert_init_ctx_is_southward_init_context::<$component>();

                use $crate::export::tracing::info_span;
                let span = info_span!(
                    "southward-driver",
                    channel_id = ctx.channel_id,
                    driver_type = $driver_type
                );

                let observer = ctx.observer_factory.create_southward(
                    $crate::supervision::SouthwardObserverLabels {
                        channel_id: ctx.channel_id,
                        driver_kind: ::std::sync::Arc::<str>::from($driver_type),
                    }
                );

                // IMPORTANT: honor per-channel retry policy from `ConnectionPolicy`.
                let retry_policy = ctx.runtime_channel.connection_policy().backoff.clone();

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

                let collect_max_inflight: usize = 1usize;
                $(let collect_max_inflight: usize = $collect_max_inflight;)?
                let driver = $crate::SupervisedDriver::new_with_collect_max_inflight(loop_, collect_max_inflight);
                Ok(Box::new(driver))
            }

            fn convert_runtime_channel(
                &self,
                channel: $crate::ChannelModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeChannel>> {
                <$model_convert as $crate::supervision::model_convert::SouthwardModelConverter>::convert_runtime_channel(
                    &self.model_convert,
                    channel,
                )
            }

            fn convert_runtime_device(
                &self,
                device: $crate::DeviceModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeDevice>> {
                <$model_convert as $crate::supervision::model_convert::SouthwardModelConverter>::convert_runtime_device(
                    &self.model_convert,
                    device,
                )
            }

            fn convert_runtime_point(
                &self,
                point: $crate::PointModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimePoint>> {
                <$model_convert as $crate::supervision::model_convert::SouthwardModelConverter>::convert_runtime_point(
                    &self.model_convert,
                    point,
                )
            }

            fn convert_runtime_action(
                &self,
                action: $crate::ActionModel,
            ) -> $crate::DriverResult<std::sync::Arc<dyn $crate::RuntimeAction>> {
                <$model_convert as $crate::supervision::model_convert::SouthwardModelConverter>::convert_runtime_action(
                    &self.model_convert,
                    action,
                )
            }
        }

        $crate::ng_driver_factory!(
            @core name = $name,
            description = None,
            driver_type = $driver_type,
            factory_ty = __NgComponentDriverFactory,
            metadata_fn = $metadata_fn,
            channel_capacity = 100 $(+ $cap * 0 + $cap)?
        );
    };

    // Private core to eliminate duplication. Accepts Option description.
    (@core name = $name:expr, description = $desc_opt:expr, driver_type = $driver_type:expr, factory_ty = $factory:ty, metadata_fn = $metadata_fn:path, channel_capacity = $cap:expr) => {
        /// Convert a `&'static str` to a C string safely.
        ///
        /// # Notes
        /// - This MUST NOT panic.
        /// - If the input contains an interior NUL, it will be sanitized to a space (`0x20`).
        #[inline]
        fn __ng_cstring_sanitized(input: &'static str) -> ::std::ffi::CString {
            let bytes = input.as_bytes();
            let mut buf: ::std::vec::Vec<u8> = ::std::vec::Vec::with_capacity(bytes.len() + 1);
            for &b in bytes.iter() {
                buf.push(if b == 0 { b' ' } else { b });
            }
            // Ensure NUL-termination (no interior NULs after sanitization).
            buf.push(0);
            // SAFETY: we ensured there are no interior NULs and we appended a terminator.
            unsafe { ::std::ffi::CString::from_vec_unchecked(buf) }
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_api_version() -> u32 {
            $crate::sdk::sdk_api_version()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_sdk_version() -> *const ::std::os::raw::c_char {
            static SDK_VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| __ng_cstring_sanitized($crate::sdk::SDK_VERSION))
            };
            SDK_VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_version() -> *const ::std::os::raw::c_char {
            static VER: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| __ng_cstring_sanitized(env!("CARGO_PKG_VERSION")))
            };
            VER.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_type() -> *const ::std::os::raw::c_char {
            static TYPE_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| __ng_cstring_sanitized($driver_type))
            };
            TYPE_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_name() -> *const ::std::os::raw::c_char {
            static NAME_STR: $crate::export::once_cell::sync::Lazy<::std::ffi::CString> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| __ng_cstring_sanitized($name))
            };
            NAME_STR.as_ptr()
        }

        #[no_mangle]
        pub extern "C" fn ng_driver_description() -> *const ::std::os::raw::c_char {
            static DESC_STR: $crate::export::once_cell::sync::Lazy<Option<::std::ffi::CString>> = {
                use $crate::export::once_cell::sync::Lazy;
                Lazy::new(|| $desc_opt.map(__ng_cstring_sanitized))
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
                // MUST NOT panic across FFI boundaries; return empty metadata on serialization error.
                serde_json::to_vec(&md).unwrap_or_else(|_| ::std::vec::Vec::new())
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

        /// Register the host log sink (FFI callback) for this driver library.
        ///
        /// # Notes
        /// The host MUST call this before `ng_driver_init_tracing` so the driver can start
        /// flushing bridged logs immediately after initialization.
        #[no_mangle]
        pub extern "C" fn ng_driver_set_log_sink(sink: $crate::log::LogSinkV1) -> u32 {
            $crate::log::set_log_sink(sink)
        }

        /// Set the driver's runtime max log level (dynamic).
        ///
        /// Level mapping:
        /// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
        #[no_mangle]
        pub extern "C" fn ng_driver_set_max_level(level: u8) -> u32 {
            $crate::log::set_max_level(level)
        }

        /// Get the driver's current runtime max log level (dynamic).
        #[no_mangle]
        pub extern "C" fn ng_driver_get_max_level() -> u8 {
            $crate::log::get_max_level()
        }



        #[no_mangle]
        pub extern "C" fn create_driver_factory() -> *mut dyn $crate::DriverFactory {
            let inner: Box<dyn $crate::DriverFactory> =
                Box::new(<$factory as ::core::default::Default>::default());
            let rt_handle = NG_RUNTIME.as_ref().map(|rt| rt.handle().clone());
            let wrapper: Box<dyn $crate::DriverFactory> =
                Box::new($crate::ffi::RuntimeAwareDriverFactory::new(inner, $cap, rt_handle));
            Box::into_raw(wrapper)
        }

        #[doc(hidden)]
        pub static NG_RUNTIME: $crate::export::once_cell::sync::Lazy<Option<tokio::runtime::Runtime>> = {
            use $crate::export::once_cell::sync::Lazy;
            Lazy::new(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name(concat!($driver_type, "-driver"))
                    .build()
                    .ok()
            })
        };

        /// Initialize tracing for this driver library.
        ///
        /// This installs a lightweight subscriber + bridge layer that captures `tracing`
        /// (and optionally `log`) records and flushes them to the host via `LogSinkV1`.
        ///
        /// # Arguments
        ///
        /// * `debug` - When true, the initial max level is set to DEBUG; otherwise INFO.
        #[no_mangle]
        pub extern "C" fn ng_driver_init_tracing(debug: bool) {
            let Some(rt) = NG_RUNTIME.as_ref() else {
                return;
            };
            $crate::log::init_driver_tracing(rt.handle().clone(), debug);
        }
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

    /// Return a "physical collection group" key for a business device.
    ///
    /// When this returns `Some(key)`, the Collector will group devices by this key and call
    /// `collect_data()` once per group, passing multiple items in a single call.
    ///
    /// When this returns `None`, the Collector will call `collect_data()` with exactly one item.
    ///
    /// # Performance note
    /// This method must be **fast** and should avoid allocations.
    #[inline]
    fn collection_group_key(&self, _device: &dyn RuntimeDevice) -> Option<CollectionGroupKey> {
        None
    }

    /// Collect data from specified devices and points (group-aware batch API).
    ///
    /// # Input invariants (enforced by the Collector)
    /// - The Collector will never call this with an empty slice.
    /// - If `collection_group_key()` returns `None` for a device, the Collector guarantees
    ///   `items.len() == 1`.
    /// - If `collection_group_key()` returns `Some(k)`, the Collector guarantees all items
    ///   in the slice belong to the same `k`.
    async fn collect_data(&self, items: &[CollectItem]) -> DriverResult<Vec<NorthwardData>>;

    /// Maximum number of in-flight `collect_data()` calls allowed for this driver instance.
    ///
    /// # Design notes
    /// - Default is `1` to preserve strict serialization for legacy and third-party drivers.
    /// - Drivers that maintain a **TCP connection pool** (or other parallel I/O lanes) should
    ///   override this to match their pool size to unlock collection concurrency.
    /// - The SDK runtime wrapper does **not** enforce ordering between `collect_data` and
    ///   control-plane operations. Drivers must enforce any required serialization at the
    ///   physical link/session/connection layer (e.g., via `Mutex` or per-connection workers).
    ///
    /// # Performance
    /// This method must be **fast** and must not allocate.
    #[inline]
    fn collect_max_inflight(&self) -> usize {
        1
    }

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
        value: &NGValue,
        timeout_ms: Option<u64>,
    ) -> DriverResult<WriteResult>;

    /// Subscribe to structured connection state updates.
    ///
    /// Implementations must return a `watch::Receiver<Arc<ConnectionState>>` which reflects
    /// the driver's current connectivity as a **snapshot stream** (watch semantics).
    fn subscribe_connection_state(&self) -> watch::Receiver<Arc<ConnectionState>>;

    /// Apply runtime delta (default no-op)
    async fn apply_runtime_delta(&self, _delta: RuntimeDelta) -> DriverResult<()> {
        Ok(())
    }
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

    /// Get the logical-layer transform rules for this point.
    ///
    /// This is always present. Identity semantics are defined by `Transform`.
    fn transform(&self) -> &Transform;

    /// Get the wire data type (protocol-level, memory-layout semantics).
    ///
    /// This is a convenience alias for `data_type()` to improve readability
    /// in hot-path driver code.
    #[inline]
    fn wire_data_type(&self) -> DataType {
        self.data_type()
    }

    /// Get the logical data type (northward-facing semantics).
    ///
    /// This is derived from `transform().transform_data_type` and falls back to the
    /// wire data type when not configured.
    #[inline]
    fn logical_data_type(&self) -> DataType {
        self.transform().resolve_logical_datatype(self.data_type())
    }
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

    /// Get the logical-layer transform rules for this parameter.
    fn transform(&self) -> &Transform;

    /// Get the wire data type (protocol-level, memory-layout semantics).
    #[inline]
    fn wire_data_type(&self) -> DataType {
        self.data_type()
    }

    /// Get the logical data type (gateway-facing semantics).
    #[inline]
    fn logical_data_type(&self) -> DataType {
        self.transform().resolve_logical_datatype(self.data_type())
    }
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

/// A stable "physical collection group" identifier for grouping devices within a protocol driver.
///
/// # Design goals
/// - **Fixed-size value type**: safe to use as a `HashMap` key with zero allocations.
/// - **Object-safe**: usable from `dyn Driver` without generics/lifetimes.
/// - **Cross-protocol safe**: callers should set a `kind` namespace to avoid collisions.
///
/// # Wire format
/// The key is 16 bytes:
/// - Bytes `[0..4)`: `kind` (big-endian `u32`)
/// - Bytes `[4..16)`: protocol-defined payload (12 bytes)
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct CollectionGroupKey(pub [u8; 16]);

impl CollectionGroupKey {
    /// Build a key from a `kind` namespace and a `u64` value.
    ///
    /// Layout: `[kind:4][0:4][v:8]` (big-endian).
    #[inline]
    pub fn from_u64(kind: u32, v: u64) -> Self {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&kind.to_be_bytes());
        out[8..16].copy_from_slice(&v.to_be_bytes());
        Self(out)
    }

    /// Build a key from a `kind` namespace and two `u64` values.
    ///
    /// Layout: `[kind:4][a_low48:6][b_low48:6]` (big-endian).
    ///
    /// Note: this intentionally truncates both inputs to 48 bits to fit 12 bytes payload.
    /// For full-width identity, prefer `from_bytes` or `from_hash128`.
    #[inline]
    pub fn from_pair_u64(kind: u32, a: u64, b: u64) -> Self {
        #[inline]
        fn write_u48_be(dst: &mut [u8], v: u64) {
            let x = v & 0x0000_FFFF_FFFF_FFFF;
            dst[0] = ((x >> 40) & 0xFF) as u8;
            dst[1] = ((x >> 32) & 0xFF) as u8;
            dst[2] = ((x >> 24) & 0xFF) as u8;
            dst[3] = ((x >> 16) & 0xFF) as u8;
            dst[4] = ((x >> 8) & 0xFF) as u8;
            dst[5] = (x & 0xFF) as u8;
        }

        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&kind.to_be_bytes());
        write_u48_be(&mut out[4..10], a);
        write_u48_be(&mut out[10..16], b);
        Self(out)
    }

    /// Build a key from a `kind` namespace and an arbitrary 12-byte payload.
    #[inline]
    pub fn from_bytes(kind: u32, payload: [u8; 12]) -> Self {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&kind.to_be_bytes());
        out[4..16].copy_from_slice(&payload);
        Self(out)
    }

    /// Build a key from a `kind` namespace and a stable 128-bit hash.
    ///
    /// The payload stores the first 12 bytes of the hash; callers should choose a
    /// stable hash function and seed to keep grouping keys consistent across restarts.
    #[inline]
    pub fn from_hash128(kind: u32, hash: [u8; 16]) -> Self {
        let mut payload = [0u8; 12];
        payload.copy_from_slice(&hash[0..12]);
        Self::from_bytes(kind, payload)
    }

    /// Return the `kind` namespace embedded in this key.
    #[inline]
    pub fn kind(&self) -> u32 {
        u32::from_be_bytes([self.0[0], self.0[1], self.0[2], self.0[3]])
    }
}

impl fmt::Debug for CollectionGroupKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CollectionGroupKey(kind=0x{:08X}, payload=0x",
            self.kind()
        )?;
        for b in &self.0[4..16] {
            write!(f, "{:02X}", b)?;
        }
        write!(f, ")")
    }
}

/// A single collection item passed from the Collector to a driver.
///
/// Each item represents a business device with its points. Drivers may aggregate multiple
/// items that belong to the same physical session/group (e.g., Modbus slave ID).
pub type CollectItem = (Arc<dyn RuntimeDevice>, Arc<[Arc<dyn RuntimePoint>]>);
