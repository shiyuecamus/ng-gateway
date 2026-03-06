//! Unified frame memory abstraction for zero-copy pipelines.
//!
//! This module provides the core types enabling zero-copy video frame
//! processing across CPU, DMA-buf, GStreamer-managed, CUDA pinned, and
//! opaque device memory. Frame data may reside in any of these memory
//! domains; consumers (preprocessor, inference backend, annotator)
//! dispatch on the variant to choose the most efficient access path.

use ng_gateway_error::ai::AiEngineError;

/// Pixel format of a decoded frame.
///
/// Distinct from the encoded [`FrameFormat`](ng_gateway_models::enums::ai::FrameFormat)
/// (H264Nal, Jpeg, etc.), this describes the decoded pixel layout in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Planar Y + interleaved UV (hardware decoder native output).
    Nv12,
    /// 3 bytes per pixel, row-major, no stride padding.
    Rgb24,
    /// 3 bytes per pixel, BGR order (OpenCV convention).
    Bgr24,
    /// 4 bytes per pixel with alpha channel.
    Rgba32,
}

impl PixelFormat {
    /// Bytes per pixel for packed formats. NV12 is semi-planar and
    /// does not have a fixed bpp — use [`PixelFormat::frame_size`] instead.
    #[inline]
    pub fn bytes_per_pixel(&self) -> Option<usize> {
        match self {
            Self::Nv12 => None,
            Self::Rgb24 | Self::Bgr24 => Some(3),
            Self::Rgba32 => Some(4),
        }
    }

    /// Total byte size for a tightly-packed frame at the given dimensions.
    #[inline]
    pub fn frame_size(&self, width: u32, height: u32) -> usize {
        let pixels = width as usize * height as usize;
        match self {
            Self::Nv12 => pixels * 3 / 2,
            Self::Rgb24 | Self::Bgr24 => pixels * 3,
            Self::Rgba32 => pixels * 4,
        }
    }
}

/// Hardware device type for device-resident buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceType {
    /// Rockchip NPU (RKNN).
    RknnNpu,
    /// NVIDIA GPU (CUDA).
    NvidiaCuda,
    /// Intel GPU (VA-API).
    IntelVaapi,
    /// Generic DRM device.
    Drm,
}

/// Detected hardware platform for dispatch decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HardwarePlatform {
    /// Rockchip SoC (RK3588, RK3566, etc.) with MPP + RGA + RKNN.
    Rockchip,
    /// NVIDIA Jetson (Nano, Xavier, Orin) with NVDEC + CUDA + TensorRT.
    NvidiaJetson,
    /// NVIDIA desktop GPU (non-Jetson) with NVCODEC + CUDA.
    NvidiaDesktop,
    /// Intel/AMD with VA-API.
    Vaapi,
    /// Generic x86/ARM with no hardware acceleration.
    Generic,
}

/// Unified frame memory abstraction.
///
/// This is the central type enabling zero-copy pipelines. Frame data may
/// reside in CPU memory, DMA-shared device memory, GStreamer-managed
/// buffers, CUDA pinned memory, or opaque GPU memory. Consumers
/// (preprocessor, inference backend, annotator) dispatch on the variant
/// to choose the most efficient access path.
///
/// # Ownership & Lifetime
///
/// `FrameMemory` owns or holds a strong reference to the underlying buffer.
/// When dropped, the backing memory is released (or ref-count decremented).
/// For `DmaBuf`, the file descriptor is closed; for `Cpu`, the `Bytes` buffer
/// is freed; for `GstBufferRef`, the GstBuffer is unmapped and returned to
/// the GStreamer buffer pool.
///
/// # Thread Safety
///
/// All variants are `Send + Sync`. DMA-buf fds are plain integers safe to
/// share across threads. CPU buffers use `bytes::Bytes` (atomic ref-count).
/// `MappedBuffer` is `Send + Sync` by virtue of GStreamer's thread-safe
/// buffer model.
pub enum FrameMemory {
    /// CPU-resident contiguous pixel buffer.
    ///
    /// This is the fallback path: all legacy code works unchanged.
    /// `Bytes` provides zero-cost cloning via atomic reference counting.
    Cpu(bytes::Bytes),

