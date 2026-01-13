use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::fmt::{self, Display, Formatter};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Status {
    Enabled = 0,
    Disabled = 1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum CollectionType {
    /// Device actively reports data to gateway
    Report = 0,
    /// Gateway actively collects data from device
    Collection = 1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum ReportType {
    /// Report data only when it changes
    Change = 0,
    /// Always report data
    Always = 1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum DataType {
    Boolean = 0,
    Int8 = 1,
    UInt8 = 2,
    Int16 = 3,
    UInt16 = 4,
    Int32 = 5,
    UInt32 = 6,
    Int64 = 7,
    UInt64 = 8,
    Float32 = 9,
    Float64 = 10,
    String = 11,
    Binary = 12,
    Timestamp = 13,
}

impl DataType {
    #[inline]
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            DataType::Int8
                | DataType::UInt8
                | DataType::Int16
                | DataType::UInt16
                | DataType::Int32
                | DataType::UInt32
                | DataType::Int64
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
        )
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum AccessMode {
    Read = 0,
    Write = 1,
    ReadWrite = 2,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum DataPointType {
    Attribute = 0,
    Telemetry = 1,
}

/// Health status enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

/// Channel connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SouthwardConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    /// Failed with a human-readable reason string
    Failed(String),
}

impl serde::Serialize for SouthwardConnectionState {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Reconnecting => "Reconnecting",
            Self::Failed(_) => "Failed",
        };
        serializer.serialize_str(s)
    }
}

impl<'de> serde::Deserialize<'de> for SouthwardConnectionState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "Disconnected" => Ok(Self::Disconnected),
            "Connecting" => Ok(Self::Connecting),
            "Connected" => Ok(Self::Connected),
            "Reconnecting" => Ok(Self::Reconnecting),
            "Failed" => Ok(Self::Failed(String::new())),
            _ => Err(serde::de::Error::unknown_variant(
                &s,
                &[
                    "Disconnected",
                    "Connecting",
                    "Connected",
                    "Reconnecting",
                    "Failed",
                ],
            )),
        }
    }
}

impl Display for SouthwardConnectionState {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SouthwardConnectionState::Failed(reason) => write!(f, "Failed({})", reason),
            other => write!(f, "{:?}", other),
        }
    }
}

/// Device operational state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceState {
    Inactive,
    Active,
    Error,
    Maintenance,
}

impl DeviceState {
    /// Returns true if the device is considered operational/online.
    #[inline]
    pub fn is_active(&self) -> bool {
        matches!(self, DeviceState::Active)
    }
}
