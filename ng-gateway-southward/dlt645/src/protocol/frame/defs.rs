use crate::types::Dl645Version;

use serde::{Deserialize, Serialize};

/// DL/T 645 supported baud rates for update‑baud‑rate commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dl645BaudRate {
    /// 600 bps.
    Bps600,
    /// 1200 bps.
    Bps1200,
    /// 2400 bps.
    Bps2400,
    /// 4800 bps.
    Bps4800,
    /// 9600 bps.
    Bps9600,
    /// 19200 bps.
    Bps19200,
}

impl Dl645BaudRate {
    /// Return the baud rate in bits per second as a plain integer.
    pub fn as_u32(self) -> u32 {
        match self {
            Dl645BaudRate::Bps600 => 600,
            Dl645BaudRate::Bps1200 => 1200,
            Dl645BaudRate::Bps2400 => 2400,
            Dl645BaudRate::Bps4800 => 4800,
            Dl645BaudRate::Bps9600 => 9600,
            Dl645BaudRate::Bps19200 => 19200,
        }
    }

    /// Map this baud rate to the DL/T 645 Z characteristic byte (without 0x33 offset).
    ///
    /// The encoding differs between V1997 and V2007.
    ///
    /// V1997 Encoding (inferred from Simulator behavior and common implementations):
    /// - 0x02 -> 600
    /// - 0x04 -> 1200
    /// - 0x08 -> 2400
    /// - 0x10 -> 4800
    /// - 0x20 -> 9600
    /// - 0x40 -> 19200 (Unofficial extension often used)
    ///
    /// V2007 Encoding (Standard):
    /// - 0x01 -> 600 (Bit 0)
    /// - 0x02 -> 1200 (Bit 1)
    /// - 0x04 -> 2400 (Bit 2)
    /// - 0x08 -> 4800 (Bit 3)
    /// - 0x10 -> 9600 (Bit 4)
    /// - 0x20 -> 19200 (Bit 5)
    pub fn as_code(self, version: Dl645Version) -> u8 {
        match version {
            Dl645Version::V1997 => match self {
                Dl645BaudRate::Bps600 => 0x04,
                Dl645BaudRate::Bps1200 => 0x08,
                Dl645BaudRate::Bps2400 => 0x10,
                Dl645BaudRate::Bps4800 => 0x20,
                Dl645BaudRate::Bps9600 => 0x40,
                // 19200 not strictly standard in 1997 but often mapped as next bit
                Dl645BaudRate::Bps19200 => 0x80,
            },
            _ => match self {
                Dl645BaudRate::Bps600 => 0x01,
                Dl645BaudRate::Bps1200 => 0x02,
                Dl645BaudRate::Bps2400 => 0x04,
                Dl645BaudRate::Bps4800 => 0x08,
                Dl645BaudRate::Bps9600 => 0x10,
                Dl645BaudRate::Bps19200 => 0x20,
            },
        }
    }
}

impl From<Dl645BaudRate> for u32 {
    fn from(rate: Dl645BaudRate) -> Self {
        rate.as_u32()
    }
}

impl TryFrom<u32> for Dl645BaudRate {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            600 => Ok(Dl645BaudRate::Bps600),
            1200 => Ok(Dl645BaudRate::Bps1200),
            2400 => Ok(Dl645BaudRate::Bps2400),
            4800 => Ok(Dl645BaudRate::Bps4800),
            9600 => Ok(Dl645BaudRate::Bps9600),
            19200 => Ok(Dl645BaudRate::Bps19200),
            _ => Err(()),
        }
    }
}

/// DL/T 645 Address (6 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct Dl645Address(pub [u8; 6]);

impl Dl645Address {
    pub fn new(addr: [u8; 6]) -> Self {
        Self(addr)
    }

    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

/// DL/T645 transfer direction derived from bit D7 of the control word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dl645Direction {
    /// Master station sends a command frame (D7 = 0).
    MasterToSlave,
    /// Slave station sends a response frame (D7 = 1).
    SlaveToMaster,
}

/// DL/T645 function code encoded in bits D0..D4 of the control word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dl645Function {
    /// 00000: Reserved.
    Reserved,
    /// 01000: Broadcast time synchronization.
    BroadcastTimeSync,
    /// 10001: Read data.
    ReadData,
    /// 10010: Read subsequent data.
    ReadNextData,
    /// 00011: Re-read data (V1997).
    ReReadData,
    /// 10011: Read communication address.
    ReadAddress,
    /// 10100: Write data.
    WriteData,
    /// 10101: Write communication address.
    WriteAddress,
    /// 10110: Freeze command.
    Freeze,
    /// 10111: Update baud rate.
    UpdateBaudRate,
    /// 11000: Modify password.
    ModifyPassword,
    /// 11001: Maximum demand clear.
    ClearMaxDemand,
    /// 11010: Meter clear.
    ClearMeter,
    /// 11011: Event clear.
    ClearEvents,
    /// Any other pattern not explicitly defined by the standard.
    Unknown(u8),
}

