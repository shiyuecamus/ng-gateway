// ===== Typed value representation and encoding =====
use super::super::error::{Error, Result};
use super::WireEncode;
use bytes::BufMut;
use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

// ... (previous Enums and Impls unchanged) ...

/// Typed scalar value decoded from S7 payload
#[derive(Debug, Clone)]
pub enum S7DataValue {
    /// Single bit value (true/false)
    Bit(bool),
    /// Unsigned 8-bit byte
    Byte(u8),
    /// Latin-1 character mapped to Unicode scalar
    Char(char),
    /// Unsigned 16-bit integer (big-endian on wire)
    Word(u16),
    /// Signed 16-bit integer (big-endian on wire)
    Int(i16),
    /// Unsigned 32-bit integer (big-endian on wire)
    DWord(u32),
    /// Signed 32-bit integer (big-endian on wire)
    DInt(i32),
    /// IEEE-754 32-bit float (big-endian on wire)
    Real(f32),
    /// Latin-1 encoded string
    String(String),
    /// UTF-16 (BE) encoded wide string
    WString(String),
    /// Timer interpreted as chrono::Duration in milliseconds
    Timer(Duration),
    /// Counter interpreted as signed 32-bit value
    Counter(i32),
    /// S7 Date mapped to chrono::NaiveDate
    Date(NaiveDate),
    /// S7 DATE_AND_TIME (8 bytes BCD) mapped to chrono::NaiveDateTime
    DateTime(NaiveDateTime),
    /// S7 DTL (12 bytes) mapped to chrono::NaiveDateTime
    DateTimeLong(NaiveDateTime),
    /// S7 TimeOfDay mapped to chrono::NaiveTime
    TimeOfDay(NaiveTime),
    /// S5Time (u16) mapped to chrono::Duration
    S5Time(Duration),
    /// IEC TIME (u32 ms) mapped to chrono::Duration
    Time(Duration),
}

impl WireEncode for S7DataValue {
    type Error = Error;
    type Context = ();

    fn encoded_len(&self, _ctx: &Self::Context) -> usize {
        match self {
            S7DataValue::Bit(_) => 1,
            S7DataValue::Byte(_) => 1,
            S7DataValue::Char(_) => 1,
            S7DataValue::Word(_) => 2,
            S7DataValue::Int(_) => 2,
            S7DataValue::DWord(_) => 4,
            S7DataValue::DInt(_) => 4,
            S7DataValue::Real(_) => 4,
            S7DataValue::String(s) => s.len() + 2,
            S7DataValue::WString(s) => s.encode_utf16().count() * 2 + 4,
            S7DataValue::Timer(_) => 2,
            S7DataValue::Counter(_) => 2,
            S7DataValue::Date(_) => 2,
            S7DataValue::DateTime(_) => 8,
            S7DataValue::DateTimeLong(_) => 12,
            S7DataValue::TimeOfDay(_) => 4,
            S7DataValue::S5Time(_) => 2,
            S7DataValue::Time(_) => 4,
        }
    }

