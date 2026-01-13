use serde::{Deserialize, Serialize};

/// Numeric notation used by MC soft devices.
///
/// Some devices (e.g. X/Y/B) use hexadecimal addressing, others use decimal.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McNotation {
    /// Decimal addressing (base 10).
    Dec,
    /// Hexadecimal addressing (base 16).
    Hex,
}

/// MC soft device code (Mitsubishi terminology).
///
/// This enum is derived from the Java `EMcDeviceCode` definition and models
/// all commonly used bit/word/dword soft elements.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum McDeviceType {
    // Special relay/register
    SM,
    SD,
    // Inputs / outputs
    X,
    Y,
    // Internal relays
    M,
    L,
    F,
    V,
    // Link relays/registers
    B,
    // Data register
    D,
    W,
    // Timers / counters (contacts/coils/current values)
    TS,
    TC,
    TN,
    LTS,
    LTC,
    LTN,
    STS,
    STC,
    STN,
    LSTS,
    LSTC,
    LSTN,
    CS,
    CC,
    CN,
    LCS,
    LCC,
    LCN,
    // Link special relay/register
    SB,
    SW,
    // Direct access
    DX,
    DY,
    // Index registers
    Z,
    LZ,
    // File registers
    R,
    ZR,
    RD,
}

impl McDeviceType {
    /// Return true if this device is bit-addressed.
    #[inline]
    pub fn is_bit(&self) -> bool {
        matches!(
            self,
            McDeviceType::SM
                | McDeviceType::X
                | McDeviceType::Y
                | McDeviceType::M
                | McDeviceType::L
                | McDeviceType::F
                | McDeviceType::V
                | McDeviceType::B
                | McDeviceType::TS
                | McDeviceType::TC
                | McDeviceType::LTS
                | McDeviceType::LTC
                | McDeviceType::STS
                | McDeviceType::STC
                | McDeviceType::LSTS
                | McDeviceType::LSTC
                | McDeviceType::CS
                | McDeviceType::CC
                | McDeviceType::LCS
                | McDeviceType::LCC
                | McDeviceType::SB
                | McDeviceType::DX
                | McDeviceType::DY
        )
    }

    /// Return true if this device is word-addressed (16-bit units).
    #[inline]
    pub fn is_word(&self) -> bool {
        matches!(
            self,
            McDeviceType::SD
                | McDeviceType::D
                | McDeviceType::W
                | McDeviceType::TN
                | McDeviceType::STN
                | McDeviceType::CN
                | McDeviceType::SW
                | McDeviceType::Z
                | McDeviceType::R
                | McDeviceType::ZR
                | McDeviceType::RD
        )
    }

    /// Return true if this device is dword-addressed (32-bit units).
    #[inline]
    pub fn is_dword(&self) -> bool {
        matches!(
            self,
            McDeviceType::LTN | McDeviceType::LSTN | McDeviceType::LCN | McDeviceType::LZ
        )
    }

    /// Return numeric notation (decimal/hex) used for this device type.
    #[inline]
    pub fn notation(&self) -> McNotation {
        match self {
            // hex notation devices
            McDeviceType::X
            | McDeviceType::Y
            | McDeviceType::B
            | McDeviceType::W
            | McDeviceType::SB
            | McDeviceType::SW
            | McDeviceType::DX
            | McDeviceType::DY
            | McDeviceType::ZR => McNotation::Hex,
            // default to decimal for the rest (including D)
            _ => McNotation::Dec,
        }
    }

