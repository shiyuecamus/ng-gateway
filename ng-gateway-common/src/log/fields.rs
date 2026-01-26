//! Shared log field keys and helpers.
//!
//! This module centralizes commonly used JSON field keys for log attribution across:
//! - Host realtime log layer
//! - Driver->host re-emit bridge
//!
//! Keeping keys in one place prevents typos and makes schema evolution (e.g. supporting string ids)
//! straightforward.

use serde_json::{Map, Value};

/// Field key for channel attribution.
pub const CHANNEL_ID: &str = "channel_id";

/// Extract an `i32` from a JSON map field.
///
/// # Notes
/// Current encoding expects a JSON integer (`i64`) that fits into `i32`.
/// If in the future we want to accept strings, this function is the single place to extend.
#[inline]
pub fn map_i32(map: &Map<String, Value>, key: &str) -> Option<i32> {
    map.get(key)
        .and_then(|v| v.as_i64())
        .and_then(|v| i32::try_from(v).ok())
}
