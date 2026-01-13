use crate::protocol::types::McSeries;
use serde::{Deserialize, Serialize};

/// High-level MC command kind, grouped by semantics.
///
/// This enum is intentionally aligned with the Java `EMcCommand` but expressed
/// in a more semantic form for use in Rust code. Each variant corresponds to
/// one or more concrete wire-level command codes depending on the frame type.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum McCommandKind {
    // 1E-only variants (bit/word batch access & random write)
    DeviceAccessBatchReadBit1E,
    DeviceAccessBatchReadWord1E,
    DeviceAccessBatchWriteBit1E,
    DeviceAccessBatchWriteWord1E,
    DeviceAccessRandomWriteBit1E,
    DeviceAccessRandomWriteWord1E,

    // 3E/4E device access
    DeviceAccessBatchReadUnits,
    DeviceAccessBatchWriteUnits,
    DeviceAccessRandomReadUnits,
    DeviceAccessRandomWriteUnits,
    DeviceAccessBatchReadMultipleBlocks,
    DeviceAccessBatchWriteMultipleBlocks,

    // 3E/4E label access
    LabelAccessBatchReadArray,
    LabelAccessBatchWriteArray,
    LabelAccessRandomReadLabels,
    LabelAccessRandomWriteLabels,

    // 3E/4E buffer memory access
    BufferMemoryBatchRead,
    BufferMemoryBatchWrite,
    BufferMemoryIFModuleBatchRead,
    BufferMemoryIFModuleBatchWrite,

    // 3E/4E module control
    ModuleControlRemoteRun,
    ModuleControlRemoteStop,
    ModuleControlRemotePause,
    ModuleControlRemoteLatchClear,
    ModuleControlRemoteReset,
    ModuleControlReadCpuModelName,
    ModuleControlPasswordUnlock,
    ModuleControlPasswordLock,
    ModuleControlLoopbackTest,
    ModuleControlClearErrorInformation,
}

/// Raw MC command code as carried on the wire.
///
/// For 1E this is a single byte; for 3E/4E it is a 16-bit value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McCommandCode {
    Code1E(u8),
    Code3E4E(u16),
}

#[allow(unused)]
impl McCommandKind {
    /// Map a semantic command kind to a concrete wire-level code.
    ///
    /// The mapping follows the Java `EMcCommand` definition. Frame-type
    /// specific constraints must be enforced by higher layers.
    pub fn to_code(self) -> McCommandCode {
        match self {
            // 1E frame
            McCommandKind::DeviceAccessBatchReadBit1E => McCommandCode::Code1E(0x00),
            McCommandKind::DeviceAccessBatchReadWord1E => McCommandCode::Code1E(0x01),
            McCommandKind::DeviceAccessBatchWriteBit1E => McCommandCode::Code1E(0x02),
            McCommandKind::DeviceAccessBatchWriteWord1E => McCommandCode::Code1E(0x03),
            McCommandKind::DeviceAccessRandomWriteBit1E => McCommandCode::Code1E(0x04),
            McCommandKind::DeviceAccessRandomWriteWord1E => McCommandCode::Code1E(0x05),

            // 3E/4E device access
            McCommandKind::DeviceAccessBatchReadUnits => McCommandCode::Code3E4E(0x0401),
            McCommandKind::DeviceAccessBatchWriteUnits => McCommandCode::Code3E4E(0x1401),
            McCommandKind::DeviceAccessRandomReadUnits => McCommandCode::Code3E4E(0x0403),
            McCommandKind::DeviceAccessRandomWriteUnits => McCommandCode::Code3E4E(0x1402),
            McCommandKind::DeviceAccessBatchReadMultipleBlocks => McCommandCode::Code3E4E(0x0406),
            McCommandKind::DeviceAccessBatchWriteMultipleBlocks => McCommandCode::Code3E4E(0x1406),

            // 3E/4E label access
            McCommandKind::LabelAccessBatchReadArray => McCommandCode::Code3E4E(0x041A),
            McCommandKind::LabelAccessBatchWriteArray => McCommandCode::Code3E4E(0x141A),
            McCommandKind::LabelAccessRandomReadLabels => McCommandCode::Code3E4E(0x041C),
            McCommandKind::LabelAccessRandomWriteLabels => McCommandCode::Code3E4E(0x141B),

            // 3E/4E buffer memory access
            McCommandKind::BufferMemoryBatchRead => McCommandCode::Code3E4E(0x0613),
            McCommandKind::BufferMemoryBatchWrite => McCommandCode::Code3E4E(0x1613),
            McCommandKind::BufferMemoryIFModuleBatchRead => McCommandCode::Code3E4E(0x0601),
            McCommandKind::BufferMemoryIFModuleBatchWrite => McCommandCode::Code3E4E(0x1601),

            // 3E/4E module control
            McCommandKind::ModuleControlRemoteRun => McCommandCode::Code3E4E(0x1001),
            McCommandKind::ModuleControlRemoteStop => McCommandCode::Code3E4E(0x1002),
            McCommandKind::ModuleControlRemotePause => McCommandCode::Code3E4E(0x1003),
            McCommandKind::ModuleControlRemoteLatchClear => McCommandCode::Code3E4E(0x1005),
            McCommandKind::ModuleControlRemoteReset => McCommandCode::Code3E4E(0x1006),
            McCommandKind::ModuleControlReadCpuModelName => McCommandCode::Code3E4E(0x0101),
            McCommandKind::ModuleControlPasswordUnlock => McCommandCode::Code3E4E(0x1630),
            McCommandKind::ModuleControlPasswordLock => McCommandCode::Code3E4E(0x1631),
            McCommandKind::ModuleControlLoopbackTest => McCommandCode::Code3E4E(0x0619),
            McCommandKind::ModuleControlClearErrorInformation => McCommandCode::Code3E4E(0x1617),
        }
    }

