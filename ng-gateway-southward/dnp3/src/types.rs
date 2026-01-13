use ng_gateway_sdk::{
    AccessMode, CollectionType, ConnectionPolicy, DataPointType, DataType, DriverConfig,
    ReportType, RuntimeAction, RuntimeChannel, RuntimeDevice, RuntimeParameter, RuntimePoint,
    Status,
};

use dnp3::app::control::{
    ControlCode as Dnp3ControlCode, OpType as Dnp3OpType, TripCloseCode as Dnp3TripCloseCode,
};
use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::sync::Arc;

/// DNP3 CROB Trip/Close Code (TCC) subset supported by the gateway.
///
/// We intentionally **exclude** `Reserved` to keep runtime configuration explicit and safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TripCloseCode {
    /// Not specified.
    Nul,
    /// Close output.
    Close,
    /// Trip output.
    Trip,
}

impl TripCloseCode {
    /// Convert to the corresponding `dnp3-rs` TCC representation.
    #[inline]
    pub const fn to_dnp3(self) -> Dnp3TripCloseCode {
        match self {
            TripCloseCode::Nul => Dnp3TripCloseCode::Nul,
            TripCloseCode::Close => Dnp3TripCloseCode::Close,
            TripCloseCode::Trip => Dnp3TripCloseCode::Trip,
        }
    }

    #[inline]
    const fn from_tcc_bits(bits: u8) -> Result<Self, ControlCodeParseError> {
        match bits {
            0 => Ok(TripCloseCode::Nul),
            1 => Ok(TripCloseCode::Close),
            2 => Ok(TripCloseCode::Trip),
            3 => Err(ControlCodeParseError::ReservedTcc),
            _ => Err(ControlCodeParseError::InvalidTcc(bits)),
        }
    }
}

/// DNP3 CROB operation type subset supported by the gateway.
///
/// We intentionally **exclude** `Nul` and unknown values to keep control explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CrobOpType {
    /// Pulse the output on.
    PulseOn,
    /// Pulse the output off.
    PulseOff,
    /// Latch the output on.
    LatchOn,
    /// Latch the output off.
    LatchOff,
}

impl CrobOpType {
    /// Convert to the corresponding `dnp3-rs` operation type representation.
    #[inline]
    pub const fn to_dnp3(self) -> Dnp3OpType {
        match self {
            CrobOpType::PulseOn => Dnp3OpType::PulseOn,
            CrobOpType::PulseOff => Dnp3OpType::PulseOff,
            CrobOpType::LatchOn => Dnp3OpType::LatchOn,
            CrobOpType::LatchOff => Dnp3OpType::LatchOff,
        }
    }

    #[inline]
    const fn from_op_bits(bits: u8) -> Result<Self, ControlCodeParseError> {
        match bits {
            1 => Ok(CrobOpType::PulseOn),
            2 => Ok(CrobOpType::PulseOff),
            3 => Ok(CrobOpType::LatchOn),
            4 => Ok(CrobOpType::LatchOff),
            0 => Err(ControlCodeParseError::NulOpType),
            _ => Err(ControlCodeParseError::InvalidOpType(bits)),
        }
    }
}

