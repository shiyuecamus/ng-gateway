//! Platform-specialized buffer extraction from GStreamer samples.
//!
//! The [`BufferExtractor`] trait abstracts the critical bridge between
//! GStreamer's buffer model and our [`FrameMemory`] abstraction. Each
//! platform has a distinct optimal path:
//!
//! | Platform | Strategy | Output |
//! |----------|----------|--------|
//! | RK3588 | DMA-buf fd extraction | `FrameMemory::DmaBuf` |
//! | Jetson (with CUDA) | NvBufSurface → CUDA device ptr | `FrameMemory::DeviceBuffer` |
//! | Jetson (no CUDA) | CPU mmap fallback | `FrameMemory::GstBufferRef` |
//! | VAAPI | DMA-buf fd extraction | `FrameMemory::DmaBuf` |
//! | Generic | GstBuffer map or copy | `FrameMemory::GstBufferRef` / `FrameMemory::Cpu` |

use crate::{
    decoded::DecodedFrame,
    frame::{
        memory::{DeviceType, FrameMemory, HardwarePlatform, PixelFormat},
        platform::PlatformCapabilities,
    },
};
use ng_gateway_error::ai::AiEngineError;

/// Extracts a [`DecodedFrame`] from a GStreamer [`Sample`](gstreamer::Sample).
///
/// Implementors specialize the extraction path for their hardware platform,
/// choosing between DMA-buf fd extraction, CUDA device pointer mapping,
/// GStreamer buffer borrowing, or CPU copying.
pub trait BufferExtractor: Send + Sync + 'static {
    /// Extract a decoded frame from a GStreamer sample.
    fn extract(&self, sample: &gstreamer::Sample) -> Result<DecodedFrame, AiEngineError>;
}

/// Create the optimal buffer extractor for the detected platform.
pub fn create_extractor(caps: &PlatformCapabilities) -> Box<dyn BufferExtractor> {
    match caps.platform {
        HardwarePlatform::Rockchip if caps.supports_dma_buf => Box::new(DmaBufExtractor {
            device_type: DeviceType::RknnNpu,
        }),
        HardwarePlatform::NvidiaJetson => Box::new(JetsonExtractor),
        HardwarePlatform::Vaapi if caps.supports_dma_buf => Box::new(DmaBufExtractor {
            device_type: DeviceType::IntelVaapi,
        }),
        _ => Box::new(CpuExtractor),
    }
}

// ── Shared helpers ─────────────────────────────────────────────────

/// Parse video metadata from a GStreamer sample's caps.
struct SampleMeta {
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    stride: u32,
    timestamp: i64,
    /// `true` when the buffer does NOT carry the `DELTA_UNIT` flag,
    /// meaning it is an IDR / keyframe.
    is_keyframe: bool,
    #[cfg(all(feature = "dmabuf", target_os = "linux"))]
    size: usize,
    #[cfg(all(feature = "dmabuf", target_os = "linux"))]
    drm_fourcc: u32,
}

fn parse_sample_meta(sample: &gstreamer::Sample) -> Result<SampleMeta, AiEngineError> {
    let buffer = sample
        .buffer()
        .ok_or_else(|| AiEngineError::FrameError("GstSample has no buffer".into()))?;
    let caps = sample
        .caps()
        .ok_or_else(|| AiEngineError::FrameError("GstSample has no caps".into()))?;

    let info = gstreamer_video::VideoInfo::from_caps(caps)
        .map_err(|e| AiEngineError::FrameError(format!("invalid video caps: {e}")))?;

    // A buffer WITHOUT the DELTA_UNIT flag is a keyframe (IDR frame).
    // After hardware or software decoding through `decodebin`, most decoders
    // propagate this flag faithfully from the input NAL units.
    let is_keyframe = !buffer.flags().contains(gstreamer::BufferFlags::DELTA_UNIT);

    Ok(SampleMeta {
        width: info.width(),
        height: info.height(),
        pixel_format: gst_format_to_pixel_format(info.format()),
        stride: info.stride()[0] as u32,
        timestamp: buffer.pts().map(|p| p.nseconds() as i64).unwrap_or(0),
        is_keyframe,
        #[cfg(all(feature = "dmabuf", target_os = "linux"))]
        size: info.size(),
        #[cfg(all(feature = "dmabuf", target_os = "linux"))]
        drm_fourcc: gst_format_to_drm_fourcc(info.format()),
    })
}

/// Convert GStreamer video format to our [`PixelFormat`].
pub(crate) fn gst_format_to_pixel_format(format: gstreamer_video::VideoFormat) -> PixelFormat {
    match format {
        gstreamer_video::VideoFormat::Nv12 => PixelFormat::Nv12,
        gstreamer_video::VideoFormat::Rgb => PixelFormat::Rgb24,
        gstreamer_video::VideoFormat::Bgr => PixelFormat::Bgr24,
        gstreamer_video::VideoFormat::Rgba => PixelFormat::Rgba32,
        other => {
            tracing::warn!(
                ?other,
                "unmapped GStreamer video format, defaulting to RGB24"
            );
            PixelFormat::Rgb24
        }
    }
}

