use super::{
    types::{
        AccessMode, CollectionType, DataPointType, DataType, HealthStatus, ReportType, Status,
    },
    RuntimeChannel, RuntimeDevice, RuntimePoint,
};
use crate::{NorthwardPublisher, RetryPolicy};
use chrono::{DateTime, Utc};
use sea_orm::FromJsonQueryResult;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc, time::Duration};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelModel {
    pub id: i32,
    /// Driver ID
    pub driver_id: i32,
    /// Name
    pub name: String,
    /// Collection type
    pub collection_type: CollectionType,
    /// Period
    pub period: Option<u32>,
    /// Report type
    pub report_type: ReportType,
    /// Status
    pub status: Status,
    /// Connection policy
    pub connection_policy: ConnectionPolicy,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceModel {
    pub id: i32,
    /// Channel ID
    pub channel_id: i32,
    /// Device Name
    pub device_name: String,
    /// Device Type
    pub device_type: String,
    /// Enabled
    pub status: Status,
    /// Driver configuration
    pub driver_config: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointModel {
    pub id: i32,
    /// Device ID
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Type
    pub r#type: DataPointType,
    /// Data Type
    pub data_type: DataType,
    /// Access Mode
    pub access_mode: AccessMode,
    /// Unit
    pub unit: Option<String>,
    /// Min Value
    pub min_value: Option<f64>,
    /// Max Value
    pub max_value: Option<f64>,
    /// Scale
    pub scale: Option<f64>,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionModel {
    pub id: i32,
    /// Device ID
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Command
    pub command: String,
    /// Inputs
    pub inputs: Vec<Parameter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Data type
    pub data_type: DataType,
    /// Required
    pub required: bool,
    /// Default value
    pub default_value: Option<serde_json::Value>,
    /// Max value
    pub max_value: Option<f64>,
    /// Min value
    pub min_value: Option<f64>,
    /// Driver configuration
    pub driver_config: serde_json::Value,
}

/// Runtime init context for a southbound driver
///
/// Consolidated view of channel topology plus host-injected capabilities for driver initialization.
/// @author saiki
#[derive(Debug, Clone)]
pub struct SouthwardInitContext {
    /// All devices under this channel
    pub devices: Vec<Arc<dyn RuntimeDevice>>,
    /// Points grouped by device id
    pub points_by_device: HashMap<i32, Vec<Arc<dyn RuntimePoint>>>,
    /// Runtime channel configuration and policies
    pub runtime_channel: Arc<dyn RuntimeChannel>,
    /// Northbound publisher injected by the host
    pub publisher: Arc<dyn NorthwardPublisher>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverHealth {
    pub status: HealthStatus,
    pub last_activity: DateTime<Utc>,
    pub error_count: u64,
    pub success_rate: f64,
    pub average_response_time: Duration,
    pub details: Option<HashMap<String, serde_json::Value>>,
}

/// Driver metrics
#[derive(Debug, Clone, Default)]
pub struct DriverMetrics {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub average_response_time: Duration,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, FromJsonQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionPolicy {
    #[serde(default = "ConnectionPolicy::default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(default = "ConnectionPolicy::default_read_timeout_ms")]
    pub read_timeout_ms: u64,
    #[serde(default = "ConnectionPolicy::default_write_timeout_ms")]
    pub write_timeout_ms: u64,
    #[serde(default)]
    pub backoff: RetryPolicy,
}

impl ConnectionPolicy {
    fn default_connect_timeout_ms() -> u64 {
        10000
    }
    fn default_read_timeout_ms() -> u64 {
        10000
    }
    fn default_write_timeout_ms() -> u64 {
        10000
    }
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            connect_timeout_ms: ConnectionPolicy::default_connect_timeout_ms(),
            read_timeout_ms: ConnectionPolicy::default_read_timeout_ms(),
            write_timeout_ms: ConnectionPolicy::default_write_timeout_ms(),
            backoff: RetryPolicy::default(),
        }
    }
}

impl sea_orm::IntoActiveValue<ConnectionPolicy> for ConnectionPolicy {
    fn into_active_value(self) -> sea_orm::ActiveValue<ConnectionPolicy> {
        sea_orm::ActiveValue::Set(self)
    }
}
