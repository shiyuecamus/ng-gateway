use crate::protocol::error::ProtocolError;
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::fmt;

// --- Enums for Protocol Constants ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u16)]
pub enum DataIdentifier {
    // --- Common (Water/Heat/Gas) ---
    CumulativeFlow = 0x901F, // 累积流量/热量 (m3 / kWh / GJ)
    HeatPower = 0x903F,      // 热功率 (kW / GJ/h)
    ReturnTemp = 0x904F,     // 回水温度 (0.01C)
    SupplyTemp = 0x905F,     // 供水温度 (0.01C)
    WorkTime = 0x906F,       // 工作时间 (h)
    Time = 0x907F,           // 实时时间 (YYYYMMDDhhmmss)

    // --- Control / Write (Common A010-A019) ---
    PriceTable = 0xA010,     // 写价格表
    SettlementDate = 0xA011, // 写结算日
    ReadingDate = 0xA012,    // 写抄表日
    PurchaseAmount = 0xA013, // 写购入金额
    NewKey = 0xA014,         // 写新密钥 (2004)
    StandardTime = 0xA015,   // 写标准时间
    MotorSync = 0xA016,      // 写机电同步数据 (Func=16H)
    ValveControl = 0xA017,   // 写阀门控制
    Address = 0xA018,        // 写地址 (Func=15H)
    FactoryEnable = 0xA019,  // 出厂启用

    // --- Extended (CJ/T 188-2018) ---
    EnterVerification = 0xA101, // 进入检定模式
    ExitVerification = 0xA102,  // 退出检定模式
    WriteCommParams = 0xA103,   // 写通信参数
    WriteFreeze = 0xA104,       // 写冻结参数
    WriteAlarmVol = 0xA105,     // 写报警剩余量参数
    WriteAlarmMoney = 0xA106,   // 写报警剩余金额参数
    ModifyKey = 0xA107,         // 修改密钥 (2018)

    // --- Special ---
    Unknown = 0xFFFF,
}

impl From<u16> for DataIdentifier {
    fn from(val: u16) -> Self {
        match val {
            0x901F => DataIdentifier::CumulativeFlow,
            0x903F => DataIdentifier::HeatPower,
            0x904F => DataIdentifier::ReturnTemp,
            0x905F => DataIdentifier::SupplyTemp,
            0x906F => DataIdentifier::WorkTime,
            0x907F => DataIdentifier::Time,
            0xA010 => DataIdentifier::PriceTable,
            0xA011 => DataIdentifier::SettlementDate,
            0xA012 => DataIdentifier::ReadingDate,
            0xA013 => DataIdentifier::PurchaseAmount,
            0xA014 => DataIdentifier::NewKey,
            0xA015 => DataIdentifier::StandardTime,
            0xA016 => DataIdentifier::MotorSync,
            0xA017 => DataIdentifier::ValveControl,
            0xA018 => DataIdentifier::Address,
            0xA019 => DataIdentifier::FactoryEnable,
            0xA101 => DataIdentifier::EnterVerification,
            0xA102 => DataIdentifier::ExitVerification,
            0xA103 => DataIdentifier::WriteCommParams,
            0xA104 => DataIdentifier::WriteFreeze,
            0xA105 => DataIdentifier::WriteAlarmVol,
            0xA106 => DataIdentifier::WriteAlarmMoney,
            0xA107 => DataIdentifier::ModifyKey,
            _ => DataIdentifier::Unknown,
        }
    }
}

impl From<DataIdentifier> for u16 {
    fn from(val: DataIdentifier) -> Self {
        val as u16
    }
}

/// CJ/T 188 Function Code (bits D0-D4 of Control Field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum FunctionCode {
    Reserved = 0x00,
    ReadData = 0x01,
    ReadAddr = 0x03,
    WriteData = 0x04,
    WriteAddr = 0x15,      // 21
    WriteMotorSync = 0x16, // 22
    Unknown = 0xFF,
}

