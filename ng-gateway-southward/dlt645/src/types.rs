use crate::protocol::frame::{parse_di_str, Dl645Address};
use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    DriverError, DriverResult, ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice,
    RuntimeParameter, RuntimePoint, Status,
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::sync::Arc;

/// DL/T 645 protocol version enumeration.
///
/// This enum allows the driver to adapt behavior for different versions of the
/// DL/T 645 standard (e.g., control codes and DI semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Dl645Version {
    /// Legacy DL/T 645-1997.
    V1997 = 1997,
    /// Widely deployed DL/T 645-2007.
    V2007 = 2007,
    /// Latest DL/T 645-2021 (security extensions, not fully implemented yet).
    V2021 = 2021,
}

/// Serial data bits configuration for RS-485 links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum DataBits {
    Five = 5,
    Six = 6,
    Seven = 7,
    Eight = 8,
}

impl From<DataBits> for tokio_serial::DataBits {
    fn from(data_bits: DataBits) -> Self {
        match data_bits {
            DataBits::Five => tokio_serial::DataBits::Five,
            DataBits::Six => tokio_serial::DataBits::Six,
            DataBits::Seven => tokio_serial::DataBits::Seven,
            DataBits::Eight => tokio_serial::DataBits::Eight,
        }
    }
}

impl From<&DataBits> for tokio_serial::DataBits {
    fn from(data_bits: &DataBits) -> Self {
        match data_bits {
            DataBits::Five => tokio_serial::DataBits::Five,
            DataBits::Six => tokio_serial::DataBits::Six,
            DataBits::Seven => tokio_serial::DataBits::Seven,
            DataBits::Eight => tokio_serial::DataBits::Eight,
        }
    }
}

/// Serial stop bits configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum StopBits {
    One = 1,
    Two = 2,
}

impl From<StopBits> for tokio_serial::StopBits {
    fn from(stop_bits: StopBits) -> Self {
        match stop_bits {
            StopBits::One => tokio_serial::StopBits::One,
            StopBits::Two => tokio_serial::StopBits::Two,
        }
    }
}

impl From<&StopBits> for tokio_serial::StopBits {
    fn from(stop_bits: &StopBits) -> Self {
        match stop_bits {
            StopBits::One => tokio_serial::StopBits::One,
            StopBits::Two => tokio_serial::StopBits::Two,
        }
    }
}

/// Serial parity configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Dl645Parity {
    None = 0,
    Odd = 1,
    Even = 2,
}

impl From<Dl645Parity> for tokio_serial::Parity {
    fn from(parity: Dl645Parity) -> Self {
        match parity {
            Dl645Parity::None => tokio_serial::Parity::None,
            Dl645Parity::Odd => tokio_serial::Parity::Odd,
            Dl645Parity::Even => tokio_serial::Parity::Even,
        }
    }
}

impl From<&Dl645Parity> for tokio_serial::Parity {
    fn from(parity: &Dl645Parity) -> Self {
        match parity {
            Dl645Parity::None => tokio_serial::Parity::None,
            Dl645Parity::Odd => tokio_serial::Parity::Odd,
            Dl645Parity::Even => tokio_serial::Parity::Even,
        }
    }
}

/// DL/T 645 channel runtime model.
///
/// This struct represents a single DL/T 645 bus (typically one RS-485 line)
/// managed by the gateway. It contains logical settings plus protocol-specific
/// configuration and connection policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645Channel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub report_type: ReportType,
    pub period: Option<u32>,
    pub status: Status,
    /// Connection policy (timeouts and backoff settings).
    pub connection_policy: ConnectionPolicy,
    /// Protocol-specific channel configuration.
    pub config: Dl645ChannelConfig,
}

/// DL/T 645 connection configuration (serial or TCP).
///
/// This enum allows a single driver implementation to support both native
/// serial (RS-485) and TCP (e.g., serial device servers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Dl645Connection {
    /// Native serial connection over RS-485.
    Serial {
        /// Serial port path, e.g. `/dev/ttyS0` or `COM1`.
        port: String,
        /// Baud rate (e.g. 2400/4800/9600).
        baud_rate: u32,
        /// Data bits (normally 8).
        data_bits: DataBits,
        /// Stop bits (1 or 2).
        stop_bits: StopBits,
        /// Parity configuration.
        parity: Dl645Parity,
    },
    /// TCP connection to a device or serial server.
    Tcp {
        /// Remote host or IP address.
        host: String,
        /// Remote TCP port.
        port: u16,
    },
}

