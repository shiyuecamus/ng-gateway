//! Unified logging module entrypoint (single source of truth).
//!
//! This module defines the process-wide logging subsystem for the gateway:
//! - Console + rolling file
//! - Runtime log level control (global + per-channel with TTL)
//! - Split file logging by driver type

pub mod cleanup;
pub mod control;
pub mod driver;
pub mod host;
pub mod span_ext;
pub mod split_file;

pub use cleanup::{cleanup_logs_once, spawn_cleanup_worker, CleanupReport};
pub use host::Logger;
pub use span_ext::{ChannelIdExt, ChannelIdLayer};
pub use split_file::{DriverFileRegistry, DriverTypeExtractorLayer, SplitFileLayer};

use ng_gateway_error::NGResult;
use ng_gateway_models::settings::Settings;
use tokio_util::sync::CancellationToken;

/// Initialize logging subsystem (control + output + cleanup worker).
///
/// # Important
/// This must run exactly once per process, before any heavy subsystems start.
#[inline]
pub fn init_log(
    logger: &mut Logger,
    settings: &Settings,
    shutdown: CancellationToken,
) -> NGResult<()> {
    control::init(settings.logging.control.clone().into())?;
    let output = settings.logging.output.get();
    logger.initialize(&output)?;

    // Start background cleanup worker (best-effort, controlled by settings.logging.cleanup).
    spawn_cleanup_worker(settings.clone(), shutdown);
    Ok(())
}