    fn encode_to<B: BufMut>(&self, dst: &mut B, _ctx: &Self::Context) -> Result<()> {
        match self {
            S7DataValue::Bit(v) => dst.put_u8(if *v { 1 } else { 0 }),
            S7DataValue::Byte(v) => dst.put_u8(*v),
            S7DataValue::Char(c) => dst.put_u8(*c as u8),
            S7DataValue::Word(v) => dst.put_u16(*v),
            S7DataValue::Int(v) => dst.put_i16(*v),
            S7DataValue::DWord(v) => dst.put_u32(*v),
            S7DataValue::DInt(v) => dst.put_i32(*v),
            S7DataValue::Real(v) => dst.put_u32(v.to_bits()),
            S7DataValue::String(s) => {
                let len = s.len().min(254);
                dst.put_u8(254);
                dst.put_u8(len as u8);
                for b in s.bytes().take(len) {
                    dst.put_u8(b);
                }
            }
            S7DataValue::WString(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                let len = units.len().min(254);
                dst.put_u16(508);
                dst.put_u16(len as u16);
                for u in units.into_iter().take(len) {
                    dst.put_u16(u);
                }
            }
            S7DataValue::Timer(dur) => {
                let raw = s5time_from_duration(*dur);
                dst.put_u16(raw);
            }
            S7DataValue::Counter(v) => dst.put_i16(*v as i16),
            S7DataValue::Date(d) => {
                if let Some(base) = NaiveDate::from_ymd_opt(1990, 1, 1) {
                    let days = (*d - base).num_days() as u16;
                    dst.put_u16(days);
                } else {
                    dst.put_u16(0);
                }
            }
            S7DataValue::DateTime(dt) => {
                // Approximate encoding: not full BCD
                let y = if dt.year() < 2000 {
                    dt.year() - 1900
                } else {
                    dt.year() - 2000
                };
                dst.put_u8(y as u8);
                dst.put_u8(dt.month() as u8);
                dst.put_u8(dt.day() as u8);
                dst.put_u8(dt.hour() as u8);
                dst.put_u8(dt.minute() as u8);
                dst.put_u8(dt.second() as u8);
                dst.put_u8((dt.and_utc().timestamp_subsec_millis() / 10) as u8);
                dst.put_u8(0);
            }
            S7DataValue::TimeOfDay(t) => {
                let ms = t.num_seconds_from_midnight() * 1000 + (t.nanosecond() / 1_000_000);
                dst.put_u32(ms);
            }
            S7DataValue::S5Time(dur) => {
                let raw = s5time_from_duration(*dur);
                dst.put_u16(raw);
            }
            S7DataValue::Time(dur) => {
                let ms = dur.num_milliseconds().clamp(0, u32::MAX as i64) as u32;
                dst.put_u32(ms);
            }
            S7DataValue::DateTimeLong(dt) => {
                // Encode DTL (12 bytes): year(u32 BE) month day hour minute second nanos(24-bit BE)
                let year = dt.year() as u32;
                dst.put_u32(year);
                dst.put_u8(dt.month() as u8);
                dst.put_u8(dt.day() as u8);
                dst.put_u8(dt.hour() as u8);
                dst.put_u8(dt.minute() as u8);
                dst.put_u8(dt.second() as u8);
                let nanos = dt.and_utc().timestamp_subsec_nanos();
                let b1 = ((nanos >> 16) & 0xFF) as u8;
                let b2 = ((nanos >> 8) & 0xFF) as u8;
                let b3 = (nanos & 0xFF) as u8;
                dst.put_u8(b1);
                dst.put_u8(b2);
                dst.put_u8(b3);
            }
        }
        Ok(())
    }
}

/// COTP TPDU type values (subset)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CotpType {
    /// Connection Request
    Cr = 0xE0,
    /// Connection Confirm
    Cc = 0xD0,
    /// Disconnection Request
    Dr = 0x80,
    /// Disconnection Confirm
    Dc = 0xC0,
    /// Reject
    Re = 0x70,
    /// Data
    D = 0xF0,
}

impl TryFrom<u8> for CotpType {
    type Error = ();

    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0xE0 => Ok(CotpType::Cr),
            0xD0 => Ok(CotpType::Cc),
            0x80 => Ok(CotpType::Dr),
            0xC0 => Ok(CotpType::Dc),
            0x70 => Ok(CotpType::Re),
            0xF0 => Ok(CotpType::D),
            _ => Err(()),
        }
    }
}

/// Minimal S7 PDU kinds we will support initially
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7PduType {
    Job = 0x01,
    Ack = 0x02,
    AckData = 0x03,
    UserData = 0x07,
}

impl TryFrom<u8> for S7PduType {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0x01 => Ok(S7PduType::Job),
            0x02 => Ok(S7PduType::Ack),
            0x03 => Ok(S7PduType::AckData),
            0x07 => Ok(S7PduType::UserData),
            _ => Err(()),
        }
    }
}
// Parameters for ReadVar/WriteVar are modeled as zero-copy views in param_ref.rs

/// S7 Function codes
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7Function {
    /// CPU Service
    CpuService = 0x00,
    /// Read Variable
    ReadVar = 0x04,
    /// Write Variable
    WriteVar = 0x05,
    /// Request download
    StartDownload = 0xFA,
    /// Request download
    Download = 0xFB,
    /// Download block
    EndDownload = 0xFC,
    /// Start upload
    StartUpload = 0x1D,
    /// Upload
    Upload = 0x1E,
    /// End upload
    EndUpload = 0x1F,
    /// PLC Control
    PlcControl = 0x28,
    /// PLC Stop
    PlcStop = 0x29,
    /// Setup Communication (param: [0xF0, 0x00])
    SetupCommunication = 0xF0,
}

