//! Public log bridge contract for `cdylib` libraries (drivers + plugins).
//!
//! # Goals
//! - Provide a **single** stable module (`ng_gateway_sdk::log`) for all cross-boundary log contracts.
//! - Keep schema + ABI types in one place (`fields`, `LogSinkV1`).
//! - Keep southward driver macro contract stable (`ng_driver_factory!` expects `$crate::log::*`).
//!
//! # Notes
//! - Do NOT re-export northward plugin bridge functions here (name collisions with driver bridge).
//!   Plugin-side bridge remains under `ng_gateway_sdk::northward::log`.

use serde_json::{Map, Value};
use std::ffi::c_void;

/// Shared log field keys used by the unified logging bridge.
///
/// Keep these as a single source of truth so host/driver/plugin agree on the schema.
pub mod fields {
    use super::{Map, Value};

    /// Field key for channel attribution.
    pub const CHANNEL_ID: &str = "channel_id";
    /// Field key for app attribution (northward plugin span context).
    pub const APP_ID: &str = "app_id";
    /// Synthetic field key used by `tracing` for event body.
    pub const MESSAGE: &str = "message";

    /// Field key that marks a log as originating from a driver/plugin/host.
    pub const SOURCE: &str = "source";
    /// The `SOURCE` value used by the driver->host bridge.
    pub const SOURCE_DRIVER: &str = "driver";
    /// The `SOURCE` value used by the plugin->host bridge.
    pub const SOURCE_PLUGIN: &str = "plugin";
    /// The `SOURCE` value used by host logs (optional; host may omit this field).
    pub const SOURCE_HOST: &str = "host";

    /// Field key for driver type attribution (e.g. "modbus", "opcua", "s7").
    pub const DRIVER_TYPE: &str = "driver_type";
    /// Field key for plugin type attribution (e.g. "kafka", "pulsar").
    pub const PLUGIN_TYPE: &str = "plugin_type";

    /// Stable tracing target for driver logs re-emitted by the host.
    pub const TARGET_DRIVER: &str = "driver";
    /// Stable tracing target for plugin logs re-emitted by the host.
    pub const TARGET_PLUGIN: &str = "plugin";

    /// Stable span name used by the host-side driver ingest bridge.
    pub const SPAN_DRIVER_LOG: &str = "driver";
    /// Stable span name used by the host-side plugin ingest bridge.
    pub const SPAN_PLUGIN_LOG: &str = "plugin";

    /// Extract an `i32` from a JSON map field.
    #[inline]
    pub fn map_i32(map: &Map<String, Value>, key: &str) -> Option<i32> {
        map.get(key)
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok())
    }
}

/// ABI version for `LogSinkV1`.
pub const LOG_SINK_ABI_V1: u32 = 1;

/// Log sink emit function signature.
pub type LogEmitFn = extern "C" fn(user_data: *mut c_void, ptr: *const u8, len: usize);

/// Optional sink flush function signature.
pub type LogFlushFn = extern "C" fn(user_data: *mut c_void);

/// A stable log sink ABI for `cdylib` -> host log streaming.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LogSinkV1 {
    pub abi_version: u32,
    pub user_data: *mut c_void,
    pub emit_json: LogEmitFn,
    pub emit_batch_json: Option<LogEmitFn>,
    pub flush: Option<LogFlushFn>,
}

// SAFETY:
// `LogSinkV1` is a plain C-ABI struct containing raw pointers and function pointers.
// The host/cdylib contract ensures `user_data` remains valid for the duration of usage.
unsafe impl Send for LogSinkV1 {}
unsafe impl Sync for LogSinkV1 {}

// --- Southward driver bridge API (stable for `ng_driver_factory!`) ---
pub use crate::southward::log::{
    get_max_level, init_driver_tracing, set_log_sink, set_max_level, DriverLogBridgeConfig,
};
