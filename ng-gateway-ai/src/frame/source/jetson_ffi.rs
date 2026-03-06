//! Minimal FFI bindings for NVIDIA Jetson NvBufSurface API.
//!
//! These types and functions are used by [`JetsonExtractor`](super::extractor::JetsonExtractor)
//! to extract CUDA device pointers from NVMM GStreamer buffers on Jetson
//! platforms. The bindings are loaded at runtime via `libloading` from
//! `libnvbuf_utils.so` — no compile-time CUDA SDK dependency.
//!
//! # Safety
//!
//! All types here mirror the C structures from NVIDIA's `nvbufsurface.h`.
//! They must be kept in sync with the Jetson L4T version deployed on the
//! target system. The current definitions target JetPack 5.x / 6.x
//! (L4T R35.x / R36.x).
//!
//! # References
//!
//! - NVIDIA DeepStream SDK: `nvbufsurface.h`
//! - JetPack documentation: NvBufSurface API

#![allow(non_camel_case_types, dead_code)]

/// NvBufSurface memory type.
///
/// Determines how the buffer is allocated and which APIs can access it.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvBufSurfaceMemType {
    /// Default memory type — system selects optimal allocation.
    Default = 0,
    /// CUDA Host (pinned) memory.
    CudaHost = 1,
    /// CUDA Device memory.
    CudaDevice = 2,
    /// CUDA Unified memory (accessible by both CPU and GPU).
    CudaUnified = 3,
    /// NVMM surface array — Jetson's default for decoder output.
    SurfaceArray = 4,
}

/// NvBufSurface color format.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NvBufSurfaceColorFormat {
    Invalid = 0,
    Rgba = 1,
    Bgra = 2,
    Argb = 3,
    Abgr = 4,
    Rgbx = 5,
    Bgrx = 6,
    Xrgb = 7,
    Xbgr = 8,
    Nv12 = 19,
    Nv21 = 20,
    Nv12_10le = 21,
}

/// Per-surface parameters within an NvBufSurface.
///
/// Each surface in the batch has its own set of parameters describing
/// dimensions, pitch, color format, and memory layout.
#[repr(C)]
#[derive(Debug)]
pub struct NvBufSurfaceParams {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub color_format: NvBufSurfaceColorFormat,
    pub layout: u32,
    pub buf_fd: i32,
    pub data_size: u32,
    pub data_ptr: *mut std::ffi::c_void,
    // Remaining fields are platform-specific and not needed for our use case.
    _padding: [u8; 64],
}

/// Top-level NvBufSurface structure.
///
/// Contains a batch of surfaces, each described by [`NvBufSurfaceParams`].
/// For our use case (single-frame appsink extraction), `batch_size` is
/// typically 1.
#[repr(C)]
#[derive(Debug)]
pub struct NvBufSurface {
    pub gpu_id: u32,
    pub batch_size: u32,
    pub num_filled: u32,
    pub is_contiguous: bool,
    pub mem_type: NvBufSurfaceMemType,
    pub surface_list: *mut NvBufSurfaceParams,
    // Additional fields exist but are not needed.
    _padding: [u8; 64],
}

/// Runtime-loaded NvBufSurface API functions.
///
/// Loaded via `libloading` from `libnvbuf_utils.so` at runtime.
/// This avoids any compile-time dependency on CUDA or L4T libraries.
#[cfg(all(feature = "cuda", target_os = "linux"))]
pub struct NvBufSurfaceApi {
    _lib: libloading::Library,
    /// `int NvBufSurfaceMapEglImage(NvBufSurface *surf, int index)`
    pub map_egl_image: unsafe extern "C" fn(*mut NvBufSurface, i32) -> i32,
    /// `int NvBufSurfaceUnMapEglImage(NvBufSurface *surf, int index)`
    pub unmap_egl_image: unsafe extern "C" fn(*mut NvBufSurface, i32) -> i32,
}

#[cfg(all(feature = "cuda", target_os = "linux"))]
impl NvBufSurfaceApi {
    /// Load the NvBufSurface API from the system library.
    ///
    /// # Safety
    ///
    /// The library must be compatible with the running Jetson L4T version.
    pub unsafe fn load() -> Result<Self, String> {
        let lib = unsafe {
            libloading::Library::new("libnvbuf_utils.so")
                .map_err(|e| format!("failed to load libnvbuf_utils.so: {e}"))?
        };

        let map_egl_image = unsafe {
            *lib.get::<unsafe extern "C" fn(*mut NvBufSurface, i32) -> i32>(
                b"NvBufSurfaceMapEglImage",
            )
            .map_err(|e| format!("symbol NvBufSurfaceMapEglImage not found: {e}"))?
        };

        let unmap_egl_image = unsafe {
            *lib.get::<unsafe extern "C" fn(*mut NvBufSurface, i32) -> i32>(
                b"NvBufSurfaceUnMapEglImage",
            )
            .map_err(|e| format!("symbol NvBufSurfaceUnMapEglImage not found: {e}"))?
        };

        Ok(Self {
            _lib: lib,
            map_egl_image,
            unmap_egl_image,
        })
    }
}