/// Strongly-typed CROB control code for WritePoint and Action inputs.
///
/// # Why this enum exists
/// The upstream `dnp3-rs` `ControlCode` is a bitfield-like struct. For gateway users, a fully
/// enumerated semantic set avoids ambiguous/unsafe combinations and makes validation deterministic.
///
/// # Encoding
/// This enum maps to the g12v1 "control code" byte:
/// - bits 7..6: `TripCloseCode` (TCC)
/// - bit 5: `clear`
/// - bit 4: `queue` (obsolete in the standard but still representable)
/// - bits 3..0: `CrobOpType` (OpType)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlCode {
    PulseOnNul,
    PulseOnNulQueue,
    PulseOnNulClear,
    PulseOnNulQueueClear,
    PulseOnClose,
    PulseOnCloseQueue,
    PulseOnCloseClear,
    PulseOnCloseQueueClear,
    PulseOnTrip,
    PulseOnTripQueue,
    PulseOnTripClear,
    PulseOnTripQueueClear,

    PulseOffNul,
    PulseOffNulQueue,
    PulseOffNulClear,
    PulseOffNulQueueClear,
    PulseOffClose,
    PulseOffCloseQueue,
    PulseOffCloseClear,
    PulseOffCloseQueueClear,
    PulseOffTrip,
    PulseOffTripQueue,
    PulseOffTripClear,
    PulseOffTripQueueClear,

    LatchOnNul,
    LatchOnNulQueue,
    LatchOnNulClear,
    LatchOnNulQueueClear,
    LatchOnClose,
    LatchOnCloseQueue,
    LatchOnCloseClear,
    LatchOnCloseQueueClear,
    LatchOnTrip,
    LatchOnTripQueue,
    LatchOnTripClear,
    LatchOnTripQueueClear,

    LatchOffNul,
    LatchOffNulQueue,
    LatchOffNulClear,
    LatchOffNulQueueClear,
    LatchOffClose,
    LatchOffCloseQueue,
    LatchOffCloseClear,
    LatchOffCloseQueueClear,
    LatchOffTrip,
    LatchOffTripQueue,
    LatchOffTripClear,
    LatchOffTripQueueClear,
}

impl ControlCode {
    const TCC_MASK: u8 = 0b1100_0000;
    const CLEAR_MASK: u8 = 0b0010_0000;
    const QUEUE_MASK: u8 = 0b0001_0000;
    const OP_MASK: u8 = 0b0000_1111;

