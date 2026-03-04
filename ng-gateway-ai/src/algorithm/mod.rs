//! Custom algorithm hosting (WASM).
//!
//! Provides a dual-mode WASM runtime for user-defined AI pipeline stages:
//!
//! - **`FrameTransform`** — pixel-level image transforms applied before inference
//!   (e.g., edge detection, background subtraction, contrast enhancement)
//! - **`ResultProcessor`** — structured result filtering/enrichment applied after
//!   inference (e.g., PPE compliance checks, counting logic, business rules)
//!
//! # Architecture
//!
//! Types (`WasmModuleType`, `WasmAlgorithmInfo`, ABI structs) are always available
//! so that camera drivers and API handlers can reference them without pulling in
//! the wasmtime dependency.  The actual `WasmAlgorithmHost` runtime is gated
//! behind the `engine` feature.

#[cfg(feature = "engine")]
pub mod host;

#[cfg(feature = "engine")]
pub mod registry;

#[cfg(feature = "engine")]
pub use host::WasmAlgorithmHost;
#[cfg(feature = "engine")]
pub use registry::AlgorithmRegistry;