    /// Linux DMA-buf exported memory.
    ///
    /// The buffer resides in device memory (VPU/GPU/NPU) and is identified
    /// by a file descriptor. Consumers that support DMA-buf import
    /// (e.g. `rknn_create_mem_from_fd`, EGL, VA-API) can use it directly
    /// without any CPU copy.
    ///
    /// **CPU access**: Possible via `mmap(fd)`, but strongly discouraged on
    /// the hot path due to cache coherence overhead. Use only for fallback
    /// paths (annotation, snapshot capture).
    #[cfg(feature = "dmabuf")]
    DmaBuf {
        /// DMA-buf file descriptor (owned; closed on drop).
        fd: std::os::unix::io::RawFd,
        /// Total buffer size in bytes.
        size: usize,
        /// Byte offset within the DMA-buf (typically 0).
        offset: u64,
        /// DRM fourcc format code for the pixel layout.
        drm_fourcc: u32,
        /// DRM format modifier (linear, tiled, etc.).
        drm_modifier: u64,
        /// Originating device type (for consumer dispatch).
        device: DeviceType,
    },

    /// GPU/NPU-managed opaque buffer.
    ///
    /// For CUDA mapped memory, TensorRT device buffers, or RKNN internal
    /// tensor memory. The `handle` is device-specific (CUDA device pointer,
    /// RKNN mem handle, etc.).
    DeviceBuffer {
        /// Opaque device-specific handle.
        handle: u64,
        /// Device type.
        device: DeviceType,
        /// Buffer size in bytes.
        size: usize,
    },

    /// GStreamer buffer held in mapped-readable state.
    ///
    /// Keeps the underlying `GstBuffer` alive and mapped so that pixel data
    /// can be borrowed without any CPU copy. The buffer is returned to
    /// GStreamer's buffer pool when this variant is dropped (unmap + unref).
    ///
    /// `as_cpu_slice()` yields a zero-copy borrow into the mapped region.
    /// `Clone` materializes to the `Cpu` variant via `Bytes::copy_from_slice`
    /// because GStreamer's mapped buffers cannot be shared across independent
    /// owners.
    #[cfg(feature = "gstreamer")]
    GstBufferRef {
        /// The mapped GstBuffer (held for lifetime management).
        buffer: gstreamer::MappedBuffer<gstreamer::buffer::Readable>,
    },

    /// CUDA page-locked (pinned) host memory.
    ///
    /// Allocated via `cudaMallocHost` / `cuMemAllocHost`. Accessible by
    /// both CPU and GPU without page faults. Used for inference I/O staging
    /// buffers where host-to-device and device-to-host transfers must be
    /// fast.
    ///
    /// On unified memory architectures (Jetson Orin), `device_ptr` may
    /// point to the same physical memory as `host_ptr`, enabling true
    /// zero-copy between CPU and GPU.
    #[cfg(feature = "cuda")]
    CudaPinned {
        /// Page-locked host pointer (from `cudaMallocHost`).
        host_ptr: *mut u8,
        /// Corresponding device pointer (from `cudaHostGetDevicePointer`,
        /// only valid on unified memory architectures).
        device_ptr: Option<u64>,
        /// Buffer size in bytes.
        size: usize,
    },
}

impl FrameMemory {
    /// Whether this memory can be accessed by the CPU without a device sync.
    #[inline]
    pub fn is_cpu_accessible(&self) -> bool {
        match self {
            Self::Cpu(_) => true,
            #[cfg(feature = "gstreamer")]
            Self::GstBufferRef { .. } => true,
            #[cfg(feature = "cuda")]
            Self::CudaPinned { .. } => true,
            _ => false,
        }
    }

    /// Whether this memory is a DMA-buf that NPU/GPU can import directly.
    #[inline]
    pub fn is_dma_buf(&self) -> bool {
        #[cfg(feature = "dmabuf")]
        {
            matches!(self, Self::DmaBuf { .. })
        }
        #[cfg(not(feature = "dmabuf"))]
        {
            false
        }
    }