impl TryFrom<u8> for S7Function {
    type Error = ();

    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0x00 => Ok(S7Function::CpuService),
            0xF0 => Ok(S7Function::SetupCommunication),
            0x04 => Ok(S7Function::ReadVar),
            0x05 => Ok(S7Function::WriteVar),
            0xFA => Ok(S7Function::StartDownload),
            0xFB => Ok(S7Function::Download),
            0xFC => Ok(S7Function::EndDownload),
            0x1D => Ok(S7Function::StartUpload),
            0x1E => Ok(S7Function::Upload),
            0x1F => Ok(S7Function::EndUpload),
            0x28 => Ok(S7Function::PlcControl),
            0x29 => Ok(S7Function::PlcStop),
            _ => Err(()),
        }
    }
}

/// S7 ANY Syntax Identifier
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7SyntaxId {
    /// Address data S7-Any pointer-like
    S7Any = 0x10,
    /// R_ID for PBC
    PbcRid = 0x13,
    /// Alarm lock/free dataset
    AlarmLockfree = 0x15,
    /// Alarm indication dataset
    AlarmIndication = 0x16,
    /// Alarm acknowledge message dataset
    AlarmAcknowledge = 0x19,
    /// Alarm query request dataset
    AlarmQueryRequest = 0x1A,
    /// Notify indication dataset
    NotifyIndication = 0x1C,
    /// DRIVE ES Any seen on Drive ES Starter with routing over S7
    DriveEsAny = 0xA2,
    /// Kind of DB block read， seen only at an S7-400
    DBRead = 0xB0,
    /// Symbolic byteAddress mode of S7-1200
    SymbolicByteAddress = 0xB2,
    /// Sinumerik NCK HMI access
    SinumerikNckHmiAccess = 0x82,
}

impl TryFrom<u8> for S7SyntaxId {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0x10 => Ok(S7SyntaxId::S7Any),
            0x13 => Ok(S7SyntaxId::PbcRid),
            0x15 => Ok(S7SyntaxId::AlarmLockfree),
            0x16 => Ok(S7SyntaxId::AlarmIndication),
            0x19 => Ok(S7SyntaxId::AlarmAcknowledge),
            0x1A => Ok(S7SyntaxId::AlarmQueryRequest),
            0x1C => Ok(S7SyntaxId::NotifyIndication),
            0xA2 => Ok(S7SyntaxId::DriveEsAny),
            0xB0 => Ok(S7SyntaxId::DBRead),
            0xB2 => Ok(S7SyntaxId::SymbolicByteAddress),
            0x82 => Ok(S7SyntaxId::SinumerikNckHmiAccess),
            _ => Err(()),
        }
    }
}

/// S7 Memory/Area codes
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7Area {
    /// System info of 200 family
    SI200 = 0x03,
    /// System flags of 200 family
    SF200 = 0x05,
    /// Analog inputs of 200 family
    AI200 = 0x06,
    /// Analog outputs of 200 family
    AO200 = 0x07,
    /// Direct peripheral access
    DP = 0x80,
    /// Inputs
    I = 0x81,
    /// Outputs
    O = 0x82,
    /// Merkers
    M = 0x83,
    /// Data Blocks (DB)
    DB = 0x84,
    /// Instance data blocks
    DI = 0x85,
    /// Local data
    L = 0x86,
    /// Unknown yet
    V = 0x87,
    /// Counters
    C = 0x1C,
    /// Timers
    T = 0x1D,
    /// IEC counters of 200 family
    Iecc = 0x1E,
    ///IEC timers of 200 family
    Iecd = 0x1F,
}

impl TryFrom<u8> for S7Area {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        match v {
            0x03 => Ok(S7Area::SI200),
            0x05 => Ok(S7Area::SF200),
            0x06 => Ok(S7Area::AI200),
            0x07 => Ok(S7Area::AO200),
            0x80 => Ok(S7Area::DP),
            0x81 => Ok(S7Area::I),
            0x82 => Ok(S7Area::O),
            0x83 => Ok(S7Area::M),
            0x84 => Ok(S7Area::DB),
            0x85 => Ok(S7Area::DI),
            0x86 => Ok(S7Area::L),
            0x87 => Ok(S7Area::V),
            0x1C => Ok(S7Area::C),
            0x1D => Ok(S7Area::T),
            0x1E => Ok(S7Area::Iecc),
            0x1F => Ok(S7Area::Iecd),
            _ => Err(()),
        }
    }
}