    /// Return the semantic constituents of this control code.
    #[inline]
    pub const fn components(self) -> (TripCloseCode, CrobOpType, bool, bool) {
        match self {
            ControlCode::PulseOnNul => (TripCloseCode::Nul, CrobOpType::PulseOn, false, false),
            ControlCode::PulseOnNulQueue => (TripCloseCode::Nul, CrobOpType::PulseOn, true, false),
            ControlCode::PulseOnNulClear => (TripCloseCode::Nul, CrobOpType::PulseOn, false, true),
            ControlCode::PulseOnNulQueueClear => {
                (TripCloseCode::Nul, CrobOpType::PulseOn, true, true)
            }
            ControlCode::PulseOnClose => (TripCloseCode::Close, CrobOpType::PulseOn, false, false),
            ControlCode::PulseOnCloseQueue => {
                (TripCloseCode::Close, CrobOpType::PulseOn, true, false)
            }
            ControlCode::PulseOnCloseClear => {
                (TripCloseCode::Close, CrobOpType::PulseOn, false, true)
            }
            ControlCode::PulseOnCloseQueueClear => {
                (TripCloseCode::Close, CrobOpType::PulseOn, true, true)
            }
            ControlCode::PulseOnTrip => (TripCloseCode::Trip, CrobOpType::PulseOn, false, false),
            ControlCode::PulseOnTripQueue => {
                (TripCloseCode::Trip, CrobOpType::PulseOn, true, false)
            }
            ControlCode::PulseOnTripClear => {
                (TripCloseCode::Trip, CrobOpType::PulseOn, false, true)
            }
            ControlCode::PulseOnTripQueueClear => {
                (TripCloseCode::Trip, CrobOpType::PulseOn, true, true)
            }

            ControlCode::PulseOffNul => (TripCloseCode::Nul, CrobOpType::PulseOff, false, false),
            ControlCode::PulseOffNulQueue => {
                (TripCloseCode::Nul, CrobOpType::PulseOff, true, false)
            }
            ControlCode::PulseOffNulClear => {
                (TripCloseCode::Nul, CrobOpType::PulseOff, false, true)
            }
            ControlCode::PulseOffNulQueueClear => {
                (TripCloseCode::Nul, CrobOpType::PulseOff, true, true)
            }
            ControlCode::PulseOffClose => {
                (TripCloseCode::Close, CrobOpType::PulseOff, false, false)
            }
            ControlCode::PulseOffCloseQueue => {
                (TripCloseCode::Close, CrobOpType::PulseOff, true, false)
            }
            ControlCode::PulseOffCloseClear => {
                (TripCloseCode::Close, CrobOpType::PulseOff, false, true)
            }
            ControlCode::PulseOffCloseQueueClear => {
                (TripCloseCode::Close, CrobOpType::PulseOff, true, true)
            }
            ControlCode::PulseOffTrip => (TripCloseCode::Trip, CrobOpType::PulseOff, false, false),
            ControlCode::PulseOffTripQueue => {
                (TripCloseCode::Trip, CrobOpType::PulseOff, true, false)
            }
            ControlCode::PulseOffTripClear => {
                (TripCloseCode::Trip, CrobOpType::PulseOff, false, true)
            }
            ControlCode::PulseOffTripQueueClear => {
                (TripCloseCode::Trip, CrobOpType::PulseOff, true, true)
            }

            ControlCode::LatchOnNul => (TripCloseCode::Nul, CrobOpType::LatchOn, false, false),
            ControlCode::LatchOnNulQueue => (TripCloseCode::Nul, CrobOpType::LatchOn, true, false),
            ControlCode::LatchOnNulClear => (TripCloseCode::Nul, CrobOpType::LatchOn, false, true),
            ControlCode::LatchOnNulQueueClear => {
                (TripCloseCode::Nul, CrobOpType::LatchOn, true, true)
            }
            ControlCode::LatchOnClose => (TripCloseCode::Close, CrobOpType::LatchOn, false, false),
            ControlCode::LatchOnCloseQueue => {
                (TripCloseCode::Close, CrobOpType::LatchOn, true, false)
            }
            ControlCode::LatchOnCloseClear => {
                (TripCloseCode::Close, CrobOpType::LatchOn, false, true)
            }
            ControlCode::LatchOnCloseQueueClear => {
                (TripCloseCode::Close, CrobOpType::LatchOn, true, true)
            }
            ControlCode::LatchOnTrip => (TripCloseCode::Trip, CrobOpType::LatchOn, false, false),
            ControlCode::LatchOnTripQueue => {
                (TripCloseCode::Trip, CrobOpType::LatchOn, true, false)
            }
            ControlCode::LatchOnTripClear => {
                (TripCloseCode::Trip, CrobOpType::LatchOn, false, true)
            }
            ControlCode::LatchOnTripQueueClear => {
                (TripCloseCode::Trip, CrobOpType::LatchOn, true, true)
            }

            ControlCode::LatchOffNul => (TripCloseCode::Nul, CrobOpType::LatchOff, false, false),
            ControlCode::LatchOffNulQueue => {
                (TripCloseCode::Nul, CrobOpType::LatchOff, true, false)
            }
            ControlCode::LatchOffNulClear => {
                (TripCloseCode::Nul, CrobOpType::LatchOff, false, true)
            }
            ControlCode::LatchOffNulQueueClear => {
                (TripCloseCode::Nul, CrobOpType::LatchOff, true, true)
            }
            ControlCode::LatchOffClose => {
                (TripCloseCode::Close, CrobOpType::LatchOff, false, false)
            }
            ControlCode::LatchOffCloseQueue => {
                (TripCloseCode::Close, CrobOpType::LatchOff, true, false)
            }
            ControlCode::LatchOffCloseClear => {
                (TripCloseCode::Close, CrobOpType::LatchOff, false, true)
            }
            ControlCode::LatchOffCloseQueueClear => {
                (TripCloseCode::Close, CrobOpType::LatchOff, true, true)
            }
            ControlCode::LatchOffTrip => (TripCloseCode::Trip, CrobOpType::LatchOff, false, false),
            ControlCode::LatchOffTripQueue => {
                (TripCloseCode::Trip, CrobOpType::LatchOff, true, false)
            }
            ControlCode::LatchOffTripClear => {
                (TripCloseCode::Trip, CrobOpType::LatchOff, false, true)
            }
            ControlCode::LatchOffTripQueueClear => {
                (TripCloseCode::Trip, CrobOpType::LatchOff, true, true)
            }
        }
    }