    /// Extract DMA-buf file descriptor, size, and byte offset if this is a
    /// `DmaBuf` variant. Used by consumers that support zero-copy DMA import
    /// (e.g. RKNN `create_mem_from_fd`, VA-API).
    #[cfg(feature = "dmabuf")]
    #[inline]
    pub fn dma_fd_info(&self) -> Option<(std::os::unix::io::RawFd, usize, u64)> {
        match self {
            Self::DmaBuf {
                fd, size, offset, ..
            } => Some((*fd, *size, *offset)),
            _ => None,
        }
    }

    /// Whether this memory is an opaque device buffer.
    #[inline]
    pub fn is_device_buffer(&self) -> bool {
        matches!(self, Self::DeviceBuffer { .. })
    }

    /// Whether this memory is a GStreamer-managed mapped buffer.
    #[inline]
    pub fn is_gst_buffer_ref(&self) -> bool {
        #[cfg(feature = "gstreamer")]
        {
            matches!(self, Self::GstBufferRef { .. })
        }
        #[cfg(not(feature = "gstreamer"))]
        {
            false
        }
    }

    /// Get CPU-accessible byte slice.
    ///
    /// - `Cpu`: zero-cost borrow.
    /// - `GstBufferRef`: zero-cost borrow into the mapped GStreamer buffer.
    /// - `CudaPinned`: zero-cost borrow into the pinned host buffer.
    /// - `DmaBuf`: returns `None` (must use [`to_cpu`](Self::to_cpu)).
    /// - `DeviceBuffer`: returns `None` (must use device-specific APIs).
    #[inline]
    pub fn as_cpu_slice(&self) -> Option<&[u8]> {
        match self {
            Self::Cpu(bytes) => Some(bytes.as_ref()),
            #[cfg(feature = "gstreamer")]
            Self::GstBufferRef { buffer } => Some(buffer.as_slice()),
            #[cfg(feature = "cuda")]
            Self::CudaPinned { host_ptr, size, .. } => {
                if host_ptr.is_null() {
                    None
                } else {
                    // SAFETY: host_ptr was allocated by cudaMallocHost with `size` bytes
                    // and remains valid for the lifetime of this FrameMemory.
                    Some(unsafe { std::slice::from_raw_parts(*host_ptr, *size) })
                }
            }
            _ => None,
        }
    }

    /// Materialize to CPU memory if not already there.
    ///
    /// - `Cpu` → no-op clone (atomic ref-count increment).
    /// - `GstBufferRef` → copy mapped buffer to `Bytes`.
    /// - `CudaPinned` → copy pinned host buffer to `Bytes`.
    /// - `DmaBuf` → mmap + copy to Bytes (fallback path, use sparingly).
    /// - `DeviceBuffer` → not supported, returns error.
    pub fn to_cpu(&self) -> Result<bytes::Bytes, AiEngineError> {
        match self {
            Self::Cpu(bytes) => Ok(bytes.clone()),

            #[cfg(feature = "gstreamer")]
            Self::GstBufferRef { buffer } => Ok(bytes::Bytes::copy_from_slice(buffer.as_slice())),

            #[cfg(feature = "cuda")]
            Self::CudaPinned { host_ptr, size, .. } => {
                if host_ptr.is_null() {
                    return Err(AiEngineError::FrameError(
                        "CudaPinned host_ptr is null".into(),
                    ));
                }
                // SAFETY: host_ptr was allocated by cudaMallocHost with `size` bytes.
                let slice = unsafe { std::slice::from_raw_parts(*host_ptr, *size) };
                Ok(bytes::Bytes::copy_from_slice(slice))
            }

            #[cfg(feature = "dmabuf")]
            Self::DmaBuf {
                fd, size, offset, ..
            } => {
                // SAFETY: fd is valid (owned by this FrameMemory) and we
                // only read `size` bytes starting at `offset`.
                let mapped = unsafe {
                    let ptr = libc::mmap(
                        std::ptr::null_mut(),
                        *size,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        *fd,
                        *offset as libc::off_t,
                    );
                    if ptr == libc::MAP_FAILED {
                        return Err(AiEngineError::FrameError(format!(
                            "mmap DMA-buf fd={fd} failed: {}",
                            std::io::Error::last_os_error()
                        )));
                    }
                    let slice = std::slice::from_raw_parts(ptr as *const u8, *size);
                    let bytes = bytes::Bytes::copy_from_slice(slice);
                    libc::munmap(ptr, *size);
                    bytes
                };
                Ok(mapped)
            }

            Self::DeviceBuffer { device, .. } => Err(AiEngineError::FrameError(format!(
                "DeviceBuffer({device:?}) → CPU transfer not implemented"
            ))),
        }
    }