/// DL/T 645 channel configuration.
///
/// This struct holds physical link and protocol tuning parameters that are
/// deserialized from the channel's `driver_config` JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645ChannelConfig {
    /// Protocol version (1997/2007/2021).
    pub version: Dl645Version,
    /// Underlying connection configuration (serial or TCP).
    pub connection: Dl645Connection,
    /// Maximum number of consecutive request timeouts before forcing a reconnect.
    ///
    /// A single timeout is often caused by a temporarily busy or slow meter and
    /// should not immediately trigger a reconnect cycle. However, when a series
    /// of timeouts is observed without any successful responses in between, it
    /// is usually an indication that the underlying transport (TCP or serial)
    /// is no longer healthy. This threshold allows the supervisor to treat such
    /// patterns as a signal to tear down the session and re-establish it using
    /// the configured backoff policy.
    ///
    /// A value of zero disables timeout-based reconnects and only fatal
    /// transport errors (e.g. EOF, write failures) will trigger reconnect.
    #[serde(default = "Dl645ChannelConfig::default_max_timeouts")]
    pub max_timeouts: u32,
    /// Preamble bytes to wake up devices before a real DL/T 645 frame.
    ///
    /// Many meters expect four leading 0xFE bytes; this sequence is safe for
    /// most deployments because 0xFE is explicitly defined as a leading byte
    /// that should be ignored by protocol parsers until the first 0x68.
    /// Configure an empty array to disable preamble sending.
    #[serde(default = "Dl645ChannelConfig::default_wakeup_preamble")]
    pub wakeup_preamble: Vec<u8>,
}

impl Dl645ChannelConfig {
    /// Default maximum number of consecutive timeouts before reconnect.
    ///
    /// Three timeouts in a row is a good trade-off between resilience against
    /// transient meter delays and fast recovery from persistent connectivity
    /// issues on the bus.
    fn default_max_timeouts() -> u32 {
        3
    }

    /// Default preamble used to wake up DL/T 645 meters before sending frames.
    ///
    /// The standard practice is to send four 0xFE bytes; devices that do not
    /// require a preamble will safely ignore them.
    fn default_wakeup_preamble() -> Vec<u8> {
        vec![0xFE, 0xFE, 0xFE, 0xFE]
    }
}

impl DriverConfig for Dl645ChannelConfig {}

impl RuntimeChannel for Dl645Channel {
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

/// DL/T 645 device runtime model.
///
/// Each device corresponds to a single meter on the bus and is identified by a
/// 6-byte BCD address stored as a human-readable string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645Device {
    pub id: i32,
    /// Channel ID that owns this device.
    pub channel_id: i32,
    /// Human readable device name.
    pub device_name: String,
    /// Device type or model identifier.
    pub device_type: String,
    /// Device status flag.
    pub status: Status,
    /// 6-byte BCD address stored as string, for example "123456789012".
    pub address: Dl645Address,
    /// Password for protected operations (e.g. write, clear), usually 8 hex digits.
    pub password: String,
    /// Operator code for protected operations (e.g. write, clear), usually 8 hex digits.
    pub operator_code: Option<String>,
}

impl RuntimeDevice for Dl645Device {
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

impl Dl645Device {
    #[inline]
    pub fn security_params(&self) -> Result<(u32, Option<u32>), DriverError> {
        let password = Self::parse_security_param(&self.password, "password", &self.device_name)?;
        let operator_code = self
            .operator_code
            .as_ref()
            .map(|code| Self::parse_security_param(code, "operator code", &self.device_name))
            .transpose()?;
        Ok((password, operator_code))
    }

    #[inline]
    fn parse_security_param(s: &str, field: &str, device_name: &str) -> DriverResult<u32> {
        // If strictly 8 characters, prefer Hex interpretation (standard 4-byte width).
        let val = if s.len() == 8 {
            u32::from_str_radix(s, 16).or_else(|_| s.parse::<u32>())
        } else {
            s.parse::<u32>().or_else(|_| u32::from_str_radix(s, 16))
        };

        val.map_err(|_| {
            DriverError::ConfigurationError(format!(
                "Invalid {} format for device {}: {}",
                field, device_name, s
            ))
        })
    }
}

/// DL/T 645 point runtime model.
///
/// Each point is mapped to a DI (data identifier) of the meter. The DI is stored
/// as a numeric value (parsed once from configuration) and interpreted by the
/// driver when building frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645Point {
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
    /// 4-byte DI encoded as a numeric value parsed from hex, such as 0x00010000.
    pub di: u32,
    /// Optional number of decimal places for BCD values.
    pub decimals: Option<u8>,
}

