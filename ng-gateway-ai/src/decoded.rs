//! Decoded frame type (engine-internal but shared across modules).

use bytes::Bytes;

/// A decoded video frame in RGB24 format, ready for preprocessing.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// RGB24 pixel data (row-major, 3 bytes per pixel).
    pub data: Bytes,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
}

/// Borrowed frame view for hot-path compute kernels.
///
/// This view is intentionally non-owning and should not cross async task
/// boundaries that require `'static`.
#[derive(Debug, Clone, Copy)]
pub struct FrameView<'a> {
    /// Borrowed RGB24 bytes (row-major, 3 bytes per pixel).
    pub data: &'a [u8],
    /// View width in pixels.
    pub width: u32,
    /// View height in pixels.
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

    /// Borrow this frame as a lightweight compute view.
    #[inline]
    pub fn view(&self) -> FrameView<'_> {
        FrameView {
            data: self.data.as_ref(),
            width: self.width,
            height: self.height,
        }
    }
}