    /// Try to clone this memory into a logically independent handle.
    ///
    /// - **Cpu**: zero-cost clone via `Bytes` atomic ref-count.
    /// - **DmaBuf**: duplicates fd via `dup(fd)`; returns error if duplication fails.
    /// - **DeviceBuffer**: copies the opaque handle.
    /// - **GstBufferRef** / **CudaPinned**: materializes to `Cpu` variant via copy.
    pub fn try_clone(&self) -> Result<Self, AiEngineError> {
        match self {
            Self::Cpu(b) => Ok(Self::Cpu(b.clone())),

            #[cfg(feature = "dmabuf")]
            Self::DmaBuf {
                fd,
                size,
                offset,
                drm_fourcc,
                drm_modifier,
                device,
            } => {
                let new_fd = unsafe { libc::dup(*fd) };
                if new_fd < 0 {
                    return Err(AiEngineError::FrameError(format!(
                        "dup(DMA-buf fd={fd}) failed: {}",
                        std::io::Error::last_os_error()
                    )));
                }
                Ok(Self::DmaBuf {
                    fd: new_fd,
                    size: *size,
                    offset: *offset,
                    drm_fourcc: *drm_fourcc,
                    drm_modifier: *drm_modifier,
                    device: *device,
                })
            }

            Self::DeviceBuffer {
                handle,
                device,
                size,
            } => Ok(Self::DeviceBuffer {
                handle: *handle,
                device: *device,
                size: *size,
            }),

            #[cfg(feature = "gstreamer")]
            Self::GstBufferRef { buffer } => {
                Ok(Self::Cpu(bytes::Bytes::copy_from_slice(buffer.as_slice())))
            }

            #[cfg(feature = "cuda")]
            Self::CudaPinned { host_ptr, size, .. } => {
                if host_ptr.is_null() || *size == 0 {
                    Ok(Self::Cpu(bytes::Bytes::new()))
                } else {
                    // SAFETY: host_ptr was allocated by cudaMallocHost with `size` bytes.
                    let slice = unsafe { std::slice::from_raw_parts(*host_ptr, *size) };
                    Ok(Self::Cpu(bytes::Bytes::copy_from_slice(slice)))
                }
            }
        }
    }
}

impl std::fmt::Debug for FrameMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cpu(b) => f
                .debug_struct("FrameMemory::Cpu")
                .field("len", &b.len())
                .finish(),

            #[cfg(feature = "dmabuf")]
            Self::DmaBuf {
                fd, size, device, ..
            } => f
                .debug_struct("FrameMemory::DmaBuf")
                .field("fd", fd)
                .field("size", size)
                .field("device", device)
                .finish(),

            Self::DeviceBuffer {
                handle,
                device,
                size,
            } => f
                .debug_struct("FrameMemory::DeviceBuffer")
                .field("handle", handle)
                .field("device", device)
                .field("size", size)
                .finish(),

            #[cfg(feature = "gstreamer")]
            Self::GstBufferRef { buffer } => f
                .debug_struct("FrameMemory::GstBufferRef")
                .field("len", &buffer.len())
                .finish(),

            #[cfg(feature = "cuda")]
            Self::CudaPinned { size, .. } => f
                .debug_struct("FrameMemory::CudaPinned")
                .field("size", size)
                .finish(),
        }
    }
}