    /// Return the canonical symbol prefix used in textual addresses.
    #[inline]
    pub fn symbol(&self) -> &'static str {
        match self {
            McDeviceType::SM => "SM",
            McDeviceType::SD => "SD",
            McDeviceType::X => "X",
            McDeviceType::Y => "Y",
            McDeviceType::M => "M",
            McDeviceType::L => "L",
            McDeviceType::F => "F",
            McDeviceType::V => "V",
            McDeviceType::B => "B",
            McDeviceType::D => "D",
            McDeviceType::W => "W",
            McDeviceType::TS => "TS",
            McDeviceType::TC => "TC",
            McDeviceType::TN => "TN",
            McDeviceType::LTS => "LTS",
            McDeviceType::LTC => "LTC",
            McDeviceType::LTN => "LTN",
            McDeviceType::STS => "STS",
            McDeviceType::STC => "STC",
            McDeviceType::STN => "STN",
            McDeviceType::LSTS => "LSTS",
            McDeviceType::LSTC => "LSTC",
            McDeviceType::LSTN => "LSTN",
            McDeviceType::CS => "CS",
            McDeviceType::CC => "CC",
            McDeviceType::CN => "CN",
            McDeviceType::LCS => "LCS",
            McDeviceType::LCC => "LCC",
            McDeviceType::LCN => "LCN",
            McDeviceType::SB => "SB",
            McDeviceType::SW => "SW",
            McDeviceType::DX => "DX",
            McDeviceType::DY => "DY",
            McDeviceType::Z => "Z",
            McDeviceType::LZ => "LZ",
            McDeviceType::R => "R",
            McDeviceType::ZR => "ZR",
            McDeviceType::RD => "RD",
        }
    }

    #[inline]
    /// Identify a device code from the beginning of an address string.
    ///
    /// This mirrors the behavior of the Java helper that scans known device
    /// prefixes (longest first) and returns the matching device plus the
    /// remaining suffix (the numeric part and optional bit-index).
    pub fn identify_by_prefix(addr: &str) -> Option<(McDeviceType, &str)> {
        // Order is important: longer symbols must be matched first.
        const ORDERED: &[McDeviceType] = &[
            McDeviceType::LSTS,
            McDeviceType::LSTC,
            McDeviceType::LSTN,
            McDeviceType::LCS,
            McDeviceType::LCC,
            McDeviceType::LCN,
            McDeviceType::LTS,
            McDeviceType::LTC,
            McDeviceType::LTN,
            McDeviceType::STS,
            McDeviceType::STC,
            McDeviceType::STN,
            McDeviceType::TS,
            McDeviceType::TC,
            McDeviceType::TN,
            McDeviceType::CS,
            McDeviceType::CC,
            McDeviceType::CN,
            McDeviceType::SM,
            McDeviceType::SD,
            McDeviceType::SB,
            McDeviceType::SW,
            McDeviceType::DX,
            McDeviceType::DY,
            McDeviceType::ZR,
            McDeviceType::RD,
            McDeviceType::LZ,
            McDeviceType::Z,
            McDeviceType::R,
            McDeviceType::D,
            McDeviceType::X,
            McDeviceType::Y,
            McDeviceType::M,
            McDeviceType::L,
            McDeviceType::F,
            McDeviceType::V,
            McDeviceType::B,
            McDeviceType::W,
        ];

        let upper = addr.trim().to_uppercase();
        for dev in ORDERED {
            let sym = dev.symbol();
            if upper.starts_with(sym) {
                let rest = &addr[sym.len()..];
                return Some((*dev, rest));
            }
        }
        None
    }

    #[inline]
    /// Return the size of the unit for this device type.
    pub fn unit_size(&self) -> usize {
        if self.is_dword() {
            4
        } else if self.is_word() {
            2
        } else {
            1
        }
    }

    /// Return true if this device type is forbidden for batch word read
    /// operations according to the MC specification.
    ///
    /// This mirrors the Java `McNetwork.readDeviceBatchInWord` restrictions:
    /// - Long timers (contacts/coils): LTS, LTC
    /// - Long accumulated timers (contacts/coils): LSTS, LSTC
    /// - Long index register: LZ
    #[inline]
    pub fn is_forbidden_batch_word_read(&self) -> bool {
        matches!(
            self,
            McDeviceType::LTS
                | McDeviceType::LTC
                | McDeviceType::LSTS
                | McDeviceType::LSTC
                | McDeviceType::LZ
        )
    }

    /// Return true if this device type is forbidden for batch word write
    /// operations according to the MC specification.
    ///
    /// This mirrors the Java `McNetwork.writeDeviceBatchInWord` restrictions:
    /// - Long timers (contacts/coils/current): LTS, LTC, LTN
    /// - Long accumulated timers (contacts/coils/current): LSTS, LSTC, LSTN
    /// - Long index register: LZ
    #[inline]
    pub fn is_forbidden_batch_word_write(&self) -> bool {
        matches!(
            self,
            McDeviceType::LTS
                | McDeviceType::LTC
                | McDeviceType::LTN
                | McDeviceType::LSTS
                | McDeviceType::LSTC
                | McDeviceType::LSTN
                | McDeviceType::LZ
        )
    }

    /// Return the 1E frame binary device code for this device type.
    ///
    /// This mapping is based on the Java EMcDeviceCode implementation and
    /// supports A series PLCs using 1E binary frames.
    ///
    /// Returns `None` for device types that are not supported in 1E frames.
    /// Note: 1E frames only support a limited subset of device types.
    #[inline]
    #[allow(unused)]
    pub fn device_code_1e(&self) -> Option<u16> {
        match self {
            // 1E frame only supports these device types
            McDeviceType::X => Some(0x5820),
            McDeviceType::Y => Some(0x5920),
            McDeviceType::M => Some(0x4D20),
            McDeviceType::L => Some(0x4C20),
            McDeviceType::F => Some(0x4620),
            McDeviceType::B => Some(0x4220),
            McDeviceType::D => Some(0x4420),
            McDeviceType::W => Some(0x5720),
            McDeviceType::TN => Some(0x544E),
            McDeviceType::TC => Some(0x5443),
            McDeviceType::TS => Some(0x5453),
            McDeviceType::CN => Some(0x434E),
            McDeviceType::CC => Some(0x4343),
            McDeviceType::CS => Some(0x4353),
            McDeviceType::R => Some(0x5220),
            // All other device types are not supported in 1E frames
            _ => None,
        }
    }

    /// Return the 3E/4E frame binary device code for this device type.
    ///
    /// This mapping is based on the Java EMcDeviceCode implementation and
    /// supports QnA/Q/L/iQ-R series PLCs using 3E or 4E binary frames.
    ///
    /// Returns `None` for device types that are not supported in 3E/4E frames.
    #[inline]
    pub fn device_code_3e(&self) -> Option<u16> {
        match self {
            // Special relay/register
            McDeviceType::SM => Some(0x0091),
            McDeviceType::SD => Some(0x00A9),
            // Inputs/outputs
            McDeviceType::X => Some(0x009C),
            McDeviceType::Y => Some(0x009D),
            // Internal relays
            McDeviceType::M => Some(0x0090),
            McDeviceType::L => Some(0x0092),
            McDeviceType::F => Some(0x0093),
            McDeviceType::V => Some(0x0094),
            // Link relay/register
            McDeviceType::B => Some(0x00A0),
            // Data register
            McDeviceType::D => Some(0x00A8),
            McDeviceType::W => Some(0x00B4),
            // Timer contacts/coils/current values
            McDeviceType::TS => Some(0x00C1),
            McDeviceType::TC => Some(0x00C0),
            McDeviceType::TN => Some(0x00C2),
            McDeviceType::LTS => Some(0x0051),
            McDeviceType::LTC => Some(0x0050),
            McDeviceType::LTN => Some(0x0052),
            McDeviceType::STS => Some(0x00C7),
            McDeviceType::STC => Some(0x00C6),
            McDeviceType::STN => Some(0x00C8),
            McDeviceType::LSTS => Some(0x0059),
            McDeviceType::LSTC => Some(0x0058),
            McDeviceType::LSTN => Some(0x005A),
            // Counter contacts/coils/current values
            McDeviceType::CS => Some(0x00C4),
            McDeviceType::CC => Some(0x00C3),
            McDeviceType::CN => Some(0x00C5),
            McDeviceType::LCS => Some(0x0055),
            McDeviceType::LCC => Some(0x0054),
            McDeviceType::LCN => Some(0x0056),
            // Link special relay/register
            McDeviceType::SB => Some(0x00A1),
            McDeviceType::SW => Some(0x00B5),
            // Direct access
            McDeviceType::DX => Some(0x00A2),
            McDeviceType::DY => Some(0x00A3),
            // Index registers
            McDeviceType::Z => Some(0x00CC),
            McDeviceType::LZ => Some(0x0062),
            // File registers
            McDeviceType::R => Some(0x00AF),
            McDeviceType::ZR => Some(0x00B0),
            McDeviceType::RD => Some(0x002C),
        }
    }
}
