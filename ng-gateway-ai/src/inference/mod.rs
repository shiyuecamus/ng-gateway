//! Model inference — multi-backend routing and session management.

#[cfg(feature = "engine")]
pub mod backend;

#[cfg(feature = "engine")]
pub mod onnx;

#[cfg(feature = "engine")]
pub mod rknn;
