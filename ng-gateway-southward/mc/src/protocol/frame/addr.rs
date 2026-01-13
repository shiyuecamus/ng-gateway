use super::device::{McDeviceType, McNotation};
use serde::{Deserialize, Serialize};

/// Parsed MC logical address (device + head + optional bit index).
///
/// This type is used by planner and codec layers to build concrete MC
/// requests from the raw address strings stored in configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McLogicalAddress {
    /// Device type (e.g. X, Y, M, D, R, ...).
    pub device: McDeviceType,
    /// Head device number (already parsed according to device notation).
    pub head: u32,
    /// Optional bit index inside the word, for bit addressing like D20.2.
    pub bit: Option<u8>,
}

impl McLogicalAddress {
    /// Try to parse a textual MC address such as "X10", "D20.2", "Y1A0".
    ///
    /// This function is intentionally lenient and only performs basic syntax
    /// validation. Semantic checks (e.g. DataType vs device bit/word/dword
    /// constraints) are handled at a higher layer.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("address is empty".to_string());
        }

        let (device, rest) = McDeviceType::identify_by_prefix(trimmed)
            .ok_or_else(|| format!("unknown MC device prefix in address: {trimmed}"))?;

        // Split numeric and optional bit index, e.g. "20.2"
        let (num_part, bit_part) = match rest.split_once('.') {
            Some((n, b)) => (n.trim(), Some(b.trim())),
            None => (rest.trim(), None),
        };

        if num_part.is_empty() {
            return Err(format!("missing head device number in address: {trimmed}"));
        }

        let head = match device.notation() {
            McNotation::Dec => num_part.parse::<u32>(),
            McNotation::Hex => u32::from_str_radix(num_part, 16),
        }
        .map_err(|e| format!("invalid head number '{num_part}' in address '{trimmed}': {e}"))?;

        let bit = if let Some(bit_str) = bit_part {
            if bit_str.is_empty() {
                return Err(format!("empty bit index in address: {trimmed}"));
            }
            let v: u8 = bit_str.parse().map_err(|e| {
                format!("invalid bit index '{bit_str}' in address '{trimmed}': {e}")
            })?;
            Some(v)
        } else {
            None
        };

        Ok(McLogicalAddress { device, head, bit })
    }
}