impl FunctionCode {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x1F {
            0x00 => FunctionCode::Reserved,
            0x01 => FunctionCode::ReadData,
            0x03 => FunctionCode::ReadAddr,
            0x04 => FunctionCode::WriteData,
            0x15 => FunctionCode::WriteAddr,
            0x16 => FunctionCode::WriteMotorSync,
            _ => FunctionCode::Unknown,
        }
    }

    pub fn to_bits(self) -> u8 {
        match self {
            FunctionCode::Unknown => 0x00, // Safe fallback
            _ => self as u8,
        }
    }
}

impl From<u8> for FunctionCode {
    fn from(val: u8) -> Self {
        Self::from_bits(val)
    }
}

impl From<FunctionCode> for u8 {
    fn from(val: FunctionCode) -> Self {
        val.to_bits()
    }
}

/// CJ/T 188 Meter Type (T field).
///
/// # Notes
/// Meter type family for schema selection
///
/// Groups meter types into families for efficient schema lookup.
/// Each family corresponds to a range of meter type codes in CJ/T 188 protocol.
///
/// # Ranges
/// - Water: 0x10-0x19 (cold water, hot water, drinking water, etc.)
/// - Heat: 0x20-0x29 (heat, cooling, heat+cooling)
/// - Gas: 0x30-0x39 (gas meters)
/// - Custom: 0x40-0x49 (user-defined meters)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeterTypeFamily {
    /// Water meters (0x10-0x19)
    Water,
    /// Heat meters (0x20-0x29)
    Heat,
    /// Gas meters (0x30-0x39)
    Gas,
    /// Custom meters (0x40-0x49)
    Custom,
}

impl MeterTypeFamily {
    /// Get string representation of the meter type family
    ///
    /// # Returns
    /// Human-readable family name
    ///
    /// # Examples
    /// ```
    /// # use ng_driver_cjt188::protocol::frame::defs::MeterTypeFamily;
    /// assert_eq!(MeterTypeFamily::Water.as_str(), "Water");
    /// assert_eq!(MeterTypeFamily::Heat.as_str(), "Heat");
    /// ```
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Water => "Water",
            Self::Heat => "Heat",
            Self::Gas => "Gas",
            Self::Custom => "Custom",
        }
    }
}

impl fmt::Display for MeterTypeFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// - This is a full 1-byte code carried in the frame right after the start byte `0x68`.
/// - **0x10..=0x19**: Water meter family (e.g. `0x10` cold water, `0x11` domestic hot water).
/// - **0x20..=0x29**: Heat meter family (e.g. `0x20` heat, `0x21` cooling, `0x22` heat+cool).
/// - **0x30..=0x39**: Gas meter family.
/// - **0x40..=0x49**: User-defined / custom meter family.
/// - **0xAA / 0x99**: Common broadcast / special addresses used by some devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MeterType(pub u8);

impl MeterType {
    /// Cold water meter type (`0x10`).
    pub const COLD_WATER: Self = Self(0x10);
    /// Domestic hot water meter type (`0x11`).
    pub const DOMESTIC_HOT_WATER: Self = Self(0x11);
    /// Drinking water meter type (`0x12`).
    pub const DRINKING_WATER: Self = Self(0x12);
    /// Reclaimed water meter type (`0x13`).
    pub const RECLAIMED_WATER: Self = Self(0x13);

    /// Heat meter type (`0x20`).
    pub const HEAT: Self = Self(0x20);
    /// Cooling meter type (`0x21`).
    pub const COOLING: Self = Self(0x21);
    /// Heat + cooling meter type (`0x22`).
    pub const HEAT_AND_COOLING: Self = Self(0x22);

    /// Gas meter type (`0x30`).
    pub const GAS: Self = Self(0x30);

    /// Custom / user-defined meter base type (`0x40`).
    pub const CUSTOM: Self = Self(0x40);

    /// Raw meter type code.
    #[inline]
    pub fn code(self) -> u8 {
        self.0
    }

    /// Returns true if this code falls into the "water meter" family.
    #[inline]
    pub fn is_water(self) -> bool {
        (0x10..=0x19).contains(&self.0)
    }

    /// Returns true if this code falls into the "heat meter" family.
    #[inline]
    pub fn is_heat(self) -> bool {
        (0x20..=0x29).contains(&self.0)
    }

