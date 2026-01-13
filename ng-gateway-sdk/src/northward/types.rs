use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

/// Alarm severity levels
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AlarmSeverity {
    Critical,
    Major,
    Minor,
    Warning,
    Info,
}

/// Command type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TargetType {
    Gateway,
    SubDevice,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum DropPolicy {
    Discard = 0,
    Block = 1,
}

/// Northward connection state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NorthwardConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    /// Failed with a human-readable reason string
    Failed(String),
}

impl serde::Serialize for NorthwardConnectionState {
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

impl<'de> serde::Deserialize<'de> for NorthwardConnectionState {
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

impl Display for NorthwardConnectionState {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            NorthwardConnectionState::Failed(reason) => write!(f, "Failed({})", reason),
            other => write!(f, "{:?}", other),
        }
    }
}
