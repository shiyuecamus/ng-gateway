use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter, RuntimePoint,
    Status,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Ethernet/IP channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpChannel {
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
    pub config: EthernetIpChannelConfig,
}

/// Ethernet/IP channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpChannelConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub slot: u8,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_port() -> u16 {
    44818
}

fn default_timeout() -> u64 {
    2000
}

impl DriverConfig for EthernetIpChannelConfig {}

impl RuntimeChannel for EthernetIpChannel {
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

/// Ethernet/IP device
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpDevice {
    pub id: i32,
    /// Channel ID
    pub channel_id: i32,
    /// Device Name
    pub device_name: String,
    /// Device Type
    pub device_type: String,
    /// Status
    pub status: Status,
}

impl RuntimeDevice for EthernetIpDevice {
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

/// Ethernet/IP point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpPoint {
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
    /// Tag Name (e.g., "Program:Main.MyTag")
    pub tag_name: String,
}

impl RuntimePoint for EthernetIpPoint {
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

/// Ethernet/IP action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpAction {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<EthernetIpParameter>,
}

impl RuntimeAction for EthernetIpAction {
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

/// Ethernet/IP parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthernetIpParameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    /// Tag Name to write to
    pub tag_name: String,
}

impl RuntimeParameter for EthernetIpParameter {
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
