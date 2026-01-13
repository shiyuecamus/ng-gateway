pub use crate::protocol::frame::defs::{Cjt188Address, MeterType};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    DriverError, DriverResult, ReportType, RuntimeChannel, RuntimeDevice, RuntimePoint, Status,
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::fmt;

/// Protocol version of CJ/T 188.
///
/// Differentiates between the 2004 and 2018 standards which have different supported meter types
/// and data identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Cjt188Version {
    #[default]
    V2004,
    V2018,
}

impl fmt::Display for Cjt188Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::V2004 => write!(f, "2004"),
            Self::V2018 => write!(f, "2018"),
        }
    }
}

// --- Configuration Structures ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr, Default)]
#[repr(u8)]
pub enum SerialDataBits {
    Five = 5,
    Six = 6,
    Seven = 7,
    #[default]
    Eight = 8,
}

impl From<SerialDataBits> for tokio_serial::DataBits {
    fn from(bits: SerialDataBits) -> Self {
        match bits {
            SerialDataBits::Five => tokio_serial::DataBits::Five,
            SerialDataBits::Six => tokio_serial::DataBits::Six,
            SerialDataBits::Seven => tokio_serial::DataBits::Seven,
            SerialDataBits::Eight => tokio_serial::DataBits::Eight,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr, Default)]
#[repr(u8)]
pub enum SerialStopBits {
    #[default]
    One = 1,
    Two = 2,
}

impl From<SerialStopBits> for tokio_serial::StopBits {
    fn from(bits: SerialStopBits) -> Self {
        match bits {
            SerialStopBits::One => tokio_serial::StopBits::One,
            SerialStopBits::Two => tokio_serial::StopBits::Two,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SerialParity {
    #[default]
    None,
    Odd,
    Even,
}

impl From<SerialParity> for tokio_serial::Parity {
    fn from(parity: SerialParity) -> Self {
        match parity {
            SerialParity::None => tokio_serial::Parity::None,
            SerialParity::Odd => tokio_serial::Parity::Odd,
            SerialParity::Even => tokio_serial::Parity::Even,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Cjt188Connection {
    Serial {
        port: String,
        baud_rate: u32,
        #[serde(default)]
        data_bits: SerialDataBits,
        #[serde(default)]
        stop_bits: SerialStopBits,
        #[serde(default)]
        parity: SerialParity,
    },
    Tcp {
        host: String,
        port: u16,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cjt188ChannelConfig {
    #[serde(default)]
    pub version: Cjt188Version,
    /// Underlying connection configuration (serial or TCP).
    pub connection: Cjt188Connection,
    /// Maximum number of consecutive request timeouts before forcing a reconnect.
    #[serde(default = "Cjt188ChannelConfig::default_max_timeouts")]
    pub max_timeouts: u32,
    /// Preamble bytes to wake up devices before a real CJ/T 188 frame.
    ///
    /// Some devices require a sequence of 0xFE bytes to wake up.
    #[serde(default = "Cjt188ChannelConfig::default_wakeup_preamble")]
    pub wakeup_preamble: Vec<u8>,
}

impl Cjt188ChannelConfig {
    fn default_max_timeouts() -> u32 {
        3
    }

    fn default_wakeup_preamble() -> Vec<u8> {
        vec![0xFE, 0xFE, 0xFE, 0xFE]
    }
}

impl DriverConfig for Cjt188ChannelConfig {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cjt188Channel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub report_type: ReportType,
    pub period: Option<u32>,
    pub status: Status,
    pub connection_policy: ConnectionPolicy,
    pub config: Cjt188ChannelConfig,
}

impl RuntimeChannel for Cjt188Channel {
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

// --- Device ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cjt188Device {
    pub id: i32,
    pub channel_id: i32,
    pub device_name: String,
    pub device_type: String,
    pub status: Status,
    /// CJ/T 188 meter type (T field).
    ///
    /// This value is encoded into each request frame and can also be used
    /// to interpret certain payload units.
    pub meter_type: MeterType,
    /// CJ/T 188 address field (A0..A6).
    ///
    /// Format: **14-hex** string representing `A6..A0` bytes (high address first, user-friendly).
    /// The protocol wire format is still `A0..A6` (low byte first) as required by the standard.
    pub address: String,
}

impl Cjt188Device {
    pub fn address_struct(&self) -> DriverResult<Cjt188Address> {
        Cjt188Address::parse_str(&self.address).map_err(|e| {
            DriverError::ConfigurationError(format!(
                "Invalid CJ/T 188 address '{}': {}",
                self.address, e
            ))
        })
    }
}

impl RuntimeDevice for Cjt188Device {
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

// --- Point ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cjt188Point {
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

    /// Data Identifier (DI) to read for this point
    ///
    /// The 16-bit DI value (e.g., 0x901F for full meter reading)
    pub di: u16,

    /// Field key within the DI response (REQUIRED)
    ///
    /// Specifies which field to extract from the DI response.
    /// Must match a field key defined in the DI's response schema.
    /// This is a MANDATORY field - no default or fallback behavior.
    ///
    /// # Examples
    /// - di=0x901F, field_key="current_flow"
    ///   -> Extract only "current_flow" value from 901F response
    ///
    /// # Common field keys
    /// - "di": Data Identifier (always present)
    /// - "ser": Serial number (always present, 1 byte)
    /// - "current_flow": Current cumulative flow
    /// - "settlement_flow": Settlement date flow
    /// - "datetime": Real-time timestamp (7 bytes BCD)
    /// - "status": Status bytes (2 bytes with bit flags)
    ///
    /// # Error Handling
    /// If the specified field_key does not exist in the schema:
    /// - A warning will be logged with available field keys
    /// - The point will return NGValue::Null with Quality::Bad
    pub field_key: String,
}

impl RuntimePoint for Cjt188Point {
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