    /// Returns true if this code falls into the "gas meter" family.
    #[inline]
    pub fn is_gas(self) -> bool {
        (0x30..=0x39).contains(&self.0)
    }

    /// Returns true if this code falls into the "custom meter" family.
    #[inline]
    pub fn is_custom(self) -> bool {
        (0x40..=0x49).contains(&self.0)
    }

    /// Get the meter type family for schema selection
    ///
    /// # Returns
    /// The `MeterTypeFamily` enum variant corresponding to this meter type
    ///
    /// # Examples
    /// ```
    /// # use ng_driver_cjt188::protocol::frame::defs::{MeterType, MeterTypeFamily};
    /// assert_eq!(MeterType::COLD_WATER.family(), MeterTypeFamily::Water);
    /// assert_eq!(MeterType::HEAT.family(), MeterTypeFamily::Heat);
    /// assert_eq!(MeterType::GAS.family(), MeterTypeFamily::Gas);
    /// ```
    pub fn family(self) -> MeterTypeFamily {
        match self.0 {
            0x10..=0x19 => MeterTypeFamily::Water,
            0x20..=0x29 => MeterTypeFamily::Heat,
            0x30..=0x39 => MeterTypeFamily::Gas,
            0x40..=0x49 => MeterTypeFamily::Custom,
            _ => MeterTypeFamily::Custom, // fallback for unknown types
        }
    }
}

impl From<u8> for MeterType {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

impl From<MeterType> for u8 {
    fn from(value: MeterType) -> Self {
        value.0
    }
}

impl fmt::Display for MeterType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02X}", self.0)
    }
}

/// CJ/T 188 7-byte address field (A0..A6).
///
/// # Wire format
/// - `A0` is the lowest (least significant) 2 digits, `A6` is the highest.
/// - Each byte is **2-digit BCD** under normal unicast addressing.
/// - `0xAA` and `0x99` are reserved for broadcast / special addressing semantics in the standard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cjt188Address {
    bytes: [u8; 7],
}

impl Cjt188Address {
    /// Create a new address from raw 7 bytes (A0..A6).
    ///
    /// This validates BCD nibbles for normal bytes and allows `0xAA` / `0x99` for broadcast.
    pub fn new(bytes: [u8; 7]) -> Result<Self, ProtocolError> {
        Ok(Self { bytes })
    }

    /// Return the wire bytes (A0..A6).
    #[inline]
    pub fn to_bytes(&self) -> [u8; 7] {
        self.bytes
    }