    /// Convert this strongly-typed code to the upstream `dnp3-rs` representation.
    #[inline]
    pub const fn to_dnp3(self) -> Dnp3ControlCode {
        let (tcc, op_type, queue, clear) = self.components();
        Dnp3ControlCode {
            tcc: tcc.to_dnp3(),
            clear,
            queue,
            op_type: op_type.to_dnp3(),
        }
    }

    /// Convert this code to its on-the-wire `u8` representation.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        let (tcc, op_type, queue, clear) = self.components();
        let mut x = 0u8;

        // TCC is encoded in the top two bits (7..6).
        x |= match tcc {
            TripCloseCode::Nul => 0,
            TripCloseCode::Close => 1,
            TripCloseCode::Trip => 2,
        } << 6;

        if clear {
            x |= Self::CLEAR_MASK;
        }
        if queue {
            x |= Self::QUEUE_MASK;
        }

        // OpType is encoded in the low nibble (3..0).
        x |= match op_type {
            CrobOpType::PulseOn => 1,
            CrobOpType::PulseOff => 2,
            CrobOpType::LatchOn => 3,
            CrobOpType::LatchOff => 4,
        };

        x
    }

    #[inline]
    const fn from_components(
        tcc: TripCloseCode,
        op_type: CrobOpType,
        queue: bool,
        clear: bool,
    ) -> Self {
        match (op_type, tcc, queue, clear) {
            (CrobOpType::PulseOn, TripCloseCode::Nul, false, false) => ControlCode::PulseOnNul,
            (CrobOpType::PulseOn, TripCloseCode::Nul, true, false) => ControlCode::PulseOnNulQueue,
            (CrobOpType::PulseOn, TripCloseCode::Nul, false, true) => ControlCode::PulseOnNulClear,
            (CrobOpType::PulseOn, TripCloseCode::Nul, true, true) => {
                ControlCode::PulseOnNulQueueClear
            }
            (CrobOpType::PulseOn, TripCloseCode::Close, false, false) => ControlCode::PulseOnClose,
            (CrobOpType::PulseOn, TripCloseCode::Close, true, false) => {
                ControlCode::PulseOnCloseQueue
            }
            (CrobOpType::PulseOn, TripCloseCode::Close, false, true) => {
                ControlCode::PulseOnCloseClear
            }
            (CrobOpType::PulseOn, TripCloseCode::Close, true, true) => {
                ControlCode::PulseOnCloseQueueClear
            }
            (CrobOpType::PulseOn, TripCloseCode::Trip, false, false) => ControlCode::PulseOnTrip,
            (CrobOpType::PulseOn, TripCloseCode::Trip, true, false) => {
                ControlCode::PulseOnTripQueue
            }
            (CrobOpType::PulseOn, TripCloseCode::Trip, false, true) => {
                ControlCode::PulseOnTripClear
            }
            (CrobOpType::PulseOn, TripCloseCode::Trip, true, true) => {
                ControlCode::PulseOnTripQueueClear
            }

            (CrobOpType::PulseOff, TripCloseCode::Nul, false, false) => ControlCode::PulseOffNul,
            (CrobOpType::PulseOff, TripCloseCode::Nul, true, false) => {
                ControlCode::PulseOffNulQueue
            }
            (CrobOpType::PulseOff, TripCloseCode::Nul, false, true) => {
                ControlCode::PulseOffNulClear
            }
            (CrobOpType::PulseOff, TripCloseCode::Nul, true, true) => {
                ControlCode::PulseOffNulQueueClear
            }
            (CrobOpType::PulseOff, TripCloseCode::Close, false, false) => {
                ControlCode::PulseOffClose
            }
            (CrobOpType::PulseOff, TripCloseCode::Close, true, false) => {
                ControlCode::PulseOffCloseQueue
            }
            (CrobOpType::PulseOff, TripCloseCode::Close, false, true) => {
                ControlCode::PulseOffCloseClear
            }
            (CrobOpType::PulseOff, TripCloseCode::Close, true, true) => {
                ControlCode::PulseOffCloseQueueClear
            }
            (CrobOpType::PulseOff, TripCloseCode::Trip, false, false) => ControlCode::PulseOffTrip,
            (CrobOpType::PulseOff, TripCloseCode::Trip, true, false) => {
                ControlCode::PulseOffTripQueue
            }
            (CrobOpType::PulseOff, TripCloseCode::Trip, false, true) => {
                ControlCode::PulseOffTripClear
            }
            (CrobOpType::PulseOff, TripCloseCode::Trip, true, true) => {
                ControlCode::PulseOffTripQueueClear
            }

            (CrobOpType::LatchOn, TripCloseCode::Nul, false, false) => ControlCode::LatchOnNul,
            (CrobOpType::LatchOn, TripCloseCode::Nul, true, false) => ControlCode::LatchOnNulQueue,
            (CrobOpType::LatchOn, TripCloseCode::Nul, false, true) => ControlCode::LatchOnNulClear,
            (CrobOpType::LatchOn, TripCloseCode::Nul, true, true) => {
                ControlCode::LatchOnNulQueueClear
            }
            (CrobOpType::LatchOn, TripCloseCode::Close, false, false) => ControlCode::LatchOnClose,
            (CrobOpType::LatchOn, TripCloseCode::Close, true, false) => {
                ControlCode::LatchOnCloseQueue
            }
            (CrobOpType::LatchOn, TripCloseCode::Close, false, true) => {
                ControlCode::LatchOnCloseClear
            }
            (CrobOpType::LatchOn, TripCloseCode::Close, true, true) => {
                ControlCode::LatchOnCloseQueueClear
            }
            (CrobOpType::LatchOn, TripCloseCode::Trip, false, false) => ControlCode::LatchOnTrip,
            (CrobOpType::LatchOn, TripCloseCode::Trip, true, false) => {
                ControlCode::LatchOnTripQueue
            }
            (CrobOpType::LatchOn, TripCloseCode::Trip, false, true) => {
                ControlCode::LatchOnTripClear
            }
            (CrobOpType::LatchOn, TripCloseCode::Trip, true, true) => {
                ControlCode::LatchOnTripQueueClear
            }

            (CrobOpType::LatchOff, TripCloseCode::Nul, false, false) => ControlCode::LatchOffNul,
            (CrobOpType::LatchOff, TripCloseCode::Nul, true, false) => {
                ControlCode::LatchOffNulQueue
            }
            (CrobOpType::LatchOff, TripCloseCode::Nul, false, true) => {
                ControlCode::LatchOffNulClear
            }
            (CrobOpType::LatchOff, TripCloseCode::Nul, true, true) => {
                ControlCode::LatchOffNulQueueClear
            }
            (CrobOpType::LatchOff, TripCloseCode::Close, false, false) => {
                ControlCode::LatchOffClose
            }
            (CrobOpType::LatchOff, TripCloseCode::Close, true, false) => {
                ControlCode::LatchOffCloseQueue
            }
            (CrobOpType::LatchOff, TripCloseCode::Close, false, true) => {
                ControlCode::LatchOffCloseClear
            }
            (CrobOpType::LatchOff, TripCloseCode::Close, true, true) => {
                ControlCode::LatchOffCloseQueueClear
            }
            (CrobOpType::LatchOff, TripCloseCode::Trip, false, false) => ControlCode::LatchOffTrip,
            (CrobOpType::LatchOff, TripCloseCode::Trip, true, false) => {
                ControlCode::LatchOffTripQueue
            }
            (CrobOpType::LatchOff, TripCloseCode::Trip, false, true) => {
                ControlCode::LatchOffTripClear
            }
            (CrobOpType::LatchOff, TripCloseCode::Trip, true, true) => {
                ControlCode::LatchOffTripQueueClear
            }
        }
    }
}

