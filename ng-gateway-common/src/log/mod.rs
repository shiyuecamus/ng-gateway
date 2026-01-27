//! Unified logging module entrypoint (single source of truth).
//!
//! This module defines the process-wide logging subsystem for the gateway:
//! - Console + rolling file
//! - Runtime log level control (global + per-channel with TTL)
//! - Split file logging by driver type

pub mod control;
pub mod driver;
pub mod host;
pub mod split_file;

pub use host::Logger;
pub use split_file::{DriverFileRegistry, DriverTypeExtractorLayer, SplitFileLayer};

use ng_gateway_error::NGResult;
use ng_gateway_models::settings::Logging as LoggingSettings;

/// Initialize logging runtime and install the global tracing subscriber.
///
/// # Important
/// This must run exactly once per process, before any heavy subsystems start.
pub fn init_runtime(logger: &mut Logger, logging: &LoggingSettings) -> NGResult<()> {
    // Convert config to log control settings and initialize runtime.
    control::init((*logging).into())?;
    logger.initialize()?;
    Ok(())
}
