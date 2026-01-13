use super::protocol::{frame::addr::McLogicalAddress, types::McSeries};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter, RuntimePoint,
    Status,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// MC channel configuration used by runtime channel and protocol layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McChannelConfig {
    /// PLC host or IP address.
    pub host: String,
    /// PLC TCP/UDP port (default 5001).
    #[serde(default = "McChannelConfig::default_port")]
    pub port: u16,
    /// PLC series (A/QnA/Q/L/IQ-R).
    pub series: McSeries,
    /// Maximum points per batch (optional, None means driver default).
    ///
    /// Defaults to 480 which matches the current driver batching strategy.
    #[serde(default = "McChannelConfig::default_max_points_per_batch")]
    pub max_points_per_batch: Option<u16>,
    /// Maximum payload bytes per frame (optional, None means driver default).
    ///
    /// Defaults to 4096 which matches the current driver frame size limit.
    #[serde(default = "McChannelConfig::default_max_bytes_per_frame")]
    pub max_bytes_per_frame: Option<u16>,
    /// Maximum concurrent in-flight requests per connection.
    ///
    /// NOTE: current MC implementation is safe with concurrent requests, but a
    /// default of 1 keeps behaviour deterministic. Higher values are reserved
    /// for future tuning and should be changed with care.
    #[serde(default = "McChannelConfig::default_concurrent_requests")]
    pub concurrent_requests: Option<u16>,
}

impl McChannelConfig {
    fn default_port() -> u16 {
        5001
    }

    fn default_concurrent_requests() -> Option<u16> {
        Some(1)
    }

    fn default_max_points_per_batch() -> Option<u16> {
        Some(480)
    }

    fn default_max_bytes_per_frame() -> Option<u16> {
        Some(4096)
    }
}

impl DriverConfig for McChannelConfig {}

/// Runtime MC channel, wrapping common channel attributes and protocol config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McChannel {
    /// Channel id.
    pub id: i32,
    /// Channel name.
    pub name: String,
    /// Driver id.
    pub driver_id: i32,
    /// Collection type (polling/event/etc).
    pub collection_type: CollectionType,
    /// Report type (telemetry/attribute).
    pub report_type: ReportType,
    /// Collection period in milliseconds.
    pub period: Option<u32>,
    /// Runtime status.
    pub status: Status,
    /// Connection policy shared with other southward drivers.
    pub connection_policy: ConnectionPolicy,
    /// MC specific protocol configuration.
    pub config: McChannelConfig,
}

impl RuntimeChannel for McChannel {
    fn id(&self) -> i32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn driver_id(&self) -> i32 {
        self.driver_id
    }

    fn collection_type(&self) -> CollectionType {
        self.collection_type
    }

    fn report_type(&self) -> ReportType {
        self.report_type
    }

    fn period(&self) -> Option<u32> {
        self.period
    }

    fn status(&self) -> Status {
        self.status
    }

    fn connection_policy(&self) -> &ConnectionPolicy {
        &self.connection_policy
    }

    fn config(&self) -> &dyn DriverConfig {
        &self.config
    }
}

/// Runtime MC device metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McDevice {
    /// Device id.
    pub id: i32,
    /// Channel id this device belongs to.
    pub channel_id: i32,
    /// Human readable device name.
    pub device_name: String,
    /// Device type string from model.
    pub device_type: String,
    /// Runtime status.
    pub status: Status,
}

impl RuntimeDevice for McDevice {
    fn id(&self) -> i32 {
        self.id
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn device_type(&self) -> &str {
        &self.device_type
    }

    fn channel_id(&self) -> i32 {
        self.channel_id
    }

    fn status(&self) -> Status {
        self.status
    }
}

/// Placeholder MC address type.
///
/// The full address model (device type, head, bit, points) is implemented
/// in the protocol/frame layer; this struct keeps a minimal representation
/// for now and will be extended when wiring protocol-level types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McAddress {
    /// Raw address string from configuration (e.g. \"D100\", \"D20.2\", \"X1A0\").
    pub raw: String,
    /// Resolved logical address used by protocol layer, populated at runtime.
    #[serde(skip)]
    pub logical: Option<McLogicalAddress>,
}

/// Runtime MC point.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McPoint {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub key: String,
    pub r#type: DataPointType,
    pub data_type: DataType,
    pub access_mode: AccessMode,
    pub unit: Option<String>,
    pub min_value: Option<f64>,
    pub max_value: Option<f64>,
    pub scale: Option<f64>,
    /// MC logical address.
    pub address: McAddress,
    /// Optional string length in bytes for string data points.
    pub string_len_bytes: Option<u16>,
}

impl RuntimePoint for McPoint {
    fn id(&self) -> i32 {
        self.id
    }

    fn device_id(&self) -> i32 {
        self.device_id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn key(&self) -> &str {
        &self.key
    }

    fn r#type(&self) -> DataPointType {
        self.r#type
    }

    fn data_type(&self) -> DataType {
        self.data_type
    }

    fn access_mode(&self) -> AccessMode {
        self.access_mode
    }

    fn unit(&self) -> Option<&str> {
        self.unit.as_deref()
    }

    fn min_value(&self) -> Option<f64> {
        self.min_value
    }

    fn max_value(&self) -> Option<f64> {
        self.max_value
    }

    fn scale(&self) -> Option<f64> {
        self.scale
    }
}

/// Runtime MC action input parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McParameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    /// MC logical address for this parameter when writing.
    pub address: McAddress,
    /// Optional string length in bytes for string parameters.
    ///
    /// When `data_type == DataType::String`, MC protocol requires a fixed read/write length
    /// (rounded up to word boundary). This field mirrors `McPoint.string_len_bytes` and is
    /// consumed by the driver to compute word length for typed writes.
    pub string_len_bytes: Option<u16>,
}

impl RuntimeParameter for McParameter {
    fn name(&self) -> &str {
        &self.name
    }

    fn key(&self) -> &str {
        &self.key
    }

    fn data_type(&self) -> DataType {
        self.data_type
    }

    fn required(&self) -> bool {
        self.required
    }

    fn default_value(&self) -> Option<serde_json::Value> {
        self.default_value.clone()
    }

    fn max_value(&self) -> Option<f64> {
        self.max_value
    }

    fn min_value(&self) -> Option<f64> {
        self.min_value
    }
}

/// Runtime MC action.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McAction {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<McParameter>,
}

impl RuntimeAction for McAction {
    fn id(&self) -> i32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn device_id(&self) -> i32 {
        self.device_id
    }

    fn command(&self) -> &str {
        &self.command
    }

    fn input_parameters(&self) -> Vec<Arc<dyn RuntimeParameter>> {
        self.input_parameters
            .iter()
            .map(|p| Arc::new(p.clone()) as Arc<dyn RuntimeParameter>)
            .collect()
    }
}