impl TryFrom<u8> for ControlCode {
    type Error = ControlCodeParseError;

    #[inline]
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        let tcc_bits = (value & Self::TCC_MASK) >> 6;
        let clear = (value & Self::CLEAR_MASK) != 0;
        let queue = (value & Self::QUEUE_MASK) != 0;
        let op_bits = value & Self::OP_MASK;

        let tcc = TripCloseCode::from_tcc_bits(tcc_bits)?;
        let op_type = CrobOpType::from_op_bits(op_bits)?;
        Ok(ControlCode::from_components(tcc, op_type, queue, clear))
    }
}

/// Error returned when a CROB control code byte cannot be mapped into the gateway's `ControlCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ControlCodeParseError {
    /// TripCloseCode bits are `0b11`, which is reserved in the standard and disallowed by the gateway.
    #[error("TripCloseCode is Reserved (0b11), which is not allowed")]
    ReservedTcc,
    /// TripCloseCode bits are not a valid 2-bit value (should be unreachable but kept for defensive parsing).
    #[error("TripCloseCode bits are invalid: {0}")]
    InvalidTcc(u8),
    /// OpType is `0`, which is `Nul` in the standard and disallowed by the gateway.
    #[error("OpType is Nul (0), which is not allowed")]
    NulOpType,
    /// OpType bits are outside the gateway supported subset: 1..=4.
    #[error("OpType bits are invalid/unsupported: {0} (expected 1..=4)")]
    InvalidOpType(u8),
}

