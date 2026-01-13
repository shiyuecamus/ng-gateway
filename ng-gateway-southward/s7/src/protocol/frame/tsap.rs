use super::{
    super::error::{Error as S7Error, Result as S7Result},
    CpuType,
};
use std::{result::Result as StdResult, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tsap(pub u8, pub u8); // (high byte, low byte)

impl Tsap {
    #[inline]
    pub fn high(&self) -> u8 {
        self.0
    }

    #[inline]
    pub fn low(&self) -> u8 {
        self.1
    }
}

impl From<Tsap> for u16 {
    #[inline]
    fn from(tsap: Tsap) -> Self {
        ((tsap.0 as u16) << 8) | (tsap.1 as u16)
    }
}

impl From<(u8, u8)> for Tsap {
    /// Create `Tsap` from a tuple of bytes (high, low). This conversion
    /// is infallible and preferred where raw bytes are already known.
    #[inline]
    fn from(value: (u8, u8)) -> Self {
        Tsap(value.0, value.1)
    }
}

impl TryFrom<&str> for Tsap {
    type Error = S7Error;

    /// Parse TSAP from string.
    ///
    /// Supported forms:
    /// - "HH:LL" where HH/LL are hex bytes (e.g. "03:00")
    /// - "0xHHLL" 16-bit hex (e.g. "0x0300")
    /// - decimal u16 (e.g. "768")
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let s = value.trim();
        if let Some((h, l)) = s.split_once(':') {
            let high = u8::from_str_radix(
                h.trim().trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .map_err(|_| S7Error::InvalidRack(0xFF))?;
            let low = u8::from_str_radix(
                l.trim().trim_start_matches("0x").trim_start_matches("0X"),
                16,
            )
            .map_err(|_| S7Error::InvalidSlot(0xFF))?;
            return Ok(Tsap(high, low));
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            let v = u16::from_str_radix(hex, 16).map_err(|_| S7Error::InvalidRack(0xFF))?;
            return Ok(Tsap(((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8));
        }
        if let Ok(v) = s.parse::<u16>() {
            return Ok(Tsap(((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8));
        }
        Err(S7Error::InvalidRack(0xFF))
    }
}

impl FromStr for Tsap {
    type Err = S7Error;
    /// Parse `Tsap` from string via `TryFrom<&str>` to integrate with `.parse()`.
    #[inline]
    fn from_str(s: &str) -> StdResult<Self, Self::Err> {
        Tsap::try_from(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TsapPair {
    pub local: Tsap,
    pub remote: Tsap,
}

impl TryFrom<&str> for TsapPair {
    type Error = S7Error;

    /// Parse TSAP pair from "local-remote" joined by '/'. Examples:
    /// - "03:00/03:01"
    /// - "0x0300/0x0301"
    /// - "768/769"
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let (l, r) = value.split_once('/').ok_or(S7Error::InvalidRack(0xFF))?;
        let local = Tsap::try_from(l.trim())?;
        let remote = Tsap::try_from(r.trim())?;
        Ok(TsapPair { local, remote })
    }
}

impl From<(Tsap, Tsap)> for TsapPair {
    /// Create `TsapPair` from a tuple (local, remote). Infallible.
    #[inline]
    fn from(value: (Tsap, Tsap)) -> Self {
        TsapPair {
            local: value.0,
            remote: value.1,
        }
    }
}

impl FromStr for TsapPair {
    type Err = S7Error;
    /// Parse `TsapPair` from string via `TryFrom<&str>` to integrate with `.parse()`.
    #[inline]
    fn from_str(s: &str) -> StdResult<Self, Self::Err> {
        TsapPair::try_from(s)
    }
}

#[inline]
fn validate_rack_slot(rack: u8, slot: u8) -> S7Result<()> {
    if rack > 0x0F {
        return Err(S7Error::InvalidRack(rack));
    }
    if slot > 0x0F {
        return Err(S7Error::InvalidSlot(slot));
    }
    Ok(())
}

/// 提供默认的 TSAP 对（优先覆盖 1200/1500/300/400 等主流机型）
pub fn default_tsap_pair(cpu: CpuType, rack: u8, slot: u8) -> S7Result<TsapPair> {
    validate_rack_slot(rack, slot)?;

    let pair = match cpu {
        // S7-200（经典）：常见为 0x1000/0x1001
        CpuType::S7200 => TsapPair {
            local: Tsap(0x10, 0x00),
            remote: Tsap(0x10, 0x01),
        },

        // LOGO! 0BA8：常见为 0x0100/0x0102
        CpuType::Logo0BA8 => TsapPair {
            local: Tsap(0x01, 0x00),
            remote: Tsap(0x01, 0x02),
        },

        // S7-200 Smart、S7-300/400/1200/1500：主流 S7 通信
        CpuType::S7200Smart
        | CpuType::S7300
        | CpuType::S7400
        | CpuType::S71200
        | CpuType::S71500 => TsapPair {
            local: Tsap(0x01, 0x00),
            remote: Tsap(0x03, (rack << 5) | slot),
        },
    };

    Ok(pair)
}