/// Transport size codes (used in var specs and data items)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7TransportSize {
    Bit = 0x01,
    Byte = 0x02,
    Char = 0x03,
    Word = 0x04,
    Int = 0x05,
    DWord = 0x06,
    DInt = 0x07,
    Real = 0x08,
    Date = 0x09,
    TimeOfDay = 0x0A,
    Time = 0x0B,
    S5Time = 0x0C,
    DateTime = 0x0F,
    DateTimeLong = 0x10,
    Counter = 0x1C,
    Timer = 0x1D,
    WString = 0xFF,
    String = 0x00,
    IECTimer = 0x1E,
    IECEvent = 0x1F,
    HSCounter = 0x20,
}

impl TryFrom<u8> for S7TransportSize {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        use S7TransportSize::*;
        Ok(match v {
            0x01 => Bit,
            0x02 => Byte,
            0x03 => Char,
            0x04 => Word,
            0x05 => Int,
            0x06 => DWord,
            0x07 => DInt,
            0x08 => Real,
            0x09 => Date,
            0x0A => TimeOfDay,
            0x0B => Time,
            0x0C => S5Time,
            0x0F => DateTime,
            0x10 => DateTimeLong,
            0x1C => Counter,
            0x1D => Timer,
            0x1E => IECTimer,
            0x1F => IECEvent,
            0x20 => HSCounter,
            0x00 => String,
            0xFF => WString,
            _ => Err(())?,
        })
    }
}

impl S7TransportSize {
    /// Return the default number of bytes per element for this transport size.
    ///
    /// Notes
    /// - Bit is treated as 1 byte here as a safe upper bound when planning/fragmenting.
    /// - String/WString represent full envelope sizes when addressed via S7Any on wire:
    ///   STRING: 256 bytes ([max: u8][len: u8] + 254 payload)
    ///   WSTRING: 512 bytes ([max: u16][len: u16] + 254*2 payload)
    /// - DateTimeLong is 3 bytes per element following PLC4X mapping.
    #[inline]
    pub fn element_bytes(self) -> usize {
        match self {
            S7TransportSize::Bit => 1,
            S7TransportSize::Byte | S7TransportSize::Char => 1,
            S7TransportSize::Word
            | S7TransportSize::Int
            | S7TransportSize::Date
            | S7TransportSize::S5Time
            | S7TransportSize::Counter
            | S7TransportSize::Timer
            | S7TransportSize::IECTimer
            | S7TransportSize::IECEvent => 2,
            S7TransportSize::DateTimeLong => 3,
            S7TransportSize::String => 256,
            S7TransportSize::WString => 512,
            S7TransportSize::DWord
            | S7TransportSize::DInt
            | S7TransportSize::Real
            | S7TransportSize::TimeOfDay
            | S7TransportSize::Time
            | S7TransportSize::HSCounter => 4,
            S7TransportSize::DateTime => 8,
        }
    }
}

/// Return code present in AckData data items
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7ReturnCode {
    Reserved = 0x00,
    Success = 0xFF,
    HardwareFault = 0x01,
    AccessDenied = 0x03,
    AddressOutOfRange = 0x05,
    DataTypeNotSupported = 0x06,
    DataTypeInconsistent = 0x07,
    ObjectDoesNotExist = 0x0A,
    ObjectNotAvailable = 0x0B,
    Unknown(u8),
}

impl TryFrom<u8> for S7ReturnCode {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        Ok(match v {
            0x00 => S7ReturnCode::Reserved,
            0xFF => S7ReturnCode::Success,
            0x01 => S7ReturnCode::HardwareFault,
            0x03 => S7ReturnCode::AccessDenied,
            0x05 => S7ReturnCode::AddressOutOfRange,
            0x06 => S7ReturnCode::DataTypeNotSupported,
            0x07 => S7ReturnCode::DataTypeInconsistent,
            0x0A => S7ReturnCode::ObjectDoesNotExist,
            0x0B => S7ReturnCode::ObjectNotAvailable,
            other => S7ReturnCode::Unknown(other),
        })
    }
}

