mod bin_inspect;
mod error;
#[doc(hidden)]
pub mod ffi;
pub mod mqtt;
pub mod northward;
mod retry;
mod southward;
pub mod supervision;
mod transform;
mod ui_schema;
mod value;

/// Internal re-exports for use in generated symbols/macros to avoid version drift
pub mod export {
    pub use once_cell;
    pub use serde_json;
    pub use tokio_util;
    pub use tracing;
    pub use tracing_subscriber;
}

/// Public log bridge API (used by both host loader and `cdylib` drivers).
///
/// # Notes
/// This is intentionally re-exported at crate root so the `ng_driver_factory!` macro
/// can reference a stable path (`$crate::log::*`) from external driver crates.
pub mod log {
    pub use crate::southward::log::*;
}

pub type DriverResult<T> = Result<T, DriverError>;
pub type NorthwardResult<T> = Result<T, NorthwardError>;

pub use bin_inspect::{
    ensure_current_platform_from_bytes, ensure_current_platform_from_path, inspect_binary,
    BinaryArch, BinaryInfo, BinaryOsType,
};
pub use error::{DriverError, NorthwardError};
pub use northward::buffer::DeviceBuffers;
/// Northward envelope protocol types (re-exported at crate root).
pub use northward::{
    envelope,
    extension::{ExtensionStore, ExtensionStoreExt},
    mapping,
    model::{
        AlarmData, AttributeData, ClientRpcResponse, Command, DeviceConnectedData,
        DeviceDisconnectedData, PointMeta, QueuePolicy, RpcRequest, ServerRpcResponse,
        TelemetryData, WritePoint, WritePointError, WritePointErrorKind, WritePointResponse,
        WritePointStatus,
    },
    probe::{discover_north_libraries_in_dir, probe_north_library, NorthwardProbeInfo},
    runtime_api::NorthwardRuntimeApi,
    supervised::{NorthwardHandle, SupervisedPlugin},
    types::{AlarmSeverity, DropPolicy, TargetType},
    EventReceiver, NorthwardData, NorthwardEvent, NorthwardInitContext, NorthwardPublisher, Plugin,
    PluginConfig, PluginFactory,
};
pub use retry::{build_exponential_backoff, RetryController, RetryDecision, RetryPolicy};
pub use southward::{
    codec::ValueCodec,
    model::{
        ActionModel, ChannelModel, ConnectionPolicy, DeviceModel, DriverMetrics, Parameter,
        PointModel, SouthwardInitContext,
    },
    probe::{probe_driver_library, DriverProbeInfo},
    supervised::{SouthwardHandle, SupervisedDriver},
    transport::{
        bind_udp_metered, bind_udp_metered_with_timeout, connect_serial_metered,
        connect_tcp_metered, connect_tcp_metered_with_timeout, MeteredStream, MeteredUdpSocket,
        NoopSouthwardTransportMeter, SerialConnectConfig, SouthwardTransportMeter,
    },
    types::{AccessMode, CollectionType, DataPointType, DataType, DeviceState, ReportType, Status},
    validation::{
        downcast_parameters, resolve_action_inputs_typed, validate_action_parameters,
        validate_and_resolve_action_inputs,
    },
    wire::{WireDecode, WireEncode},
    CollectItem, CollectionGroupKey, Driver, DriverConfig, DriverFactory, ExecuteOutcome,
    ExecuteResult, RuntimeAction, RuntimeChannel, RuntimeDelta, RuntimeDevice, RuntimeParameter,
    RuntimePoint, WriteOutcome, WriteResult,
};
pub use supervision::{
    ConnectionState, FailureKind, FailurePhase, FailureReport, HandleCell, Phase,
    RetryBudgetSnapshot,
};
pub use transform::Transform;
pub use ui_schema::{
    DriverEntityTemplate, DriverSchemas, EnumItem, Field, FieldError, FlattenColumn, FlattenEntity,
    FromValidatedRow, Group, ImportValidationPreview, Node, Operator, PluginConfigSchemas,
    RowMappingContext, RuleValue, Rules, TemplateMetadata, UiDataType, UiProps, UiText, Union,
    UnionCase, ValidatedRow, ValidationCode, ValidationSummary, When, WhenEffect,
};
pub use value::{
    BinaryJsonEncoding, NGValue, NGValueCastError, NGValueJsonOptions, PointValue,
    TimestampJsonEncoding,
};

/// Public SDK constants for loader/driver macros to reference.
///
/// These constants are embedded via build.rs and used to form export symbols
/// for dynamic driver loading gates and routing.
pub mod sdk {
    /// Raw API version as string (from build.rs). Use `sdk_api_version()` to parse.
    pub const SDK_API_VERSION_STR: &str = env!("NG_SDK_API_VERSION");
    /// SDK SemVer string (e.g., 0.1.0)
    pub const SDK_VERSION: &str = env!("NG_SDK_VERSION");

    /// Parse the API version into u32 with safe fallback.
    pub fn sdk_api_version() -> u32 {
        SDK_API_VERSION_STR.parse::<u32>().unwrap_or(1)
    }
}
