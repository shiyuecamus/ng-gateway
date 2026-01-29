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
