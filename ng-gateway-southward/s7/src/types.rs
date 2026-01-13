use crate::protocol::frame::CpuType;

use super::protocol::frame::addr::S7Address;
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter, RuntimePoint,
    Status,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TsapConfig {
    RackSlot { rack: u8, slot: u8 },
    Tsap { src: u16, dst: u16 },
}

/// S7 Channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7Channel {
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
    pub config: S7ChannelConfig,
}
/// S7 channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7ChannelConfig {
    /// CPU type
    pub cpu: CpuType,
    /// Remote host
    pub host: String,
    /// Remote port (default 102)
    #[serde(default = "S7ChannelConfig::default_port")]
    pub port: u16,
    /// TSAP configuration
    pub tsap: TsapConfig,
    /// Preferred S7 PDU size in bytes; None lets PLC decide
    pub preferred_pdu_size: Option<u16>,
    /// Preferred Max AmQ Caller-Ack (big-endian on wire), None lets PLC decide
    pub preferred_amq_caller: Option<u16>,
    /// Preferred Max AmQ Callee-Ack (big-endian on wire), None lets PLC decide
    pub preferred_amq_callee: Option<u16>,
}

impl S7ChannelConfig {
    fn default_port() -> u16 {
        102
    }
}

impl DriverConfig for S7ChannelConfig {}

impl RuntimeChannel for S7Channel {
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

/// S7 runtime device
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7Device {
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

impl RuntimeDevice for S7Device {
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

/// S7 runtime point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7Point {
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
    /// Address expression like "DB1.DBD0" or "I0.0"
    pub address: S7Address,
}

impl RuntimePoint for S7Point {
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

/// S7 runtime parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7Parameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    /// Address expression if needed for write
    pub address: S7Address,
}

impl RuntimeParameter for S7Parameter {
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

/// S7 runtime action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct S7Action {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<S7Parameter>,
}

impl RuntimeAction for S7Action {
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
