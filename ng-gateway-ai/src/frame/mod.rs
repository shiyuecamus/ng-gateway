//! Video frame acquisition, decoding, and memory management.
//!
//! This module contains:
//! - [`memory`]: Unified frame memory abstraction (`FrameMemory`, `PixelFormat`, etc.)
//! - [`pool`]: Pre-allocated CPU buffer pool for fallback preprocessing paths
//! - [`source`]: GStreamer-based frame source (replaces decode, engine + gstreamer)
//! - [`platform`]: Hardware platform detection and capability probing

pub mod memory;

#[cfg(feature = "engine")]
pub mod pool;

#[cfg(feature = "gstreamer")]
pub mod platform;

#[cfg(feature = "gstreamer")]
pub mod source;