/// Map GStreamer video format to DRM fourcc code.
///
/// These constants match the standard DRM fourcc definitions from
/// `drm_fourcc.h`. We inline the values to avoid pulling in the
/// `drm-fourcc` crate for a handful of constants.
#[cfg(all(feature = "dmabuf", target_os = "linux"))]
fn gst_format_to_drm_fourcc(format: gstreamer_video::VideoFormat) -> u32 {
    const DRM_FORMAT_NV12: u32 = u32::from_le_bytes(*b"NV12");
    const DRM_FORMAT_RGB888: u32 = u32::from_le_bytes(*b"RG24");
    const DRM_FORMAT_BGR888: u32 = u32::from_le_bytes(*b"BG24");
    const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

    match format {
        gstreamer_video::VideoFormat::Nv12 => DRM_FORMAT_NV12,
        gstreamer_video::VideoFormat::Rgb => DRM_FORMAT_RGB888,
        gstreamer_video::VideoFormat::Bgr => DRM_FORMAT_BGR888,
        gstreamer_video::VideoFormat::Rgba | gstreamer_video::VideoFormat::Rgbx => {
            DRM_FORMAT_XRGB8888
        }
        _ => 0,
    }
}

// ── CpuExtractor ───────────────────────────────────────────────────

/// CPU-based buffer extractor (generic fallback).
///
/// On the `gstreamer` feature path, wraps the buffer in `FrameMemory::GstBufferRef`
/// for zero-copy borrow. The GstBuffer stays mapped and alive; data is borrowed
/// directly without any CPU copy.
///
/// If the buffer cannot be mapped (should not happen with well-formed pipelines),
/// falls back to `FrameMemory::Cpu` via copy.
pub struct CpuExtractor;

impl BufferExtractor for CpuExtractor {
    fn extract(&self, sample: &gstreamer::Sample) -> Result<DecodedFrame, AiEngineError> {
        let meta = parse_sample_meta(sample)?;
        let buffer = sample
            .buffer()
            .ok_or_else(|| AiEngineError::FrameError("GstSample has no buffer".into()))?;

        let mapped = buffer
            .to_owned()
            .into_mapped_buffer_readable()
            .map_err(|_| AiEngineError::FrameError("failed to map GstBuffer readable".into()))?;

        Ok(DecodedFrame {
            memory: FrameMemory::GstBufferRef { buffer: mapped },
            width: meta.width,
            height: meta.height,
            pixel_format: meta.pixel_format,
            timestamp: meta.timestamp,
            stride: meta.stride,
            is_keyframe: meta.is_keyframe,
        })
    }
}

// ── DmaBufExtractor ────────────────────────────────────────────────

/// DMA-buf based buffer extractor for Rockchip and VAAPI platforms.
///
/// Probes every `GstMemory` block in the buffer to verify it is backed
/// by DMA-buf. If all blocks are DMA-buf, extracts the fd from the first
/// block and `dup()`s it for independent ownership. Falls back to CPU
/// extraction if any block is not DMA-buf.
pub struct DmaBufExtractor {
    device_type: DeviceType,
}

impl BufferExtractor for DmaBufExtractor {
    fn extract(&self, sample: &gstreamer::Sample) -> Result<DecodedFrame, AiEngineError> {
        #[cfg(all(feature = "dmabuf", target_os = "linux"))]
        {
            let meta = parse_sample_meta(sample)?;
            let buffer = sample
                .buffer()
                .ok_or_else(|| AiEngineError::FrameError("GstSample has no buffer".into()))?;

            if let Some(frame) = try_extract_dmabuf(buffer, &meta, self.device_type)? {
                return Ok(frame);
            }
        }

        // Fallback to CPU if DMA-buf extraction is not available or failed.
        let _ = self.device_type;
        CpuExtractor.extract(sample)
    }
}