    /// Map a wire-level command code back to a semantic kind.
    ///
    /// This is primarily used when decoding responses from the PLC. For
    /// unsupported codes we return `None` so that higher layers can decide
    /// whether to treat it as a hard error or an opaque payload.
    pub fn from_code(code: McCommandCode) -> Option<Self> {
        match code {
            McCommandCode::Code1E(0x00) => Some(McCommandKind::DeviceAccessBatchReadBit1E),
            McCommandCode::Code1E(0x01) => Some(McCommandKind::DeviceAccessBatchReadWord1E),
            McCommandCode::Code1E(0x02) => Some(McCommandKind::DeviceAccessBatchWriteBit1E),
            McCommandCode::Code1E(0x03) => Some(McCommandKind::DeviceAccessBatchWriteWord1E),
            McCommandCode::Code1E(0x04) => Some(McCommandKind::DeviceAccessRandomWriteBit1E),
            McCommandCode::Code1E(0x05) => Some(McCommandKind::DeviceAccessRandomWriteWord1E),

            McCommandCode::Code3E4E(0x0401) => Some(McCommandKind::DeviceAccessBatchReadUnits),
            McCommandCode::Code3E4E(0x1401) => Some(McCommandKind::DeviceAccessBatchWriteUnits),
            McCommandCode::Code3E4E(0x0403) => Some(McCommandKind::DeviceAccessRandomReadUnits),
            McCommandCode::Code3E4E(0x1402) => Some(McCommandKind::DeviceAccessRandomWriteUnits),
            McCommandCode::Code3E4E(0x0406) => {
                Some(McCommandKind::DeviceAccessBatchReadMultipleBlocks)
            }
            McCommandCode::Code3E4E(0x1406) => {
                Some(McCommandKind::DeviceAccessBatchWriteMultipleBlocks)
            }

            McCommandCode::Code3E4E(0x041A) => Some(McCommandKind::LabelAccessBatchReadArray),
            McCommandCode::Code3E4E(0x141A) => Some(McCommandKind::LabelAccessBatchWriteArray),
            McCommandCode::Code3E4E(0x041C) => Some(McCommandKind::LabelAccessRandomReadLabels),
            McCommandCode::Code3E4E(0x141B) => Some(McCommandKind::LabelAccessRandomWriteLabels),

            McCommandCode::Code3E4E(0x0613) => Some(McCommandKind::BufferMemoryBatchRead),
            McCommandCode::Code3E4E(0x1613) => Some(McCommandKind::BufferMemoryBatchWrite),
            McCommandCode::Code3E4E(0x0601) => Some(McCommandKind::BufferMemoryIFModuleBatchRead),
            McCommandCode::Code3E4E(0x1601) => Some(McCommandKind::BufferMemoryIFModuleBatchWrite),

            McCommandCode::Code3E4E(0x1001) => Some(McCommandKind::ModuleControlRemoteRun),
            McCommandCode::Code3E4E(0x1002) => Some(McCommandKind::ModuleControlRemoteStop),
            McCommandCode::Code3E4E(0x1003) => Some(McCommandKind::ModuleControlRemotePause),
            McCommandCode::Code3E4E(0x1005) => Some(McCommandKind::ModuleControlRemoteLatchClear),
            McCommandCode::Code3E4E(0x1006) => Some(McCommandKind::ModuleControlRemoteReset),
            McCommandCode::Code3E4E(0x0101) => Some(McCommandKind::ModuleControlReadCpuModelName),
            McCommandCode::Code3E4E(0x1630) => Some(McCommandKind::ModuleControlPasswordUnlock),
            McCommandCode::Code3E4E(0x1631) => Some(McCommandKind::ModuleControlPasswordLock),
            McCommandCode::Code3E4E(0x0619) => Some(McCommandKind::ModuleControlLoopbackTest),
            McCommandCode::Code3E4E(0x1617) => {
                Some(McCommandKind::ModuleControlClearErrorInformation)
            }

            // Unknown / reserved code
            _ => None,
        }
    }

    /// Compute the default subcommand for this command and PLC series.
    ///
    /// For most Q/QnA series commands the subcommand is `0x0000`. Some IQ-R
    /// series commands use a different subcommand as defined by the MC
    /// specification and mirrored by the Java reference implementation.
    #[inline]
    pub fn default_subcommand(self, series: McSeries) -> u16 {
        match (self, series) {
            // Device access batch read/write units:
            //
            // - Q/QnA/QL use subcommand 0x0000
            // - IQ-R uses subcommand 0x0002 for batch read/write/random units
            //   (see McReadDeviceBatchInWordReqData / McWriteDeviceBatchInWordReqData /
            //   McReadDeviceRandomInWordReqData / McWriteDeviceRandomInWordReqData).
            (McCommandKind::DeviceAccessBatchReadUnits, McSeries::IQR) => 0x0002,
            (McCommandKind::DeviceAccessBatchWriteUnits, McSeries::IQR) => 0x0002,
            (McCommandKind::DeviceAccessRandomReadUnits, McSeries::IQR) => 0x0002,
            (McCommandKind::DeviceAccessRandomWriteUnits, McSeries::IQR) => 0x0002,
            // Other commands currently default to 0x0000; series-specific
            // overrides can be added here when the corresponding commands
            // are implemented.
            _ => 0x0000,
        }
    }
}
