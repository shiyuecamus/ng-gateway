use serde::{Deserialize, Serialize};

/// MC access route for 1E frame.
///
/// 1E frames only carry a single PC number as route selector.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McRoute1E {
    /// PC number (1 byte on the wire).
    pub pc_number: u8,
}

#[allow(unused)]
impl McRoute1E {
    /// Create a default 1E route (PC number = 0xFF).
    pub fn default_1e() -> Self {
        Self { pc_number: 0xFF }
    }

    /// Byte length of the encoded route (1 byte).
    pub const fn byte_len() -> usize {
        1
    }
}

/// MC access route for 3E/4E frames.
///
/// 3E/4E frames carry a 5-byte route: network, PC, module I/O and station.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McRoute3E4E {
    /// Network number (1 byte).
    pub network_number: u8,
    /// PC number (1 byte).
    pub pc_number: u8,
    /// Request destination module I/O number (2 bytes).
    pub module_io_number: u16,
    /// Request destination module station number (1 byte).
    pub station_number: u8,
}

impl McRoute3E4E {
    /// Byte length of the encoded route (5 bytes).
    pub const fn byte_len() -> usize {
        5
    }
}

impl Default for McRoute3E4E {
    fn default() -> Self {
        Self {
            network_number: 0x00,
            pc_number: 0xFF,
            module_io_number: 0x03FF,
            station_number: 0x00,
        }
    }
}
