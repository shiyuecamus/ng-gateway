//! Decoded frame type (engine-internal but shared across modules).

/// A decoded video frame in RGB24 format, ready for preprocessing.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// RGB24 pixel data (row-major, 3 bytes per pixel).
    pub data: Vec<u8>,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

impl DecodedFrame {
    /// Expected byte length for an RGB24 frame at the given dimensions.
    #[inline]
    pub fn expected_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3
    }

    /// Validate that the data length matches the declared dimensions.
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.data.len() == Self::expected_len(self.width, self.height)
    }
}
