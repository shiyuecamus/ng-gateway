use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter, RuntimePoint,
    Status,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// IEC 60870-5-104 channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iec104Channel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub report_type: ReportType,
    pub period: Option<u32>,
    pub status: Status,
    /// Connection policy
    pub connection_policy: ConnectionPolicy,
    /// Channel configuration
    pub config: Iec104ChannelConfig,
}

/// IEC 60870-5-104 channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iec104ChannelConfig {
    /// Remote host
    pub host: String,
    /// Remote port, default 2404
    #[serde(default = "Iec104ChannelConfig::default_port")]
    pub port: u16,
    /// t0 confirm timeout (ms)
    #[serde(default = "Iec104ChannelConfig::default_t0_ms")]
    pub t0_ms: u32,
    /// t1 i-frame ack timeout (ms)
    #[serde(default = "Iec104ChannelConfig::default_t1_ms")]
    pub t1_ms: u32,
    /// t2 s-ack aggregation timeout (ms)
    #[serde(default = "Iec104ChannelConfig::default_t2_ms")]
    pub t2_ms: u32,
    /// t3 idle test frame interval (ms)
    #[serde(default = "Iec104ChannelConfig::default_t3_ms")]
    pub t3_ms: u32,
    /// k window (max unacked i-frames)
    #[serde(default = "Iec104ChannelConfig::default_k_window")]
    pub k_window: u16,
    /// w threshold (ack aggregation)
    #[serde(default = "Iec104ChannelConfig::default_w_threshold")]
    pub w_threshold: u16,
    /// Application send queue capacity
    #[serde(default = "Iec104ChannelConfig::default_send_queue_capacity")]
    pub send_queue_capacity: usize,
    /// TCP_NODELAY flag
    #[serde(default = "Iec104ChannelConfig::default_true")]
    pub tcp_nodelay: bool,
    /// Max budget of pending ASDU bytes (hi backlog + inflight)
    #[serde(default = "Iec104ChannelConfig::default_max_pending_asdu_bytes")]
    pub max_pending_asdu_bytes: usize,
    /// Whether to drop low-priority when memory window full
    #[serde(default = "Iec104ChannelConfig::default_true")]
    pub discard_low_priority_when_window_full: bool,
    /// Merge low-priority ASDUs to last-seen one
    #[serde(default = "Iec104ChannelConfig::default_true")]
    pub merge_low_priority: bool,
    /// Low priority flushing max age (ms)
    #[serde(default = "Iec104ChannelConfig::default_low_prio_flush_max_age_ms")]
    pub low_prio_flush_max_age_ms: u64,
    /// Whether to auto send Counter Interrogation on Active
    #[serde(default = "Iec104ChannelConfig::default_true")]
    pub auto_startup_counter_interrogation: bool,
    /// Whether to auto send General Interrogation on Active
    #[serde(default = "Iec104ChannelConfig::default_true")]
    pub auto_startup_general_interrogation: bool,
    /// Startup General Interrogation QOI (default 20)
    #[serde(default = "Iec104ChannelConfig::default_startup_qoi")]
    pub startup_qoi: u8,
    /// Startup Counter Interrogation QCC (default 5)
    #[serde(default = "Iec104ChannelConfig::default_startup_qcc")]
    pub startup_qcc: u8,
}

impl Iec104ChannelConfig {
    fn default_true() -> bool {
        true
    }

    fn default_port() -> u16 {
        2404
    }

    fn default_t0_ms() -> u32 {
        10_000
    }

    fn default_t1_ms() -> u32 {
        15_000
    }

    fn default_t2_ms() -> u32 {
        10_000
    }

    fn default_t3_ms() -> u32 {
        20_000
    }

    fn default_k_window() -> u16 {
        12
    }

    fn default_w_threshold() -> u16 {
        8
    }

    fn default_send_queue_capacity() -> usize {
        1024
    }

    fn default_max_pending_asdu_bytes() -> usize {
        1024 * 1024
    }

    fn default_low_prio_flush_max_age_ms() -> u64 {
        2000
    }

    fn default_startup_qoi() -> u8 {
        20
    }

    fn default_startup_qcc() -> u8 {
        5
    }
}

impl DriverConfig for Iec104ChannelConfig {}

impl RuntimeChannel for Iec104Channel {
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

/// IEC104 runtime device (per common address)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Iec104Device {
    pub id: i32,
    /// Channel ID
    pub channel_id: i32,
    /// Device Name
    pub device_name: String,
    /// Device Type
    pub device_type: String,
    /// Status
    pub status: Status,
    /// Common address (CA)
    pub ca: u16,
}

impl RuntimeDevice for Iec104Device {
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

/// IEC104 runtime point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iec104Point {
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
    /// Information object address (IOA)
    pub ioa: u16,
    /// ASDU Type ID as raw u8 (mapping handled in driver)
    pub type_id: u8,
}

impl RuntimePoint for Iec104Point {
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

/// IEC104 runtime action (placeholder for future commands)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iec104Action {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    /// Optional parameters as opaque JSON for future extension
    pub input_parameters: Vec<Iec104Parameter>,
}

impl RuntimeAction for Iec104Action {
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

/// IEC104 parameter (opaque, mainly for future write commands)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Iec104Parameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    pub ioa: u16,
    pub type_id: u8,
}

impl RuntimeParameter for Iec104Parameter {
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
