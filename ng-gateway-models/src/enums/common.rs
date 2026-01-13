use ng_gateway_error::NGError;
use ng_gateway_macros::IntoActiveValue;
use ng_gateway_sdk::{BinaryArch, BinaryOsType};
use once_cell::sync::Lazy;
use sea_orm::{ActiveValue, DeriveActiveEnum, EnumIter, IntoActiveValue};
use sea_query::StringLen;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::{
    collections::HashMap,
    fmt::{Display, Error, Formatter},
};

#[derive(Debug, Copy, Clone, PartialEq, Eq, EnumIter)]
pub enum ToFromType {
    To,
    From,
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize_repr,
    Deserialize_repr,
    IntoActiveValue,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum Status {
    Enabled = 0,
    Disabled = 1,
}

impl From<Status> for ng_gateway_sdk::Status {
    fn from(value: Status) -> Self {
        match value {
            Status::Enabled => ng_gateway_sdk::Status::Enabled,
            Status::Disabled => ng_gateway_sdk::Status::Disabled,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    EnumIter,
    DeriveActiveEnum,
    Serialize,
    Deserialize,
    IntoActiveValue,
)]
#[sea_orm(
    rs_type = "String",
    db_type = "String(StringLen::N(20))",
    rename_all = "SCREAMING_SNAKE_CASE"
)]
pub enum EntityType {
    User,
    Role,
    Menu,
    Driver,
    Plugin,
    App,
    Channel,
    Device,
    Point,
    Action,
}

impl EntityType {
    /// Returns the string representation of the entity type
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "USER",
            Self::Role => "ROLE",
            Self::Menu => "MENU",
            Self::Driver => "DRIVER",
            Self::Plugin => "NORTHWARD_PLUGIN",
            Self::App => "NORTHWARD_APP",
            Self::Channel => "CHANNEL",
            Self::Device => "DEVICE",
            Self::Point => "POINT",
            Self::Action => "ACTION",
        }
    }

    /// Returns the display name of the entity type
    #[inline]
    pub fn name(&self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Role => "Role",
            Self::Menu => "Menu",
            Self::Driver => "Driver",
            Self::Plugin => "NorthwardPlugin",
            Self::App => "NorthwardApp",
            Self::Channel => "Channel",
            Self::Device => "Device",
            Self::Point => "Point",
            Self::Action => "Action",
        }
    }

    /// Returns all operations supported by this entity type
    #[inline]
    pub fn operations(&self) -> Vec<Operation> {
        RESOURCE_OPERATIONS.get(self).cloned().unwrap_or_default()
    }

    /// Returns all entity types
    #[inline]
    pub fn all() -> Vec<EntityType> {
        vec![
            Self::User,
            Self::Role,
            Self::Menu,
            Self::Driver,
            Self::Plugin,
            Self::App,
            Self::Channel,
            Self::Device,
            Self::Point,
            Self::Action,
        ]
    }
}

impl Display for EntityType {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        write!(f, "{}", self.as_str())
    }
}

/// Operation enum representing different actions that can be performed on resources
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operation {
    /// Create a resource
    Create,
    /// Read a resource
    Read,
    /// Write/Update a resource
    Write,
    /// Delete a resource
    Delete,
    /// Assign a resource
    Assign,
    /// Do action for a device
    DoAction,
    /// Read point from a device
    ReadPoint,
    /// Write point to a device
    WritePoint,
    /// Reset password
    ResetPassword,
}

impl Operation {
    /// Returns the string representation of the operation
    #[inline]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Read => "READ",
            Self::Write => "WRITE",
            Self::Delete => "DELETE",
            Self::Assign => "ASSIGN",
            Self::DoAction => "DO_ACTION",
            Self::ReadPoint => "READ_POINT",
            Self::WritePoint => "WRITE_POINT",
            Self::ResetPassword => "RESET_PASSWORD",
        }
    }
}

impl Display for Operation {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        write!(f, "{}", self.as_str())
    }
}

