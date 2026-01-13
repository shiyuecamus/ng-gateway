mod bin_inspect;
mod error;
pub mod mqtt;
pub mod northward;
mod retry;
mod southward;
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

pub type DriverResult<T> = Result<T, DriverError>;
pub type NorthwardResult<T> = Result<T, NorthwardError>;

pub use bin_inspect::{
    ensure_current_platform_from_bytes, ensure_current_platform_from_path, inspect_binary,
    BinaryArch, BinaryInfo, BinaryOsType,
};
pub use error::{DriverError, NorthwardError};
/// Northward envelope protocol types (re-exported at crate root).
pub use northward::{
    envelope,
    extension::{ExtensionManager, ExtensionManagerExt},
    loader::{
        discover_north_libraries_in_dir, probe_north_library, NorthwardLoader, NorthwardProbeInfo,
        NorthwardRegistry,
    },
    mapping,
    model::{
        AlarmData, AttributeData, ClientRpcResponse, Command, DeviceConnectedData,
        DeviceDisconnectedData, PointMeta, QueuePolicy, RpcRequest, ServerRpcResponse,
        TelemetryData, WritePoint, WritePointError, WritePointErrorKind, WritePointResponse,
        WritePointStatus,
    },
    runtime_api::NorthwardRuntimeApi,
    types::{AlarmSeverity, DropPolicy, NorthwardConnectionState, TargetType},
    EventReceiver, NorthwardData, NorthwardEvent, NorthwardInitContext, NorthwardPublisher, Plugin,
    PluginConfig, PluginFactory,
};
pub use retry::{build_exponential_backoff, RetryPolicy};
pub use southward::{
    codec::ValueCodec,
    loader::{probe_driver_library, DriverLoader, DriverProbeInfo, DriverRegistry},
    model::{
        ActionModel, ChannelModel, ConnectionPolicy, DeviceModel, DriverHealth, DriverMetrics,
        Parameter, PointModel, SouthwardInitContext,
    },
    types::{
        AccessMode, CollectionType, DataPointType, DataType, DeviceState, HealthStatus, ReportType,
        SouthwardConnectionState, Status,
    },
    validation::{
        downcast_parameters, resolve_action_inputs_typed, validate_action_parameters,
        validate_and_resolve_action_inputs,
    },
    wire::{WireDecode, WireEncode},
    Driver, DriverConfig, DriverFactory, ExecuteOutcome, ExecuteResult, RuntimeAction,
    RuntimeChannel, RuntimeDelta, RuntimeDevice, RuntimeParameter, RuntimePoint, WriteOutcome,
    WriteResult,
};
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
