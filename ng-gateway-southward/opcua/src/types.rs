use super::codec::OpcUaCodec;
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    DriverError, NGValue, ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimeParameter, RuntimePoint, Status,
};
use opcua::{
    client::{IssuedTokenWrapper, Password},
    crypto::{PrivateKey, X509},
    types::{ByteString, NodeId, StatusCode, Variant},
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::{collections::HashMap, sync::Arc};

/// OPC UA channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaChannel {
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
    pub config: OpcUaChannelConfig,
}

/// OPC UA channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaChannelConfig {
    /// Application name
    pub application_name: String,
    /// Application URI
    pub application_uri: String,
    /// OPC UA server endpoint URL
    pub url: String,
    /// Authentication configuration
    pub auth: OpcUaAuth,
    /// Security policy
    pub security_policy: SecurityPolicy,
    /// Security mode
    pub security_mode: SecurityMode,
    /// Data reading mode
    pub read_mode: OpcUaReadMode,
    /// Session timeout
    #[serde(default = "OpcUaChannelConfig::default_session_timeout_ms")]
    pub session_timeout: u32,
    /// Keep alive interval
    #[serde(default = "OpcUaChannelConfig::default_keep_alive_interval_ms")]
    pub keep_alive_interval: u32,
    /// Maximum number of failed keep alives before the client will be closed.
    #[serde(default = "OpcUaChannelConfig::default_max_failed_keep_alive_count")]
    pub max_failed_keep_alive_count: u32,
    /// Batch size for create/modify/delete monitored items
    #[serde(default = "OpcUaChannelConfig::default_subscribe_batch_size")]
    pub subscribe_batch_size: usize,
}

impl OpcUaChannelConfig {
    fn default_session_timeout_ms() -> u32 {
        30000
    }

    fn default_keep_alive_interval_ms() -> u32 {
        30000
    }

    fn default_max_failed_keep_alive_count() -> u32 {
        3
    }

    fn default_subscribe_batch_size() -> usize {
        256
    }
}

impl DriverConfig for OpcUaChannelConfig {}

impl RuntimeChannel for OpcUaChannel {
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

/// OpcUa-specific device
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaDevice {
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

impl RuntimeDevice for OpcUaDevice {
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

/// OpcUa-specific point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaPoint {
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
    pub node_id: String,
}

impl RuntimePoint for OpcUaPoint {
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

/// OpcUa-specific parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaParameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    pub node_id: String,
}

impl RuntimeParameter for OpcUaParameter {
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

/// OpcUa-specific action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpcUaAction {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<OpcUaParameter>,
}

impl RuntimeAction for OpcUaAction {
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

/// OPC UA authentication types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum OpcUaAuth {
    Anonymous,
    UserPassword {
        username: String,
        password: String,
    },
    IssuedToken {
        token: String,
    },
    Certificate {
        private_key: String,
        certificate: String,
    },
}

impl TryFrom<OpcUaAuth> for opcua::client::IdentityToken {
    type Error = DriverError;

    fn try_from(value: OpcUaAuth) -> Result<Self, Self::Error> {
        match value {
            OpcUaAuth::Anonymous => Ok(opcua::client::IdentityToken::Anonymous),
            OpcUaAuth::UserPassword { username, password } => Ok(
                opcua::client::IdentityToken::UserName(username, Password::new(password)),
            ),
            OpcUaAuth::IssuedToken { token } => Ok(opcua::client::IdentityToken::IssuedToken(
                IssuedTokenWrapper::new_source(
                    ByteString::from_base64(&token)
                        .ok_or(DriverError::ConfigurationError("Invalid token".to_string()))?,
                ),
            )),
            OpcUaAuth::Certificate {
                private_key,
                certificate,
            } => Ok(opcua::client::IdentityToken::X509(
                Box::new(
                    X509::from_pem(certificate.as_bytes())
                        .map_err(|e| DriverError::ConfigurationError(e.to_string()))?,
                ),
                Box::new(
                    PrivateKey::from_pem(private_key.as_bytes())
                        .map_err(|e| DriverError::ConfigurationError(e.to_string()))?,
                ),
            )),
        }
    }
}

/// OPC UA security policies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum SecurityPolicy {
    None = 0,
    Basic128Rsa15 = 1,
    Basic256 = 2,
    Basic256Sha256 = 3,
    Aes128Sha256RsaOaep = 4,
    Aes256Sha256RsaPss = 5,
    Unknown = 6,
}

impl From<SecurityPolicy> for opcua::crypto::SecurityPolicy {
    fn from(policy: SecurityPolicy) -> Self {
        match policy {
            SecurityPolicy::None => opcua::crypto::SecurityPolicy::None,
            SecurityPolicy::Basic128Rsa15 => opcua::crypto::SecurityPolicy::Basic128Rsa15,
            SecurityPolicy::Basic256 => opcua::crypto::SecurityPolicy::Basic256,
            SecurityPolicy::Basic256Sha256 => opcua::crypto::SecurityPolicy::Basic256Sha256,
            SecurityPolicy::Aes128Sha256RsaOaep => {
                opcua::crypto::SecurityPolicy::Aes128Sha256RsaOaep
            }
            SecurityPolicy::Aes256Sha256RsaPss => opcua::crypto::SecurityPolicy::Aes256Sha256RsaPss,
            SecurityPolicy::Unknown => opcua::crypto::SecurityPolicy::Unknown,
        }
    }
}

/// OPC UA security modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum SecurityMode {
    Invalid = 0,
    None = 1,
    Sign = 2,
    SignAndEncrypt = 3,
}

