//! Decoded frame type shared across all engine modules.
//!
//! [`DecodedFrame`] is the universal currency after frame acquisition —
//! every consumer (preprocessor, inference backend, annotator, WASM
//! algorithm host) operates on this type.

use crate::frame::memory::{FrameMemory, PixelFormat};
use bytes::Bytes;
use ng_gateway_error::ai::AiEngineError;

/// A decoded video frame with unified memory backing.
///
/// The `memory` field may point to CPU, DMA-buf, or device memory.
/// Consumers must check the memory variant and choose the optimal
/// access path (zero-copy DMA import, mmap fallback, or CPU buffer).
///
/// # Migration from `Bytes`
///
/// Previously this struct held a bare `data: Bytes` field and assumed
/// RGB24. Now it carries a [`FrameMemory`] discriminant plus an explicit
/// [`PixelFormat`], enabling zero-copy DMA pipelines while remaining
/// fully compatible with the existing CPU path via
/// [`try_cpu_data()`](Self::try_cpu_data).
#[derive(Debug)]
pub struct DecodedFrame {
    /// Pixel data (may be in CPU, DMA, or device memory).
    pub memory: FrameMemory,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Decoded pixel format.
    pub pixel_format: PixelFormat,
    /// Original capture timestamp (nanoseconds from GStreamer clock, or 0).
    pub timestamp: i64,
    /// Row stride in bytes (0 = tightly packed, i.e. `width × bpp`).
    pub stride: u32,
    /// Whether this frame is a keyframe (I-frame).
    ///
    /// Derived from GStreamer's `GST_BUFFER_FLAG_DELTA_UNIT` flag:
    /// a buffer **without** the DELTA_UNIT flag is a keyframe.
    /// Defaults to `true` for non-GStreamer sources (legacy path).
    pub is_keyframe: bool,
}

/// Borrowed frame view for hot-path compute kernels.
///
/// This view is intentionally non-owning and should not cross async task
/// boundaries that require `'static`.
#[derive(Debug, Clone, Copy)]
pub struct FrameView<'a> {
    /// Borrowed pixel bytes (row-major).
    pub data: &'a [u8],
    /// View width in pixels.
    pub width: u32,
    /// View height in pixels.
    pub height: u32,
}

impl DecodedFrame {
    /// Construct a CPU-backed RGB24 frame (convenience for the legacy path).
    ///
    /// This is equivalent to what `FrameDecoderPool` produces today.
    #[inline]
    pub fn from_rgb24(data: Bytes, width: u32, height: u32) -> Self {
        Self {
            memory: FrameMemory::Cpu(data),
            width,
            height,
            pixel_format: PixelFormat::Rgb24,
            timestamp: 0,
            stride: 0,
            is_keyframe: true,
        }
    }

    /// Expected byte length for an RGB24 frame at the given dimensions.
    #[inline]
    pub fn expected_rgb24_len(width: u32, height: u32) -> usize {
        width as usize * height as usize * 3
    }

    /// Total pixel data size in bytes for tightly packed layout.
    #[inline]
    pub fn pixel_data_size(&self) -> usize {
        self.pixel_format.frame_size(self.width, self.height)
    }

    /// Validate that the data length matches the declared dimensions.
    ///
    /// Only meaningful for CPU-resident frames. Returns `true` for
    /// non-CPU memory (validation deferred to the device layer).
    #[inline]
    pub fn is_valid(&self) -> bool {
        match self.memory.as_cpu_slice() {
            Some(data) => data.len() == self.pixel_data_size(),
            None => true,
        }
    }

    /// Borrow CPU pixel data.
    ///
    /// Returns an error when the frame is not CPU-resident.
    /// Callers may fallback to `memory.to_cpu()` when materialization
    /// from DMA/device memory is acceptable.
    #[inline]
    pub fn try_cpu_data(&self) -> Result<&[u8], AiEngineError> {
        self.memory.as_cpu_slice().ok_or_else(|| {
            AiEngineError::FrameError(
                "frame is not CPU-resident; use memory.to_cpu() for fallback".into(),
            )
        })
    }

