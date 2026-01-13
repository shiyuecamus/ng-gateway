use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    DriverError, ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter,
    RuntimePoint, Status,
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::sync::Arc;

/// Modbus channel
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusChannel {
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
    pub config: ModbusChannelConfig,
}

/// Modbus channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusChannelConfig {
    /// Connection settings based on processor type
    pub connection: ModbusConnection,
    /// Byte order (endianness)
    pub byte_order: Endianness,
    /// Word order
    pub word_order: Endianness,
    /// Maximum address gap for batch merging
    #[serde(default = "ModbusChannelConfig::default_max_gap")]
    pub max_gap: u16,
    /// Maximum batch size (number of registers/coils)
    #[serde(default = "ModbusChannelConfig::default_max_batch")]
    pub max_batch: u16,
}

impl ModbusChannelConfig {
    /// Default value for max_gap (10 registers/coils)
    fn default_max_gap() -> u16 {
        10
    }

    /// Default value for max_batch (100 registers/coils)
    fn default_max_batch() -> u16 {
        100
    }
}

impl DriverConfig for ModbusChannelConfig {}

impl RuntimeChannel for ModbusChannel {
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

/// Modbus-specific device
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusDevice {
    pub id: i32,
    /// Channel ID
    pub channel_id: i32,
    /// Device Name
    pub device_name: String,
    /// Device Type
    pub device_type: String,
    /// Status
    pub status: Status,
    /// Slave ID
    pub slave_id: u8,
}

impl RuntimeDevice for ModbusDevice {
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

/// Modbus-specific point
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusPoint {
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
    pub function_code: ModbusFunctionCode,
    pub address: u16,
    pub quantity: u16,
}

impl RuntimePoint for ModbusPoint {
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

/// Modbus-specific parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusParameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,
    pub function_code: ModbusFunctionCode,
    pub address: u16,
    pub quantity: u16,
}

impl RuntimeParameter for ModbusParameter {
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

/// Modbus-specific action
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModbusAction {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<ModbusParameter>,
}

impl RuntimeAction for ModbusAction {
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
        // Convert concrete parameters to trait objects using Arc and clone to own data
        self.input_parameters
            .iter()
            .map(|p| Arc::new(p.clone()) as Arc<dyn RuntimeParameter>)
            .collect()
    }
}

/// Internal batch request for merged reads
#[derive(Debug, Clone)]
pub struct ReadBatch<'a> {
    pub function: ModbusFunctionCode,
    pub start_addr: u16,
    pub quantity: u16,
    /// Points that are covered by this batch (already filtered by function)
    pub points: Vec<&'a ModbusPoint>,
}

// Prepare a per-parameter execution plan: (function, address, words/coils)
// For coils: Vec<bool>; for registers: Vec<u16>
pub struct WritePlan {
    pub function: ModbusFunctionCode,
    pub address: u16,
    pub coils: Option<Vec<bool>>,
    pub registers: Option<Vec<u16>>,
}

/// Modbus connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ModbusConnection {
    Tcp {
        host: String,
        port: u16,
    },
    Rtu {
        port: String,
        baud_rate: u32,
        data_bits: DataBits,
        stop_bits: StopBits,
        parity: Parity,
    },
}

/// Serial communication settings
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

/// Serial stop bits
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

/// Serial parity settings
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Parity {
    None = 0,
    Odd = 1,
    Even = 2,
}

impl From<Parity> for tokio_serial::Parity {
    fn from(parity: Parity) -> Self {
        match parity {
            Parity::None => tokio_serial::Parity::None,
            Parity::Odd => tokio_serial::Parity::Odd,
            Parity::Even => tokio_serial::Parity::Even,
        }
    }
}

impl From<&Parity> for tokio_serial::Parity {
    fn from(parity: &Parity) -> Self {
        match parity {
            Parity::None => tokio_serial::Parity::None,
            Parity::Odd => tokio_serial::Parity::Odd,
            Parity::Even => tokio_serial::Parity::Even,
        }
    }
}

/// Byte order (endianness)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum Endianness {
    BigEndian = 0,
    LittleEndian = 1,
}

/// Modbus function codes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum ModbusFunctionCode {
    /// Read Coils (0x01)
    ReadCoils = 1,
    /// Read Discrete Inputs (0x02)
    ReadDiscreteInputs = 2,
    /// Read Holding Registers (0x03)
    ReadHoldingRegisters = 3,
    /// Read Input Registers (0x04)
    ReadInputRegisters = 4,
    /// Write Single Coil (0x05)
    WriteSingleCoil = 5,
    /// Write Single Register (0x06)
    WriteSingleRegister = 6,
    /// Write Multiple Coils (0x0F)
    WriteMultipleCoils = 15,
    /// Write Multiple Registers (0x10)
    WriteMultipleRegisters = 16,
}

impl From<ModbusFunctionCode> for u8 {
    fn from(function_code: ModbusFunctionCode) -> Self {
        function_code as u8
    }
}

impl From<&ModbusFunctionCode> for u8 {
    fn from(function_code: &ModbusFunctionCode) -> Self {
        *function_code as u8
    }
}

impl TryFrom<u8> for ModbusFunctionCode {
    type Error = DriverError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(ModbusFunctionCode::ReadCoils),
            2 => Ok(ModbusFunctionCode::ReadDiscreteInputs),
            3 => Ok(ModbusFunctionCode::ReadHoldingRegisters),
            4 => Ok(ModbusFunctionCode::ReadInputRegisters),
            5 => Ok(ModbusFunctionCode::WriteSingleCoil),
            6 => Ok(ModbusFunctionCode::WriteSingleRegister),
            15 => Ok(ModbusFunctionCode::WriteMultipleCoils),
            16 => Ok(ModbusFunctionCode::WriteMultipleRegisters),
            _ => Err(DriverError::ConfigurationError(format!(
                "Invalid function code: {value}"
            ))),
        }
    }
}

impl TryFrom<&u8> for ModbusFunctionCode {
    type Error = DriverError;

    fn try_from(value: &u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(ModbusFunctionCode::ReadCoils),
            2 => Ok(ModbusFunctionCode::ReadDiscreteInputs),
            3 => Ok(ModbusFunctionCode::ReadHoldingRegisters),
            4 => Ok(ModbusFunctionCode::ReadInputRegisters),
            5 => Ok(ModbusFunctionCode::WriteSingleCoil),
            6 => Ok(ModbusFunctionCode::WriteSingleRegister),
            15 => Ok(ModbusFunctionCode::WriteMultipleCoils),
            16 => Ok(ModbusFunctionCode::WriteMultipleRegisters),
            _ => Err(DriverError::ConfigurationError(format!(
                "Invalid function code: {value}"
            ))),
        }
    }
}

impl TryFrom<serde_json::Value> for ModbusFunctionCode {
    type Error = DriverError;

    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        if !value.is_number() {
            return Err(DriverError::ConfigurationError(format!(
                "Invalid function code: {value}"
            )));
        }
        ModbusFunctionCode::try_from(value.as_u64().unwrap() as u8)
    }
}

impl TryFrom<&serde_json::Value> for ModbusFunctionCode {
    type Error = DriverError;

    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        if !value.is_number() {
            return Err(DriverError::ConfigurationError(format!(
                "Invalid function code: {value}"
            )));
        }
        ModbusFunctionCode::try_from(value.as_u64().unwrap() as u8)
    }
}