impl Dl645Function {
    /// Decode a 5-bit value (D0..D4) into a `Dl645Function`.
    /// Note: This handles both V1997 and V2007 mappings where they don't conflict.
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x1F {
            0b00000 => Dl645Function::Reserved,
            0b00001 => Dl645Function::ReadData, // V1997
            0b10001 => Dl645Function::ReadData, // V2007

            0b00010 => Dl645Function::ReadNextData, // V1997
            0b10010 => Dl645Function::ReadNextData, // V2007

            0b00011 => Dl645Function::ReReadData, // V1997

            0b01000 => Dl645Function::BroadcastTimeSync,

            0b10011 => Dl645Function::ReadAddress, // V2007

            0b00100 => Dl645Function::WriteData, // V1997
            0b10100 => Dl645Function::WriteData, // V2007

            0b01010 => Dl645Function::WriteAddress, // V1997
            0b10101 => Dl645Function::WriteAddress, // V2007

            0b10110 => Dl645Function::Freeze, // V2007

            0b01100 => Dl645Function::UpdateBaudRate, // V1997
            0b10111 => Dl645Function::UpdateBaudRate, // V2007

            0b01111 => Dl645Function::ModifyPassword, // V1997
            0b11000 => Dl645Function::ModifyPassword, // V2007

            0b10000 => Dl645Function::ClearMaxDemand, // V1997
            0b11001 => Dl645Function::ClearMaxDemand, // V2007

            0b11010 => Dl645Function::ClearMeter,  // V2007
            0b11011 => Dl645Function::ClearEvents, // V2007

            other => Dl645Function::Unknown(other),
        }
    }

    /// Encode this function into the low 5 bits D0..D4 (V2007 Standard).
    pub fn to_bits(self) -> u8 {
        match self {
            Dl645Function::Reserved => 0b00000,
            Dl645Function::BroadcastTimeSync => 0b01000,
            Dl645Function::ReadData => 0b10001,
            Dl645Function::ReadNextData => 0b10010,
            Dl645Function::ReReadData => 0b00011, // Unique to V1997, but providing a mapping
            Dl645Function::ReadAddress => 0b10011,
            Dl645Function::WriteData => 0b10100,
            Dl645Function::WriteAddress => 0b10101,
            Dl645Function::Freeze => 0b10110,
            Dl645Function::UpdateBaudRate => 0b10111,
            Dl645Function::ModifyPassword => 0b11000,
            Dl645Function::ClearMaxDemand => 0b11001,
            Dl645Function::ClearMeter => 0b11010,
            Dl645Function::ClearEvents => 0b11011,
            Dl645Function::Unknown(bits) => bits & 0x1F,
        }
    }
}

/// Bit-level view over the 8-bit DL/T645 control word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dl645ControlWord {
    /// Raw control byte exactly as carried on the wire.
    pub raw: u8,
}

impl Dl645ControlWord {
    /// Create a new control word wrapper from a raw byte.
    pub fn new(raw: u8) -> Self {
        Self { raw }
    }

    /// Construct a master‑to‑slave request control word from a logical function code (Default V2007).
    pub fn from_function(function: Dl645Function) -> Self {
        Self::new(function.to_bits())
    }

    /// Build a request control word handling protocol version differences.
    pub fn for_request(version: Dl645Version, function: Dl645Function) -> Self {
        match version {
            Dl645Version::V1997 => match function {
                Dl645Function::ReadData => Dl645ControlWord::new(0b00001), // 01
                Dl645Function::ReadNextData => Dl645ControlWord::new(0b00010), // 02
                Dl645Function::ReReadData => Dl645ControlWord::new(0b00011), // 03
                Dl645Function::WriteData => Dl645ControlWord::new(0b00100), // 04
                Dl645Function::BroadcastTimeSync => Dl645ControlWord::new(0b01000), // 08
                Dl645Function::WriteAddress => Dl645ControlWord::new(0b01010), // 0A
                Dl645Function::UpdateBaudRate => Dl645ControlWord::new(0b01100), // 0C
                Dl645Function::ModifyPassword => Dl645ControlWord::new(0b01111), // 0F
                Dl645Function::ClearMaxDemand => Dl645ControlWord::new(0b10000), // 10
                // For other functions not defined in V1997, fall back to default (V2007) bits.
                // This includes things like Freeze, ClearMeter, ClearEvents which are V2007 specific or not listed.
                other => Dl645ControlWord::from_function(other),
            },
            _ => Dl645ControlWord::from_function(function),
        }
    }

    /// Extract the low 5-bit function field (D0..D4).
    pub fn function_bits(&self) -> u8 {
        self.raw & 0x1F
    }

    /// Interpret the low 5 bits as a DL/T645 function code.
    pub fn function(&self) -> Dl645Function {
        Dl645Function::from_bits(self.function_bits())
    }

    /// Check whether the "following frame" flag (D5) is set.
    pub fn has_following_frame(&self) -> bool {
        (self.raw & 0x20) != 0
    }

    /// Check whether this response represents an exception (D6 = 1).
    pub fn is_exception_response(&self) -> bool {
        (self.raw & 0x40) != 0
    }

    /// Derive the transfer direction (D7) from the control word.
    pub fn direction(&self) -> Dl645Direction {
        if (self.raw & 0x80) != 0 {
            Dl645Direction::SlaveToMaster
        } else {
            Dl645Direction::MasterToSlave
        }
    }
}