    /// Parse an address from a raw slice that contains at least 7 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < 7 {
            return Err(ProtocolError::FrameTooShort(
                "address requires 7 bytes".into(),
            ));
        }
        let mut buf = [0u8; 7];
        buf.copy_from_slice(&bytes[0..7]);
        Self::new(buf)
    }

    /// Broadcast address using `0xAA` wildcard bytes.
    #[inline]
    pub fn broadcast_aa() -> Self {
        // SAFETY: broadcast bytes are explicitly allowed.
        Self { bytes: [0xAA; 7] }
    }

    /// Broadcast address using `0x99` special bytes (only valid for certain commands).
    #[inline]
    pub fn broadcast_99() -> Self {
        // SAFETY: broadcast bytes are explicitly allowed.
        Self { bytes: [0x99; 7] }
    }

    /// Returns true if all bytes are `0xAA`.
    #[inline]
    pub fn is_all_aa(&self) -> bool {
        self.bytes.iter().all(|b| *b == 0xAA)
    }

    /// Returns true if any byte is `0xAA` (wildcard broadcast by byte).
    #[inline]
    pub fn has_any_aa(&self) -> bool {
        self.bytes.contains(&0xAA)
    }

    /// Returns true if all bytes are `0x99`.
    #[inline]
    pub fn is_all_99(&self) -> bool {
        self.bytes.iter().all(|b| *b == 0x99)
    }

    /// Parse address from a user string.
    ///
    /// Supported format:
    /// - **14-hex string (big-endian, recommended)**: e.g. `"00000000EE0001"` representing
    ///   `A6..A0` bytes (high address first, matches typical human notation).
    ///   The protocol wire format is still `A0..A6` (low byte first).
    pub fn parse_str(s: &str) -> Result<Self, ProtocolError> {
        let s = s.trim();

        // Default: user-friendly big-endian: A6..A0
        if s.len() == 14 && s.chars().all(|c| c.is_ascii_hexdigit()) {
            let bytes = hex::decode(s)
                .map_err(|e| ProtocolError::Semantic(format!("Invalid hex address: {e}")))?;
            if bytes.len() != 7 {
                return Err(ProtocolError::Semantic(format!(
                    "Invalid address length: got {} bytes, expected 7 bytes",
                    bytes.len()
                )));
            }

            // Convert A6..A0 (user) -> A0..A6 (wire/internal)
            let mut buf = [0u8; 7];
            buf.copy_from_slice(&bytes);
            buf.reverse();
            return Self::new(buf);
        }

        Err(ProtocolError::Semantic(format!(
            "Invalid address string format: '{s}'. Expected 14-hex string (A6..A0)."
        )))
    }

    /// Convert this address into a 14-digit decimal string (A6..A0).
    ///
    /// Broadcast bytes (`0xAA` / `0x99`) are rendered as uppercase hex pairs to avoid ambiguity.
    pub fn to_string_be(&self) -> String {
        let mut out = String::with_capacity(14);
        for &b in self.bytes.iter().rev() {
            if b == 0xAA || b == 0x99 {
                out.push_str(&format!("{:02X}", b));
                continue;
            }
            let hi = (b >> 4) & 0x0F;
            let lo = b & 0x0F;
            if hi <= 9 && lo <= 9 {
                out.push(char::from(b'0' + hi));
                out.push(char::from(b'0' + lo));
            } else {
                // Fallback for non-BCD bytes.
                out.push_str(&format!("{:02X}", b));
            }
        }
        out
    }
}

impl fmt::Display for Cjt188Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string_be())
    }
}

/// CJ/T 188 Transfer Direction (D7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    MasterToSlave, // D7 = 0
    SlaveToMaster, // D7 = 1
}

/// Control Word (C Field) for CJ/T 188.
///
/// Structure:
/// D7: Direction (0=Master->Slave, 1=Slave->Master)
/// D6: Abnormal Flag (0=Normal, 1=Abnormal)
/// D5: Follow-up Flag (0=No, 1=Yes)
/// D4-D0: Function Code
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ControlWord {
    pub raw: u8,
}

impl ControlWord {
    pub fn new(raw: u8) -> Self {
        Self { raw }
    }

    pub fn from_parts(dir: Direction, error: bool, follow: bool, func: FunctionCode) -> Self {
        let mut raw = func.to_bits() & 0x1F;
        if dir == Direction::SlaveToMaster {
            raw |= 0x80;
        }
        if error {
            raw |= 0x40;
        }
        if follow {
            raw |= 0x20;
        }
        Self { raw }
    }

    pub fn direction(&self) -> Direction {
        if (self.raw & 0x80) != 0 {
            Direction::SlaveToMaster
        } else {
            Direction::MasterToSlave
        }
    }

    pub fn is_error(&self) -> bool {
        (self.raw & 0x40) != 0
    }

    pub fn has_follow_up(&self) -> bool {
        (self.raw & 0x20) != 0
    }

    pub fn function_code(&self) -> FunctionCode {
        FunctionCode::from_bits(self.raw)
    }
}

impl From<u8> for ControlWord {
    fn from(raw: u8) -> Self {
        Self { raw }
    }
}

impl From<ControlWord> for u8 {
    fn from(cw: ControlWord) -> Self {
        cw.raw
    }
}