/// DNP3 Channel Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3ChannelConfig {
    /// Transport layer connection settings
    pub connection: Dnp3Connection,

    /// Master Address (Link Layer)
    pub local_addr: u16,
    /// Outstation Address (Link Layer)
    pub remote_addr: u16,

    /// Link layer response timeout (ms)
    #[serde(default = "Dnp3ChannelConfig::default_response_timeout_ms")]
    pub response_timeout_ms: u64,

    /// Class 0/1/2/3 (Integrity) scan interval (ms).
    #[serde(default = "Dnp3ChannelConfig::default_integrity_scan_ms")]
    pub integrity_scan_interval_ms: u64,

    /// Event (Class 1/2/3) scan interval (ms).
    #[serde(default = "Dnp3ChannelConfig::default_event_scan_interval_ms")]
    pub event_scan_interval_ms: u64,
}

impl Dnp3ChannelConfig {
    fn default_response_timeout_ms() -> u64 {
        5000
    }
    fn default_integrity_scan_ms() -> u64 {
        20_000
    }
    fn default_event_scan_interval_ms() -> u64 {
        1_000
    }
}

impl DriverConfig for Dnp3ChannelConfig {}

/// DNP3 Connection Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Dnp3Connection {
    /// DNP3 over TCP/IP
    Tcp { host: String, port: u16 },
    /// DNP3 over UDP/IP
    Udp {
        host: String,
        port: u16,
        #[serde(default)]
        local_port: Option<u16>,
    },
    /// DNP3 over Serial
    Serial {
        path: String,
        baud_rate: u32,
        data_bits: DataBits,
        stop_bits: StopBits,
        parity: Parity,
    },
}

/// Serial Data Bits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum DataBits {
    Five = 5,
    Six = 6,
    Seven = 7,
    Eight = 8,
}

impl From<DataBits> for tokio_serial::DataBits {
    fn from(bits: DataBits) -> Self {
        match bits {
            DataBits::Five => tokio_serial::DataBits::Five,
            DataBits::Six => tokio_serial::DataBits::Six,
            DataBits::Seven => tokio_serial::DataBits::Seven,
            DataBits::Eight => tokio_serial::DataBits::Eight,
        }
    }
}

/// Serial Stop Bits
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum StopBits {
    One = 1,
    Two = 2,
}

impl From<StopBits> for tokio_serial::StopBits {
    fn from(bits: StopBits) -> Self {
        match bits {
            StopBits::One => tokio_serial::StopBits::One,
            StopBits::Two => tokio_serial::StopBits::Two,
        }
    }
}

/// Serial Parity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
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

