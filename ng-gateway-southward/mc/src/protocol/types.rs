use serde::{Deserialize, Serialize};

/// MC frame variant for Mitsubishi PLC communication.
///
/// This enum models the different MC frame families and encoding modes.
/// ASCII variants are declared for future extension but are not implemented yet.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McFrameVariant {
    /// A-compatible 1E frame, binary encoding.
    Frame1EBinary,
    /// QnA-compatible 3E frame, binary encoding.
    Frame3EBinary,
    /// QnA-compatible 4E frame, binary encoding.
    Frame4EBinary,
    /// QnA-compatible 3E frame, ASCII encoding (not yet implemented).
    Frame3EAscii,
    /// QnA-compatible 4E frame, ASCII encoding (not yet implemented).
    Frame4EAscii,
}

impl McFrameVariant {
    /// Derive MC request sub-header from frame variant.
    pub fn sub_header(&self) -> u16 {
        match self {
            McFrameVariant::Frame3EBinary | McFrameVariant::Frame3EAscii => 0x0050,
            McFrameVariant::Frame4EBinary | McFrameVariant::Frame4EAscii => 0x0054,
            McFrameVariant::Frame1EBinary => 0x0000,
        }
    }
}

/// PLC series for MC protocol (A/QnA/Q/L/iQ-R).
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum McSeries {
    /// A series, typically using 1E frame.
    A,
    /// QnA series, commonly using 3E/4E frame.
    QnA,
    /// Q/L series.
    QL,
    /// iQ-R series.
    IQR,
}

impl McSeries {
    /// Return true if this series uses the legacy A-compatible 1E frame.
    #[allow(unused)]
    pub fn uses_1e(&self) -> bool {
        matches!(self, McSeries::A)
    }

    #[inline]
    pub fn frame_variant(&self) -> McFrameVariant {
        match self {
            McSeries::A => McFrameVariant::Frame1EBinary,
            McSeries::QnA => McFrameVariant::Frame3EBinary,
            McSeries::QL => McFrameVariant::Frame3EBinary,
            McSeries::IQR => McFrameVariant::Frame3EBinary,
        }
    }

    /// Maximum number of word units for batch read/write per MC specification.
    ///
    /// These limits mirror the Java `EMcSeries.deviceBatchInWordPointsCount`
    /// values:
    ///
    /// - A   : 10  (1E frame, not used by current driver paths)
    /// - QnA : 480
    /// - Q/L : 960
    /// - iQ-R: 960
    #[inline]
    pub fn device_batch_in_word_points_max(&self) -> u16 {
        match self {
            McSeries::A => 10,
            McSeries::QnA => 480,
            McSeries::QL => 960,
            McSeries::IQR => 960,
        }
    }

    /// Maximum number of word units for random read per MC specification.
    ///
    /// Mirrors Java `EMcSeries.deviceRandomReadInWordPointsCount`.
    #[inline]
    pub fn device_random_read_in_word_points_max(&self) -> u16 {
        match self {
            McSeries::A => 0,
            McSeries::QnA => 96,
            McSeries::QL => 192,
            McSeries::IQR => 96,
        }
    }

    /// Maximum number of word units for random write per MC specification.
    ///
    /// Mirrors Java `EMcSeries.deviceRandomWriteInWordPointsCount`.
    #[inline]
    pub fn device_random_write_in_word_points_max(&self) -> u16 {
        match self {
            McSeries::A => 10,
            McSeries::QnA => 960,
            McSeries::QL => 1920,
            McSeries::IQR => 960,
        }
    }

    /// Maximum number of bit units for random write per MC specification.
    ///
    /// Mirrors Java `EMcSeries.deviceRandomWriteInBitPointsCount`.
    #[inline]
    #[allow(unused)]
    pub fn device_random_write_in_bit_points_max(&self) -> u16 {
        match self {
            McSeries::A => 80,
            McSeries::QnA => 94,
            McSeries::QL => 188,
            McSeries::IQR => 94,
        }
    }
}