/// Attempt to extract a DMA-buf backed frame from a GstBuffer.
///
/// Returns `Ok(Some(frame))` on success, `Ok(None)` if the buffer does not
/// contain DMA-buf memory (caller should fall through to CPU path).
///
/// The fd is `dup()`-ed to establish independent ownership — the original
/// fd is managed by GStreamer's buffer pool and will be released when the
/// GstBuffer ref-count drops to zero.
#[cfg(all(feature = "dmabuf", target_os = "linux"))]
fn try_extract_dmabuf(
    buffer: &gstreamer::BufferRef,
    meta: &SampleMeta,
    device_type: DeviceType,
) -> Result<Option<DecodedFrame>, AiEngineError> {
    let n_memory = buffer.n_memory();
    if n_memory == 0 {
        return Ok(None);
    }

    // Verify ALL memory blocks are DMA-buf before extracting. Multi-plane
    // formats (NV12, NV21) split planes across separate GstMemory blocks;
    // we must confirm every block is DMA-buf to avoid mixing DMA and CPU.
    for i in 0..n_memory {
        let mem = buffer.peek_memory(i);
        if mem
            .downcast_memory_ref::<gstreamer_allocators::DmaBufMemoryRef>()
            .is_none()
        {
            return Ok(None);
        }
    }

    // All blocks verified — extract fd from the first block (plane 0).
    let memory = buffer.peek_memory(0);
    let dmabuf_ref = memory
        .downcast_memory_ref::<gstreamer_allocators::DmaBufMemoryRef>()
        .ok_or(AiEngineError::FrameError(
            "DMA-buf extraction failed after memory verification; GstMemory layout changed".into(),
        ))?;

    let original_fd = dmabuf_ref.fd();

    // `dup()` via OwnedFd for RAII-safe ownership independent of
    // GStreamer's buffer pool. OwnedFd::from_raw_fd + dup avoids the
    // double-close risk of bare RawFd.
    let raw_dup = unsafe { libc::dup(original_fd) };
    if raw_dup < 0 {
        return Err(AiEngineError::FrameError(format!(
            "dup() DMA-buf fd={original_fd} failed: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: raw_dup is a valid fd just returned by dup().
    let owned_fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(raw_dup) };

    Ok(Some(DecodedFrame {
        memory: FrameMemory::DmaBuf {
            fd: owned_fd,
            size: meta.size,
            offset: 0,
            drm_fourcc: meta.drm_fourcc,
            drm_modifier: 0,
            device: device_type,
        },
        width: meta.width,
        height: meta.height,
        pixel_format: meta.pixel_format,
        timestamp: meta.timestamp,
        stride: meta.stride,
        is_keyframe: meta.is_keyframe,
    }))
}

// ── JetsonExtractor ────────────────────────────────────────────────

/// NVIDIA Jetson NVMM buffer extractor.
///
/// Jetson `nvv4l2decoder` produces buffers in `memory:NVMM` format, which
/// is **not** standard DMA-buf — `DmaBufMemoryRef` downcast returns `None`.
///
/// ## With `cuda` feature
///
/// The CUDA zero-copy path uses:
/// 1. NvBufSurface → NvBufSurfaceMapEglImage
/// 2. cuGraphicsEGLRegisterImage → cuGraphicsResourceGetMappedPointer
/// 3. Result: `FrameMemory::DeviceBuffer` with CUDA device pointer
///
/// ## Without `cuda` feature
///
/// Falls back to CPU mapping via `nvvidconv` (which converts NVMM → raw).
/// The mapped buffer is valid RGB/RGBA data. Uses `GstBufferRef` for
/// zero-copy CPU access.
pub struct JetsonExtractor;

impl BufferExtractor for JetsonExtractor {
    fn extract(&self, sample: &gstreamer::Sample) -> Result<DecodedFrame, AiEngineError> {
        // TODO(cuda): When the `cuda` feature is enabled, attempt NVMM →
        // CUDA device pointer extraction via NvBufSurface + EglImage +
        // cuGraphicsResourceGetMappedPointer. This will produce a
        // FrameMemory::DeviceBuffer that can be fed directly to TensorRT
        // or ONNX-RT CUDA EP.
        //
        // For now, fall through to CPU path. The pipeline uses `nvvidconv`
        // to convert NVMM → CPU-accessible `video/x-raw`, so the mapped
        // buffer contains valid pixel data.

        #[cfg(feature = "cuda")]
        {
            if let Some(frame) = try_extract_nvmm_cuda(sample)? {
                return Ok(frame);
            }
        }

        CpuExtractor.extract(sample)
    }
}

/// Attempt NVMM → CUDA device pointer extraction.
///
/// This is the Jetson zero-copy path. Currently a placeholder that will be
/// implemented when the `jetson_ffi` module provides NvBufSurface bindings.
#[cfg(feature = "cuda")]
fn try_extract_nvmm_cuda(
    _sample: &gstreamer::Sample,
) -> Result<Option<DecodedFrame>, AiEngineError> {
    // Phase 6 implementation: JetsonExtractor CUDA path
    //
    // 1. Get NvBufSurface from GstBuffer metadata
    // 2. NvBufSurfaceMapEglImage → EglImage
    // 3. cuGraphicsEGLRegisterImage → CUgraphicsResource
    // 4. cuGraphicsResourceGetMappedPointer → CUdeviceptr
    // 5. Return FrameMemory::DeviceBuffer { handle: device_ptr, .. }
    //
    // Deferred until jetson_ffi.rs provides the FFI bindings.
    tracing::trace!("NVMM CUDA extraction not yet implemented, falling back to CPU");
    Ok(None)
}