impl From<S7ReturnCode> for u8 {
    fn from(value: S7ReturnCode) -> Self {
        match value {
            S7ReturnCode::Reserved => 0x00,
            S7ReturnCode::Success => 0xFF,
            S7ReturnCode::HardwareFault => 0x01,
            S7ReturnCode::AccessDenied => 0x03,
            S7ReturnCode::AddressOutOfRange => 0x05,
            S7ReturnCode::DataTypeNotSupported => 0x06,
            S7ReturnCode::DataTypeInconsistent => 0x07,
            S7ReturnCode::ObjectDoesNotExist => 0x0A,
            S7ReturnCode::ObjectNotAvailable => 0x0B,
            S7ReturnCode::Unknown(v) => v,
        }
    }
}

/// Return data variable type used in AckData items (aligned with Java EDataVariableType)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S7DataVariableType {
    /// No data
    Null = 0x00,
    /// Bit access, length field semantics: bytes
    Bit = 0x03,
    /// Byte/Word/DWord access, length field semantics: bits
    ByteWordDWord = 0x04,
    /// Integer access, length field semantics: bits
    Integer = 0x05,
    /// DInteger access, length field semantics: bytes
    DInteger = 0x06,
    /// Real access, length field semantics: bytes
    Real = 0x07,
    /// Octet string, length field semantics: bytes
    OctetString = 0x09,
}

impl TryFrom<u8> for S7DataVariableType {
    type Error = ();
    fn try_from(v: u8) -> std::result::Result<Self, Self::Error> {
        Ok(match v {
            0x00 => S7DataVariableType::Null,
            0x03 => S7DataVariableType::Bit,
            0x04 => S7DataVariableType::ByteWordDWord,
            0x05 => S7DataVariableType::Integer,
            0x06 => S7DataVariableType::DInteger,
            0x07 => S7DataVariableType::Real,
            0x09 => S7DataVariableType::OctetString,
            _ => Err(())?,
        })
    }
}

/// Enum for CPU Function Group discriminator with semantic names and an Unknown variant
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFunctionGroup {
    /// Mode-transition
    ModeTransition = 0x00,
    /// Programmer commands
    ProgrammerCommands = 0x01,
    /// Cyclic services
    CyclicServices = 0x02,
    /// Block Functions
    BlockFunctions = 0x03,
    /// CPU functions
    CpuFunctions = 0x04,
    /// Security Functions
    SecurityFunctions = 0x05,
    /// Time functions
    TimeFunctions = 0x07,
}

impl TryFrom<u8> for CpuFunctionGroup {
    type Error = ();
    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            0x00 => Ok(CpuFunctionGroup::ModeTransition),
            0x01 => Ok(CpuFunctionGroup::ProgrammerCommands),
            0x02 => Ok(CpuFunctionGroup::CyclicServices),
            0x03 => Ok(CpuFunctionGroup::BlockFunctions),
            0x04 => Ok(CpuFunctionGroup::CpuFunctions),
            0x05 => Ok(CpuFunctionGroup::SecurityFunctions),
            0x07 => Ok(CpuFunctionGroup::TimeFunctions),
            _ => Err(())?,
        }
    }
}

/// Enum for CPU Function Type discriminator with semantic names and an Unknown variant
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFunctionType {
    /// Type 0x00: Indication/Push
    IndicationPush = 0x00,
    /// Type 0x04: Request
    Request = 0x04,
    /// Type 0x08: Response
    Response = 0x08,
}

impl TryFrom<u8> for CpuFunctionType {
    type Error = ();
    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            0x00 => Ok(CpuFunctionType::IndicationPush),
            0x04 => Ok(CpuFunctionType::Request),
            0x08 => Ok(CpuFunctionType::Response),
            _ => Err(())?,
        }
    }
}

// ===== Helpers for S7 typed encode/decode =====

/// Convert Latin-1 bytes to Rust `String` by widening each byte to Unicode scalar
pub(crate) fn latin1_bytes_to_string(input: &[u8]) -> String {
    input.iter().map(|&b| b as char).collect()
}

/// Convert one BCD-encoded byte to decimal 0..=99
pub(crate) fn bcd_to_dec(b: u8) -> u8 {
    ((b >> 4) & 0x0F) * 10 + (b & 0x0F)
}