/// Global resource operations map defining which operations are allowed on each resource type
pub static RESOURCE_OPERATIONS: Lazy<HashMap<EntityType, Vec<Operation>>> = Lazy::new(|| {
    let mut map = HashMap::new();

    // User operations
    map.insert(
        EntityType::User,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
            Operation::ResetPassword,
        ],
    );

    // Role operations
    map.insert(
        EntityType::Role,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
            Operation::Assign,
        ],
    );

    // Northward plugin operations
    map.insert(
        EntityType::Plugin,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
        ],
    );

    // Northward operations
    map.insert(
        EntityType::App,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
        ],
    );

    // Channel operations
    map.insert(
        EntityType::Channel,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
        ],
    );

    // Menu operations
    map.insert(
        EntityType::Menu,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
            Operation::Assign,
        ],
    );

    // Point operations
    map.insert(
        EntityType::Point,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
        ],
    );

    // Action operations
    map.insert(
        EntityType::Action,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
            Operation::DoAction,
        ],
    );

    // Device operations
    map.insert(
        EntityType::Device,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
            Operation::DoAction,
            Operation::Assign,
            Operation::ReadPoint,
            Operation::WritePoint,
        ],
    );

    // Driver operations
    map.insert(
        EntityType::Driver,
        vec![
            Operation::Create,
            Operation::Read,
            Operation::Write,
            Operation::Delete,
        ],
    );

    map
});

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum CollectionType {
    /// Device actively reports data to gateway
    Report = 0,
    /// Gateway actively collects data from device
    Collection = 1,
}

impl IntoActiveValue<CollectionType> for CollectionType {
    fn into_active_value(self) -> ActiveValue<CollectionType> {
        ActiveValue::Set(self)
    }
}

impl From<CollectionType> for ng_gateway_sdk::CollectionType {
    fn from(value: CollectionType) -> Self {
        match value {
            CollectionType::Report => ng_gateway_sdk::CollectionType::Report,
            CollectionType::Collection => ng_gateway_sdk::CollectionType::Collection,
        }
    }
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum ReportType {
    /// Report data only when it changes
    Change = 0,
    /// Always report data
    Always = 1,
}

impl IntoActiveValue<ReportType> for ReportType {
    fn into_active_value(self) -> ActiveValue<ReportType> {
        ActiveValue::Set(self)
    }
}

impl From<ReportType> for ng_gateway_sdk::ReportType {
    fn from(value: ReportType) -> Self {
        match value {
            ReportType::Change => ng_gateway_sdk::ReportType::Change,
            ReportType::Always => ng_gateway_sdk::ReportType::Always,
        }
    }
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
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

impl TryFrom<i16> for DataType {
    type Error = NGError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(DataType::Boolean),
            1 => Ok(DataType::Int8),
            2 => Ok(DataType::UInt8),
            3 => Ok(DataType::Int16),
            4 => Ok(DataType::UInt16),
            5 => Ok(DataType::Int32),
            6 => Ok(DataType::UInt32),
            7 => Ok(DataType::Int64),
            8 => Ok(DataType::UInt64),
            9 => Ok(DataType::Float32),
            10 => Ok(DataType::Float64),
            11 => Ok(DataType::String),
            12 => Ok(DataType::Binary),
            13 => Ok(DataType::Timestamp),
            _ => Err(NGError::Error(format!("Unsupported data type: {}", value))),
        }
    }
}

impl IntoActiveValue<DataType> for DataType {
    fn into_active_value(self) -> ActiveValue<DataType> {
        ActiveValue::Set(self)
    }
}