/// DNP3 Point Group
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Dnp3PointGroup {
    /// Group 1 - Binary Input
    BinaryInput = 1,
    /// Group 3 - Double Bit Binary Input
    DoubleBitBinaryInput = 3,
    /// Group 10 - Binary Output Status
    BinaryOutput = 10,
    /// Group 20 - Counter
    Counter = 20,
    /// Group 21 - Frozen Counter
    FrozenCounter = 21,
    /// Group 30 - Analog Input
    AnalogInput = 30,
    /// Group 40 - Analog Output Status
    AnalogOutput = 40,
    /// Group 110 - Octet String
    OctetString = 110,
}

/// DNP3 Command Type (Group)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum Dnp3CommandType {
    /// Group 12 - Control Relay Output Block (CROB)
    CROB = 12,
    /// Group 41 - Analog Output Command
    AnalogOutputCommand = 41,
    /// Warm restart of the outstation (no DNP3 object group, mapped to `AssociationHandle::warm_restart`)
    WarmRestart = 200,
    /// Cold restart of the outstation (no DNP3 object group, mapped to `AssociationHandle::cold_restart`)
    ColdRestart = 201,
}

/// DNP3 Channel Runtime Object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3Channel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub report_type: ReportType,
    pub period: Option<u32>,
    pub status: Status,
    pub connection_policy: ConnectionPolicy,
    pub config: Dnp3ChannelConfig,
}

impl RuntimeChannel for Dnp3Channel {
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

/// DNP3 Device Runtime Object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3Device {
    pub id: i32,
    pub channel_id: i32,
    pub device_name: String,
    pub device_type: String,
    pub status: Status,
}

impl RuntimeDevice for Dnp3Device {
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

/// DNP3 Point Runtime Object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3Point {
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

    // DNP3 specific
    pub group: Dnp3PointGroup,
    pub index: u16,
}

impl RuntimePoint for Dnp3Point {
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

/// DNP3 Action Runtime Object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3Action {
    pub id: i32,
    pub device_id: i32,
    pub name: String,
    pub command: String,
    pub input_parameters: Vec<Dnp3Parameter>,
}

impl RuntimeAction for Dnp3Action {
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

/// DNP3 Parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dnp3Parameter {
    pub name: String,
    pub key: String,
    pub data_type: DataType,
    pub required: bool,
    pub default_value: Option<serde_json::Value>,
    pub max_value: Option<f64>,
    pub min_value: Option<f64>,

    // DNP3 specific for control
    pub group: Dnp3CommandType,
    pub index: u16,

    /// CROB (Group 12 Var 1) repetition count.
    ///
    /// - Applies only when `group = CROB`.
    /// - If omitted, driver uses a sensible default (usually 1).
    #[serde(default)]
    pub crob_count: Option<u8>,

    /// CROB "on time" in milliseconds.
    ///
    /// - Applies only when `group = CROB`.
    /// - The underlying DNP3 master library represents this as an unsigned integer (`u32`).
    ///   Keep values reasonable for your outstation implementation.
    #[serde(default)]
    pub crob_on_time_ms: Option<u32>,

    /// CROB "off time" in milliseconds.
    ///
    /// - Applies only when `group = CROB`.
    /// - The underlying DNP3 master library represents this as an unsigned integer (`u32`).
    ///   Keep values reasonable for your outstation implementation.
    #[serde(default)]
    pub crob_off_time_ms: Option<u32>,
}

impl RuntimeParameter for Dnp3Parameter {
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

/// Runtime Metadata for Point lookup
#[derive(Debug, Clone)]
pub struct PointMeta {
    /// Point identifier (primary key).
    pub point_id: i32,
    pub key: Arc<str>,
    pub data_type: DataType,
    pub scale: Option<f64>,
    pub kind: DataPointType,
    pub device_id: i32,
    /// Device name shared across all points of the same device.
    ///
    /// Using `Arc<str>` avoids cloning the device name `String` for every single
    /// point during driver initialization and reduces allocations in hot paths.
    pub device_name: Arc<str>,
}