impl RuntimePoint for Dl645Point {
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

/// DL/T 645 function codes used at configuration and action parameter level.
///
/// This enum mirrors the low 5-bit function field (D0..D4) of the DL/T 645
/// control word, but is intentionally kept free of the `Unknown` variant so
/// that it can be serialized and deserialized using `serde` in a stable way.
/// For on-wire parsing and handling of unknown patterns the protocol layer
/// continues to use the `Dl645Function` enum defined in `protocol::frame`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Dl645FunctionCode {
    /// 00000: Reserved.
    Reserved = 0b00000,
    /// 01000: Broadcast time synchronization.
    BroadcastTimeSync = 0b01000,
    /// 10001: Read data.
    ReadData = 0b10001,
    /// 10010: Read subsequent data.
    ReadNextData = 0b10010,
    /// 10011: Read communication address.
    ReadAddress = 0b10011,
    /// 10100: Write data.
    WriteData = 0b10100,
    /// 10101: Write communication address.
    WriteAddress = 0b10101,
    /// 10110: Freeze command.
    Freeze = 0b10110,
    /// 10111: Update baud rate.
    UpdateBaudRate = 0b10111,
    /// 11000: Modify password.
    ModifyPassword = 0b11000,
    /// 11001: Maximum demand clear.
    ClearMaxDemand = 0b11001,
    /// 11010: Meter clear.
    ClearMeter = 0b11010,
    /// 11011: Event clear.
    ClearEvents = 0b11011,
}

impl TryFrom<u8> for Dl645FunctionCode {
    type Error = DriverError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0b00000 => Ok(Dl645FunctionCode::Reserved),
            0b01000 => Ok(Dl645FunctionCode::BroadcastTimeSync),
            0b10001 => Ok(Dl645FunctionCode::ReadData),
            0b10010 => Ok(Dl645FunctionCode::ReadNextData),
            0b10011 => Ok(Dl645FunctionCode::ReadAddress),
            0b10100 => Ok(Dl645FunctionCode::WriteData),
            0b10101 => Ok(Dl645FunctionCode::WriteAddress),
            0b10110 => Ok(Dl645FunctionCode::Freeze),
            0b10111 => Ok(Dl645FunctionCode::UpdateBaudRate),
            0b11000 => Ok(Dl645FunctionCode::ModifyPassword),
            0b11001 => Ok(Dl645FunctionCode::ClearMaxDemand),
            0b11010 => Ok(Dl645FunctionCode::ClearMeter),
            0b11011 => Ok(Dl645FunctionCode::ClearEvents),
            _ => Err(DriverError::ConfigurationError(format!(
                "Invalid function code: {value}"
            ))),
        }
    }
}

/// DL/T 645 parameter model used for driver actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645Parameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    /// Optional number of decimal places for BCD-encoded numeric parameters.
    ///
    /// When present, this value controls how floating point and integer
    /// parameters are scaled during BCD encoding for DL/T 645 actions.
    /// If omitted, the codec falls back to a type-specific default.
    #[serde(default = "Dl645Parameter::default_decimals")]
    pub decimals: Option<u8>,
    /// DL/T 645 function code associated with this parameter.
    ///
    /// This value is deserialized directly from configuration using a
    /// numeric representation, similar to the `ModbusFunctionCode` used
    /// by the Modbus driver. It represents the low 5-bit function code
    /// encoded in the DL/T 645 control word.
    pub function_code: Dl645FunctionCode,
    /// Optional data identifier associated with this parameter.
    ///
    /// For classic "write data" style commands this field carries the DI
    /// (data identifier) that determines which register or energy value is
    /// addressed by the operation. For commands that do not rely on DI
    /// (such as broadcast time synchronization or clear meter commands),
    /// this field is left as `None`.
    pub di: Option<u32>,
}

impl Dl645Parameter {
    /// Default number of decimal places for BCD-encoded numeric parameters.
    fn default_decimals() -> Option<u8> {
        Some(3)
    }
}

impl RuntimeParameter for Dl645Parameter {
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

/// DL/T 645 action runtime model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dl645Action {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<Dl645Parameter>,
}

impl RuntimeAction for Dl645Action {
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

/// Internal helper for parsing DI values from configuration.
pub fn parse_di_from_config(config: &serde_json::Value) -> DriverResult<u32> {
    let di_value =
        config
            .get("di")
            .and_then(|v| v.as_str())
            .ok_or(DriverError::ConfigurationError(
                "DI is required for DL/T 645 point or parameter".to_string(),
            ))?;

    parse_di_str(di_value).ok_or(DriverError::ConfigurationError(format!(
        "Invalid DL/T 645 DI in configuration: {}",
        di_value
    )))
}