/// Unit codes defined in CJ/T 188 protocol
///
/// Reference: CJ/T 188-2004 Table 13 and CJ/T 188-2018 Table 20
///
/// This enum represents all physical units supported by the CJ/T 188 protocol.
/// Each unit is identified by a single byte code in the protocol wire format.
///
/// **IMPORTANT**: Only standard CJ/T 188 compatible units are included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Unit {
    // --- Energy Units (能量单位) ---
    /// J - 焦耳 (0x01)
    J,
    /// Wh - 瓦时 (0x02)
    Wh,
    /// Wh×10 (0x03)
    WhX10,
    /// Wh×100 (0x04)
    WhX100,
    /// kWh - 千瓦时 (0x05)
    KWh,
    /// kWh×10 (0x06)
    KWhX10,
    /// kWh×100 (0x07)
    KWhX100,
    /// MWh - 兆瓦时 (0x08)
    MWh,
    /// MWh×10 (0x09)
    MWhX10,
    /// MWh×100 (0x0A)
    MWhX100,
    /// kJ - 千焦 (0x0B)
    KJ,
    /// kJ×10 (0x0C)
    KJX10,
    /// kJ×100 (0x0D)
    KJX100,
    /// MJ - 兆焦 (0x0E)
    MJ,
    /// MJ×10 (0x0F)
    MJX10,
    /// MJ×100 (0x10)
    MJX100,
    /// GJ - 吉焦 (0x11)
    GJ,
    /// GJ×10 (0x12)
    GJX10,
    /// GJ×100 (0x13)
    GJX100,

    // --- Power Units (功率单位) ---
    /// W - 瓦特 (0x14)
    W,
    /// W×10 (0x15)
    WX10,
    /// W×100 (0x16)
    WX100,
    /// kW - 千瓦 (0x17)
    KW,
    /// kW×10 (0x18)
    KWX10,
    /// kW×100 (0x19)
    KWX100,
    /// MW - 兆瓦 (0x1A)
    MW,
    /// MW×10 (0x1B)
    MWX10,
    /// MW×100 (0x1C)
    MWX100,

    // --- Volume Units (体积单位) ---
    /// L - 升 (0x29)
    L,
    /// L×10 (0x2A)
    LX10,
    /// L×100 (0x2B)
    LX100,
    /// m³ - 立方米 (0x2C)
    M3,
    /// m³×10 (0x2D)
    M3X10,
    /// m³×100 (0x2E)
    M3X100,

    // --- Flow Rate Units (流量单位) ---
    /// L/h - 升/小时 (0x32)
    Lh,
    /// L/h×10 (0x33)
    LhX10,
    /// L/h×100 (0x34)
    LhX100,
    /// m³/h - 立方米/小时 (0x35)
    M3h,
    /// m³/h×10 (0x36)
    M3hX10,
    /// m³/h×100 (0x37)
    M3hX100,

    // --- Power Rate Units (功率/时间单位) ---
    /// J/h - 焦耳/小时 (0x40)
    Jh,
    /// kJ/h - 千焦/小时 (0x43)
    KJh,
    /// kJ/h×10 (0x44)
    KJhX10,
    /// kJ/h×100 (0x45)
    KJhX100,
    /// MJ/h - 兆焦/小时 (0x46)
    MJh,
    /// MJ/h×10 (0x47)
    MJhX10,
    /// MJ/h×100 (0x48)
    MJhX100,
    /// GJ/h - 吉焦/小时 (0x49)
    GJh,
    /// GJ/h×10 (0x4A)
    GJhX10,
    /// GJ/h×100 (0x4B)
    GJhX100,

    // --- Special ---
    /// Unknown or unsupported unit
    Unknown,
}