/// Convert S7 DATE (days since 1990-01-01) to `NaiveDate`
pub(crate) fn s7_date_to_naive_date(days: u16) -> Option<NaiveDate> {
    let base = NaiveDate::from_ymd_opt(1990, 1, 1)?;
    base.checked_add_signed(Duration::days(days as i64))
}

/// Convert S7 TimeOfDay (milliseconds since midnight) to `NaiveTime`
pub(crate) fn s7_tod_to_naive_time(millis: u32) -> Option<NaiveTime> {
    let secs = millis / 1000;
    let nanos = (millis % 1000) * 1_000_000;
    NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
}

/// Decode S5TIME/TIMER raw 16-bit into `Duration`
pub(crate) fn s5time_to_duration(raw: u16) -> Duration {
    let time_base = ((raw >> 12) & 0x0003) as u32;
    let bcd_value = (raw & 0x0FFF) as u32;
    // BCD to decimal (3 nibbles -> 0..999)
    let mut value = 0u32;
    let mut factor = 1u32;
    let mut tmp = bcd_value;
    for _ in 0..3 {
        let digit = tmp & 0xF;
        value += digit * factor;
        factor *= 10;
        tmp >>= 4;
    }
    let mult = match time_base {
        0 => 10u32,
        1 => 100u32,
        2 => 1000u32,
        3 => 10000u32,
        _ => 10u32,
    };
    Duration::milliseconds((value.saturating_mul(mult)) as i64)
}

/// Encode `Duration` into S5TIME/TIMER raw 16-bit using best-fit base and clamping to 0..999 units
pub(crate) fn s5time_from_duration(dur: Duration) -> u16 {
    let total_ms = dur.num_milliseconds().max(0) as u64;
    // Prefer smallest base that fits into <=999 units, else clamp at largest base
    const BASES: &[(u16, u64)] = &[(0, 10), (1, 100), (2, 1000), (3, 10_000)];
    let mut base_sel = BASES[0];
    for &(code, step) in BASES {
        if total_ms / step <= 999 {
            base_sel = (code, step);
            break;
        }
        base_sel = (code, step);
    }
    let units = (total_ms / base_sel.1).min(999) as u16;
    // Encode 3 BCD digits into low 12 bits
    let hundreds = (units / 100) % 10;
    let tens = (units / 10) % 10;
    let ones = units % 10;
    let bcd = (hundreds << 8) | (tens << 4) | ones;
    (base_sel.0 << 12) | bcd
}

/// Decode 8-byte S7 DateTime (BCD) into `NaiveDateTime`
pub(crate) fn decode_datetime8(bytes: &[u8]) -> Option<NaiveDateTime> {
    if bytes.len() < 8 {
        return None;
    }
    let yy = bcd_to_dec(bytes[0]) as i32;
    let year = if yy < 90 { 2000 + yy } else { 1900 + yy };
    let month = bcd_to_dec(bytes[1]) as u32;
    let day = bcd_to_dec(bytes[2]) as u32;
    let hour = bcd_to_dec(bytes[3]) as u32;
    let minute = bcd_to_dec(bytes[4]) as u32;
    let second = bcd_to_dec(bytes[5]) as u32;
    let ms_low = bcd_to_dec(bytes[6]) as u32; // 00..99
    let hundreds = ((bytes[7] >> 4) & 0x0F) as u32; // 0..9
    let millis = hundreds * 100 + ms_low; // 0..999
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    date.and_hms_milli_opt(hour, minute, second, millis)
}

/// Decode 12-byte S7 DTL into `NaiveDateTime`
pub(crate) fn decode_dtl12(bytes: &[u8]) -> Option<NaiveDateTime> {
    if bytes.len() < 12 {
        return None;
    }
    let year = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i32;
    let month = bytes[4] as u32;
    let day = bytes[5] as u32;
    let hour = bytes[6] as u32;
    let minute = bytes[7] as u32;
    let second = bytes[8] as u32;
    let nanosecond = u32::from_be_bytes([0, bytes[9], bytes[10], bytes[11]]);
    if !(1970..=2262).contains(&year)
        || month == 0
        || month > 12
        || day == 0
        || day > 31
        || hour > 23
        || minute > 59
        || second > 59
        || nanosecond > 999_999_999
    {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    date.and_hms_nano_opt(hour, minute, second, nanosecond)
}