impl Drop for FrameMemory {
    fn drop(&mut self) {
        match self {
            #[cfg(feature = "dmabuf")]
            Self::DmaBuf { fd, .. } => {
                // SAFETY: fd is owned by this instance and has not been closed.
                unsafe {
                    libc::close(*fd);
                }
            }
            #[cfg(feature = "cuda")]
            Self::CudaPinned { host_ptr, .. } => {
                if !host_ptr.is_null() {
                    // SAFETY: host_ptr was allocated by cudaMallocHost and has
                    // not been freed. We own it exclusively.
                    //
                    // NOTE: actual cudaFreeHost call is deferred to the CUDA
                    // runtime wrapper — this placeholder logs a warning if the
                    // pointer leaks. The caller must use the CUDA allocator's
                    // drop path to free pinned memory properly.
                    tracing::trace!(
                        ptr = ?*host_ptr,
                        "CudaPinned dropped — caller must ensure cudaFreeHost"
                    );
                }
            }
            _ => {}
        }
    }
}

// SAFETY: DMA-buf fds are plain integers safe to share across threads.
// CPU buffers use Bytes (atomic ref-count). DeviceBuffer handles are
// opaque integers — actual device synchronization is the consumer's
// responsibility. GStreamer MappedBuffer is Send+Sync by design.
// CudaPinned host_ptr is a raw pointer to page-locked memory that does
// not change address — safe to share across threads.
unsafe impl Send for FrameMemory {}
unsafe impl Sync for FrameMemory {}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn pixel_format_bytes_per_pixel() {
        assert_eq!(PixelFormat::Nv12.bytes_per_pixel(), None);
        assert_eq!(PixelFormat::Rgb24.bytes_per_pixel(), Some(3));
        assert_eq!(PixelFormat::Bgr24.bytes_per_pixel(), Some(3));
        assert_eq!(PixelFormat::Rgba32.bytes_per_pixel(), Some(4));
    }

    #[test]
    fn pixel_format_frame_size_rgb24() {
        assert_eq!(PixelFormat::Rgb24.frame_size(1920, 1080), 1920 * 1080 * 3);
    }

    #[test]
    fn pixel_format_frame_size_nv12() {
        assert_eq!(
            PixelFormat::Nv12.frame_size(1920, 1080),
            1920 * 1080 * 3 / 2
        );
    }

    #[test]
    fn cpu_memory_is_accessible() {
        let mem = FrameMemory::Cpu(Bytes::from_static(b"pixels"));

        assert!(mem.is_cpu_accessible());
        assert!(!mem.is_device_buffer());
        assert!(!mem.is_dma_buf());
    }

    #[test]
    fn cpu_as_slice_returns_data() {
        let raw = b"RGB";
        let mem = FrameMemory::Cpu(Bytes::from_static(raw));

        let slice = mem.as_cpu_slice().expect("Cpu variant should return Some");
        assert_eq!(slice, raw);
    }

    #[test]
    fn cpu_to_cpu_identity() {
        let raw = vec![1u8, 2, 3, 4, 5];
        let mem = FrameMemory::Cpu(Bytes::from(raw.clone()));

        let out = mem.to_cpu().expect("Cpu → Cpu should succeed");
        assert_eq!(out.as_ref(), raw.as_slice());
    }

    #[test]
    fn try_clone_cpu_is_refcount_clone() {
        let mem = FrameMemory::Cpu(Bytes::from_static(b"abc"));
        let cloned = mem.try_clone().expect("CPU try_clone should succeed");
        let cloned_slice = cloned
            .as_cpu_slice()
            .expect("CPU clone should provide CPU slice");
        assert_eq!(cloned_slice, b"abc");
    }

    #[test]
    fn device_buffer_not_cpu_accessible() {
        let mem = FrameMemory::DeviceBuffer {
            handle: 0xDEAD,
            device: DeviceType::RknnNpu,
            size: 1024,
        };

        assert!(!mem.is_cpu_accessible());
        assert!(mem.is_device_buffer());
        assert!(mem.as_cpu_slice().is_none());
    }

    #[test]
    fn device_buffer_to_cpu_returns_error() {
        let mem = FrameMemory::DeviceBuffer {
            handle: 0xBEEF,
            device: DeviceType::NvidiaCuda,
            size: 512,
        };

        assert!(mem.to_cpu().is_err());
    }
}