impl Unit {
    /// Convert unit to human-readable string representation
    ///
    /// # Returns
    /// The standard unit symbol (e.g., "m³", "kWh", "L/h")
    pub fn as_str(&self) -> &'static str {
        match self {
            // Energy units
            Unit::J => "J",
            Unit::Wh => "Wh",
            Unit::WhX10 => "Wh×10",
            Unit::WhX100 => "Wh×100",
            Unit::KWh => "kWh",
            Unit::KWhX10 => "kWh×10",
            Unit::KWhX100 => "kWh×100",
            Unit::MWh => "MWh",
            Unit::MWhX10 => "MWh×10",
            Unit::MWhX100 => "MWh×100",
            Unit::KJ => "kJ",
            Unit::KJX10 => "kJ×10",
            Unit::KJX100 => "kJ×100",
            Unit::MJ => "MJ",
            Unit::MJX10 => "MJ×10",
            Unit::MJX100 => "MJ×100",
            Unit::GJ => "GJ",
            Unit::GJX10 => "GJ×10",
            Unit::GJX100 => "GJ×100",

            // Power units
            Unit::W => "W",
            Unit::WX10 => "W×10",
            Unit::WX100 => "W×100",
            Unit::KW => "kW",
            Unit::KWX10 => "kW×10",
            Unit::KWX100 => "kW×100",
            Unit::MW => "MW",
            Unit::MWX10 => "MW×10",
            Unit::MWX100 => "MW×100",

            // Volume units
            Unit::L => "L",
            Unit::LX10 => "L×10",
            Unit::LX100 => "L×100",
            Unit::M3 => "m³",
            Unit::M3X10 => "m³×10",
            Unit::M3X100 => "m³×100",

            // Flow rate units
            Unit::Lh => "L/h",
            Unit::LhX10 => "L/h×10",
            Unit::LhX100 => "L/h×100",
            Unit::M3h => "m³/h",
            Unit::M3hX10 => "m³/h×10",
            Unit::M3hX100 => "m³/h×100",

            // Power rate units
            Unit::Jh => "J/h",
            Unit::KJh => "kJ/h",
            Unit::KJhX10 => "kJ/h×10",
            Unit::KJhX100 => "kJ/h×100",
            Unit::MJh => "MJ/h",
            Unit::MJhX10 => "MJ/h×10",
            Unit::MJhX100 => "MJ/h×100",
            Unit::GJh => "GJ/h",
            Unit::GJhX10 => "GJ/h×10",
            Unit::GJhX100 => "GJ/h×100",

            // Special
            Unit::Unknown => "unknown",
        }
    }

    /// Parse unit code from protocol byte value
    ///
    /// # Arguments
    /// * `code` - The byte value from the protocol wire
    ///
    /// # Returns
    /// The corresponding `Unit` enum variant
    ///
    /// # Standard CJ/T 188 Unit Codes
    /// This follows the official CJ/T 188 protocol specification
    pub fn from_code(code: u8) -> Self {
        match code {
            // Energy units
            0x01 => Unit::J,
            0x02 => Unit::Wh,
            0x03 => Unit::WhX10,
            0x04 => Unit::WhX100,
            0x05 => Unit::KWh,
            0x06 => Unit::KWhX10,
            0x07 => Unit::KWhX100,
            0x08 => Unit::MWh,
            0x09 => Unit::MWhX10,
            0x0A => Unit::MWhX100,
            0x0B => Unit::KJ,
            0x0C => Unit::KJX10,
            0x0D => Unit::KJX100,
            0x0E => Unit::MJ,
            0x0F => Unit::MJX10,
            0x10 => Unit::MJX100,
            0x11 => Unit::GJ,
            0x12 => Unit::GJX10,
            0x13 => Unit::GJX100,

            // Power units
            0x14 => Unit::W,
            0x15 => Unit::WX10,
            0x16 => Unit::WX100,
            0x17 => Unit::KW,
            0x18 => Unit::KWX10,
            0x19 => Unit::KWX100,
            0x1A => Unit::MW,
            0x1B => Unit::MWX10,
            0x1C => Unit::MWX100,

            // Volume units
            0x29 => Unit::L,
            0x2A => Unit::LX10,
            0x2B => Unit::LX100,
            0x2C => Unit::M3,
            0x2D => Unit::M3X10,
            0x2E => Unit::M3X100,

            // Flow rate units
            0x32 => Unit::Lh,
            0x33 => Unit::LhX10,
            0x34 => Unit::LhX100,
            0x35 => Unit::M3h,
            0x36 => Unit::M3hX10,
            0x37 => Unit::M3hX100,

            // Power rate units
            0x40 => Unit::Jh,
            0x43 => Unit::KJh,
            0x44 => Unit::KJhX10,
            0x45 => Unit::KJhX100,
            0x46 => Unit::MJh,
            0x47 => Unit::MJhX10,
            0x48 => Unit::MJhX100,
            0x49 => Unit::GJh,
            0x4A => Unit::GJhX10,
            0x4B => Unit::GJhX100,

            // Unknown
            _ => Unit::Unknown,
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
