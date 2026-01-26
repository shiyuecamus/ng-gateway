//! Host-side dynamic driver loader (core-owned).
//!
//! This module centralizes all `cdylib` driver loading responsibilities in the host process.

pub mod loader;

pub use loader::{DriverLoader, DriverProbeInfo, DriverRegistry};