    /// Borrow this frame as a lightweight compute view.
    ///
    /// Returns an error if the frame is not CPU-resident.
    #[inline]
    pub fn view(&self) -> Result<FrameView<'_>, AiEngineError> {
        let data = self.try_cpu_data()?;
        Ok(FrameView {
            data,
            width: self.width,
            height: self.height,
        })
    }

    /// Try to clone this frame.
    ///
    /// - CPU memory: zero-cost clone (Bytes ref-count).
    /// - DMA-buf / DeviceBuffer: materializes to CPU and wraps as `FrameMemory::Cpu`.
    pub fn try_clone(&self) -> Result<Self, AiEngineError> {
        let cloned_memory = self.memory.try_clone()?;
        Ok(Self {
            memory: cloned_memory,
            width: self.width,
            height: self.height,
            pixel_format: self.pixel_format,
            timestamp: self.timestamp,
            stride: self.stride,
            is_keyframe: self.is_keyframe,
        })
    }

    /// Create an empty placeholder frame (for merge-only pipeline contexts).
    #[inline]
    pub fn empty() -> Self {
        Self {
            memory: FrameMemory::Cpu(Bytes::new()),
            width: 0,
            height: 0,
            pixel_format: PixelFormat::Rgb24,
            timestamp: 0,
            stride: 0,
            is_keyframe: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn from_rgb24_correct_layout() {
        let w = 320u32;
        let h = 240u32;
        let data = Bytes::from(vec![0u8; (w * h * 3) as usize]);
        let frame = DecodedFrame::from_rgb24(data, w, h);

        assert_eq!(frame.width, w);
        assert_eq!(frame.height, h);
        assert_eq!(frame.pixel_format, PixelFormat::Rgb24);
        assert_eq!(
            frame.stride, 0,
            "from_rgb24 should set stride to 0 (tightly packed)"
        );
        assert_eq!(
            frame.timestamp, 0,
            "from_rgb24 should default timestamp to 0"
        );
    }

    #[test]
    fn expected_rgb24_len() {
        assert_eq!(
            DecodedFrame::expected_rgb24_len(1920, 1080),
            1920 * 1080 * 3
        );
    }

    #[test]
    fn pixel_data_size_rgb24() {
        let w = 1920u32;
        let h = 1080u32;
        let data = Bytes::from(vec![0u8; (w * h * 3) as usize]);
        let frame = DecodedFrame::from_rgb24(data, w, h);

        assert_eq!(
            frame.pixel_data_size(),
            DecodedFrame::expected_rgb24_len(w, h)
        );
    }

    #[test]
    fn is_valid_matching_size() {
        let w = 64u32;
        let h = 48u32;
        let data = Bytes::from(vec![128u8; (w * h * 3) as usize]);
        let frame = DecodedFrame::from_rgb24(data, w, h);

        assert!(frame.is_valid());
    }

    #[test]
    fn is_valid_truncated_data() {
        let w = 64u32;
        let h = 48u32;
        let short_data = Bytes::from(vec![0u8; 10]);
        let frame = DecodedFrame::from_rgb24(short_data, w, h);

        assert!(!frame.is_valid());
    }

    #[test]
    fn try_clone_preserves_content() {
        let w = 8u32;
        let h = 4u32;
        let pixels: Vec<u8> = (0..w * h * 3).map(|i| (i % 256) as u8).collect();
        let frame = DecodedFrame::from_rgb24(Bytes::from(pixels.clone()), w, h);

        let cloned = frame.try_clone().expect("CPU clone should succeed");
        assert_eq!(cloned.width, frame.width);
        assert_eq!(cloned.height, frame.height);
        assert_eq!(cloned.pixel_format, frame.pixel_format);
        assert_eq!(cloned.timestamp, frame.timestamp);
        assert_eq!(cloned.stride, frame.stride);
        let cloned_data = cloned
            .try_cpu_data()
            .expect("cloned frame should be CPU-resident");
        let frame_data = frame
            .try_cpu_data()
            .expect("source frame should be CPU-resident");
        assert_eq!(cloned_data, frame_data);
    }

    #[test]
    fn empty_frame_properties() {
        let frame = DecodedFrame::empty();

        assert_eq!(frame.width, 0);
        assert_eq!(frame.height, 0);
        assert!(frame
            .try_cpu_data()
            .expect("empty frame should be CPU-resident")
            .is_empty());
    }
}
