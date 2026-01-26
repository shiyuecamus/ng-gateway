//! Unified logging module entrypoint (single source of truth).
//!
//! This module defines the process-wide logging subsystem for the gateway:
//! - Console + rolling file
//! - Realtime logs (tail+follow for Web UI), hot-pluggable at runtime
//! - Override leases (TTL)

pub mod driver;
pub mod fields;
pub mod host;
pub mod realtime;
pub mod runtime;

pub use host::Logger;

use ng_gateway_error::NGResult;
use ng_gateway_models::settings::RealtimeLogs as RealtimeLogsSettings;

/// Initialize logging runtime and install the global tracing subscriber.
///
/// # Important
/// This must run exactly once per process, before any heavy subsystems start.
pub fn init_runtime(logger: &mut Logger, realtime: &RealtimeLogsSettings) -> NGResult<()> {
    runtime::init(*realtime)?;
    logger.initialize()?;
    Ok(())
}