impl From<DataType> for ng_gateway_sdk::DataType {
    fn from(value: DataType) -> Self {
        match value {
            DataType::Boolean => ng_gateway_sdk::DataType::Boolean,
            DataType::Int8 => ng_gateway_sdk::DataType::Int8,
            DataType::UInt8 => ng_gateway_sdk::DataType::UInt8,
            DataType::Int16 => ng_gateway_sdk::DataType::Int16,
            DataType::UInt16 => ng_gateway_sdk::DataType::UInt16,
            DataType::Int32 => ng_gateway_sdk::DataType::Int32,
            DataType::UInt32 => ng_gateway_sdk::DataType::UInt32,
            DataType::Int64 => ng_gateway_sdk::DataType::Int64,
            DataType::UInt64 => ng_gateway_sdk::DataType::UInt64,
            DataType::Float32 => ng_gateway_sdk::DataType::Float32,
            DataType::Float64 => ng_gateway_sdk::DataType::Float64,
            DataType::String => ng_gateway_sdk::DataType::String,
            DataType::Binary => ng_gateway_sdk::DataType::Binary,
            DataType::Timestamp => ng_gateway_sdk::DataType::Timestamp,
        }
    }
}
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum AccessMode {
    Read = 0,
    Write = 1,
    ReadWrite = 2,
}

impl TryFrom<i16> for AccessMode {
    type Error = NGError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(AccessMode::Read),
            1 => Ok(AccessMode::Write),
            2 => Ok(AccessMode::ReadWrite),
            _ => Err(NGError::Error(format!("Unsupported access mode: {value}"))),
        }
    }
}
impl IntoActiveValue<AccessMode> for AccessMode {
    fn into_active_value(self) -> ActiveValue<AccessMode> {
        ActiveValue::Set(self)
    }
}

impl From<AccessMode> for ng_gateway_sdk::AccessMode {
    fn from(value: AccessMode) -> Self {
        match value {
            AccessMode::Read => ng_gateway_sdk::AccessMode::Read,
            AccessMode::Write => ng_gateway_sdk::AccessMode::Write,
            AccessMode::ReadWrite => ng_gateway_sdk::AccessMode::ReadWrite,
        }
    }
}
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize_repr, Deserialize_repr,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum DataPointType {
    Attribute = 0,
    Telemetry = 1,
}

impl TryFrom<i16> for DataPointType {
    type Error = NGError;

    fn try_from(value: i16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(DataPointType::Attribute),
            1 => Ok(DataPointType::Telemetry),
            _ => Err(NGError::Error(format!(
                "Unsupported data point type: {value}",
            ))),
        }
    }
}

impl IntoActiveValue<DataPointType> for DataPointType {
    fn into_active_value(self) -> ActiveValue<DataPointType> {
        ActiveValue::Set(self)
    }
}

impl From<DataPointType> for ng_gateway_sdk::DataPointType {
    fn from(value: DataPointType) -> Self {
        match value {
            DataPointType::Attribute => ng_gateway_sdk::DataPointType::Attribute,
            DataPointType::Telemetry => ng_gateway_sdk::DataPointType::Telemetry,
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize_repr,
    Deserialize_repr,
    IntoActiveValue,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum SourceType {
    BuiltIn = 0,
    Custom = 1,
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize_repr,
    Deserialize_repr,
    IntoActiveValue,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum OsType {
    Windows = 0,
    Linux = 1,
    Mac = 2,
    Unknown = 3,
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize_repr,
    Deserialize_repr,
    IntoActiveValue,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum OsArch {
    X86_64 = 0,
    Arm64 = 1,
    Arm = 2,
    Unknown = 3,
}

impl From<BinaryArch> for OsArch {
    fn from(value: BinaryArch) -> Self {
        match value {
            BinaryArch::X86_64 => OsArch::X86_64,
            BinaryArch::Arm64 => OsArch::Arm64,
            BinaryArch::Arm => OsArch::Arm,
            BinaryArch::Unknown => OsArch::Unknown,
        }
    }
}

impl From<BinaryOsType> for OsType {
    fn from(value: BinaryOsType) -> Self {
        match value {
            BinaryOsType::Windows => OsType::Windows,
            BinaryOsType::Mac => OsType::Mac,
            BinaryOsType::Linux => OsType::Linux,
            BinaryOsType::Unknown => OsType::Unknown,
        }
    }
}