impl From<SecurityMode> for opcua::types::MessageSecurityMode {
    fn from(mode: SecurityMode) -> Self {
        match mode {
            SecurityMode::Invalid => opcua::types::MessageSecurityMode::Invalid,
            SecurityMode::None => opcua::types::MessageSecurityMode::None,
            SecurityMode::Sign => opcua::types::MessageSecurityMode::Sign,
            SecurityMode::SignAndEncrypt => opcua::types::MessageSecurityMode::SignAndEncrypt,
        }
    }
}

/// OPC UA data reading modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum OpcUaReadMode {
    /// Use subscriptions for real-time data
    Subscribe = 0,
    /// Periodic read requests
    Read = 1,
}

/// Metadata for a monitored point used in subscription callbacks.
#[derive(Debug, Clone)]
pub(super) struct PointMeta {
    /// Device id of the point
    pub device_id: i32,
    /// Full point definition for consistent behavior
    pub point: Arc<OpcUaPoint>,
}

impl PointMeta {
    #[inline]
    pub(super) fn kind(&self) -> DataPointType {
        self.point.r#type
    }

    #[inline]
    pub(super) fn coerce(&self, variant: &Variant) -> Option<NGValue> {
        OpcUaCodec::coerce_variant_value(variant, self.point.data_type, self.point.scale)
    }
}

#[derive(Debug, Clone)]
pub(super) struct DeviceMeta {
    pub name: String,
    pub status: Status,
}

/// Immutable snapshot for zero-lock publishing path
#[derive(Debug, Clone, Default)]
pub(super) struct PointSnapshot {
    pub node_to_meta: HashMap<NodeId, PointMeta>,
    pub point_id_to_node: HashMap<i32, NodeId>,
    pub device_to_nodes: HashMap<i32, Vec<NodeId>>,
    /// Device metadata mapping for dynamic lookup at publish time
    pub devices: HashMap<i32, DeviceMeta>,
}

/// Classification for OPC UA monitored item creation failures.
///
/// This type groups low-level `StatusCode` values into a small number of
/// meaningful categories that can be used consistently in logging and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MonitoredItemFailureKind {
    /// Capacity or resource related limits (too many items, server too busy, etc.).
    CapacityOrResources,
    /// Configuration problems (invalid/unknown NodeId, bad attribute, etc.).
    Configuration,
    /// Authentication / authorization / access control problems.
    PermissionOrAuth,
    /// Any other error code that does not fit the above categories.
    Other,
}

impl MonitoredItemFailureKind {
    /// Return a stable string label for use in structured logging and metrics.
    #[inline]
    pub(super) fn as_str(self) -> &'static str {
        match self {
            MonitoredItemFailureKind::CapacityOrResources => "capacity_or_resources",
            MonitoredItemFailureKind::Configuration => "configuration",
            MonitoredItemFailureKind::PermissionOrAuth => "permission_or_auth",
            MonitoredItemFailureKind::Other => "other",
        }
    }
}

impl From<StatusCode> for MonitoredItemFailureKind {
    #[inline]
    fn from(status: StatusCode) -> Self {
        if status == StatusCode::BadTooManyMonitoredItems
            || status == StatusCode::BadTooManyOperations
            || status == StatusCode::BadOutOfMemory
            || status == StatusCode::BadResourceUnavailable
            || status == StatusCode::BadServerTooBusy
        {
            MonitoredItemFailureKind::CapacityOrResources
        } else if status == StatusCode::BadNodeIdInvalid
            || status == StatusCode::BadNodeIdUnknown
            || status == StatusCode::BadAttributeIdInvalid
        {
            MonitoredItemFailureKind::Configuration
        } else if status == StatusCode::BadUserAccessDenied
            || status == StatusCode::BadIdentityTokenInvalid
            || status == StatusCode::BadIdentityTokenRejected
        {
            MonitoredItemFailureKind::PermissionOrAuth
        } else {
            MonitoredItemFailureKind::Other
        }
    }
}
