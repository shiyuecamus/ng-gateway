//! ThingsBoard Gateway API payload builder (hot path).
//!
//! This module centralizes the JSON framing bytes for TB Gateway API messages.
//! The hot path in `handle.rs` should not be cluttered with ad-hoc "magic" byte
//! literals; instead it calls the helpers here.
//!
//! # Performance
//! - The prefix is built once per publish batch and is reused for each chunk.
//! - We intentionally write framing bytes directly to `Vec<u8>` for minimal overhead.
//! - All escaping is delegated to `serde_json` to ensure correctness.

use ng_gateway_sdk::{NGValue, NorthwardError, NorthwardResult};
use std::io::Write as _;
use tracing::warn;

// NOTE: keep framing bytes private to avoid leaking "magic values" into call sites.

/// Fixed bytes after the device key for telemetry.
///
/// Telemetry payload shape:
/// `{ "<device>": [ { "ts": <ms>, "values": { ... } } ] }`
const TELEMETRY_AFTER_DEVICE: &[u8] = br#":[{"ts":"#;

/// Fixed bytes after the `ts` value for telemetry.
const TELEMETRY_AFTER_TS: &[u8] = br#","values":{"#;

/// Telemetry payload suffix bytes closing `values`, entry, array, and root object.
const TELEMETRY_SUFFIX: &[u8] = br#"}}]}"#;

/// Fixed bytes after the device key for attributes.
///
/// Attributes payload shape:
/// `{ "<device>": { ... } }`
const ATTRIBUTES_AFTER_DEVICE: &[u8] = br#":{"#;

/// Attributes payload suffix bytes closing the device object and root object.
const ATTRIBUTES_SUFFIX: &[u8] = br#"}}"#;

/// Serialize a JSON string (including escaping) into the buffer.
#[inline]
pub fn write_json_str(buf: &mut Vec<u8>, s: &str) -> NorthwardResult<()> {
    serde_json::to_writer(buf, s).map_err(|e| NorthwardError::SerializationError {
        reason: e.to_string(),
    })
}

/// Serialize an `NGValue` into JSON bytes without building a `serde_json::Value`.
///
/// # Semantics
/// Uses `NGValue`'s `Serialize` implementation (default semantics), aligned with
/// `NGValue::to_json_value(NGValueJsonOptions::default())`.
#[inline]
pub fn write_ng_value(buf: &mut Vec<u8>, v: &NGValue) -> NorthwardResult<()> {
    serde_json::to_writer(buf, v).map_err(|e| NorthwardError::SerializationError {
        reason: e.to_string(),
    })
}

/// Build telemetry prefix bytes into `buf`.
///
/// Prefix:
/// `{ "<device>":[{"ts":<ts_ms>,"values":{`
pub fn build_telemetry_prefix(
    buf: &mut Vec<u8>,
    device_name: &str,
    ts_ms: i64,
) -> NorthwardResult<()> {
    buf.clear();
    buf.push(b'{');
    write_json_str(buf, device_name)?;
    buf.extend_from_slice(TELEMETRY_AFTER_DEVICE);
    write!(buf, "{ts_ms}").map_err(|e| NorthwardError::SerializationError {
        reason: e.to_string(),
    })?;
    buf.extend_from_slice(TELEMETRY_AFTER_TS);
    Ok(())
}

/// Build attributes prefix bytes into `buf`.
///
/// Prefix:
/// `{ "<device>":{`
pub fn build_attributes_prefix(buf: &mut Vec<u8>, device_name: &str) -> NorthwardResult<()> {
    buf.clear();
    buf.push(b'{');
    write_json_str(buf, device_name)?;
    buf.extend_from_slice(ATTRIBUTES_AFTER_DEVICE);
    Ok(())
}

/// Incremental chunker for ThingsBoard gateway telemetry payloads.
///
/// It guarantees that every returned chunk is `<= max_payload_bytes`.
pub struct TelemetryChunker {
    max_payload_bytes: usize,
    prefix: Vec<u8>,
    payload: Vec<u8>,
    out: Vec<u8>,
    scratch_key: Vec<u8>,
    scratch_val: Vec<u8>,
    wrote_any: bool,
}

impl TelemetryChunker {
    /// Create a new telemetry chunker.
    ///
    /// # Arguments
    /// - `device_name`: sub-device name in TB gateway API.
    /// - `ts_ms`: telemetry timestamp in milliseconds.
    /// - `max_payload_bytes`: hard cap for the serialized JSON bytes.
    pub fn new(device_name: &str, ts_ms: i64, max_payload_bytes: usize) -> NorthwardResult<Self> {
        let max_payload_bytes = max_payload_bytes.max(256);
        let mut prefix = Vec::with_capacity(128);
        build_telemetry_prefix(&mut prefix, device_name, ts_ms)?;

        if prefix.len() + TELEMETRY_SUFFIX.len() > max_payload_bytes {
            return Err(NorthwardError::ConfigurationError {
                message: format!(
                    "communication.max_payload_bytes too small: prefix({})+suffix({}) > {}",
                    prefix.len(),
                    TELEMETRY_SUFFIX.len(),
                    max_payload_bytes
                ),
            });
        }

        let mut payload = Vec::with_capacity(max_payload_bytes.min(64 * 1024));
        payload.extend_from_slice(&prefix);
        let out = Vec::with_capacity(payload.capacity());

        Ok(Self {
            max_payload_bytes,
            prefix,
            payload,
            out,
            scratch_key: Vec::with_capacity(64),
            scratch_val: Vec::with_capacity(64),
            wrote_any: false,
        })
    }

    /// Push one key/value pair.
    ///
    /// Returns `Some(chunk_bytes)` when the current chunk was flushed due to size limit.
    /// The current entry is always attempted to be added after flushing. If even a single
    /// entry cannot fit, it is dropped with a warning.
    pub fn push(&mut self, key: &str, value: &NGValue) -> NorthwardResult<Option<Vec<u8>>> {
        self.scratch_key.clear();
        self.scratch_val.clear();
        write_json_str(&mut self.scratch_key, key)?;
        write_ng_value(&mut self.scratch_val, value)?;

        let comma_len = if self.wrote_any { 1 } else { 0 };
        let entry_len = comma_len + self.scratch_key.len() + 1 + self.scratch_val.len();

        // If doesn't fit, flush current (if non-empty) and try again.
        if self.payload.len() + entry_len + TELEMETRY_SUFFIX.len() > self.max_payload_bytes {
            let flushed = if self.wrote_any {
                self.payload.extend_from_slice(TELEMETRY_SUFFIX);
                // Double-buffer swap: preserve payload capacity; publish buffer can be recycled.
                std::mem::swap(&mut self.payload, &mut self.out);
                let out = std::mem::take(&mut self.out);
                self.payload.clear();
                self.payload.extend_from_slice(&self.prefix);
                self.wrote_any = false;
                Some(out)
            } else {
                None
            };

            // Re-check for single-entry fit.
            if self.payload.len() + entry_len + TELEMETRY_SUFFIX.len() > self.max_payload_bytes {
                return Ok(flushed);
            }

            // Add entry after flush (fallthrough to append).
            if self.wrote_any {
                self.payload.push(b',');
            }
            self.payload.extend_from_slice(&self.scratch_key);
            self.payload.push(b':');
            self.payload.extend_from_slice(&self.scratch_val);
            self.wrote_any = true;
            return Ok(flushed);
        }

        if self.wrote_any {
            self.payload.push(b',');
        }
        self.payload.extend_from_slice(&self.scratch_key);
        self.payload.push(b':');
        self.payload.extend_from_slice(&self.scratch_val);
        self.wrote_any = true;
        Ok(None)
    }

    /// Finish the current chunk (if any).
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if !self.wrote_any {
            return None;
        }
        self.payload.extend_from_slice(TELEMETRY_SUFFIX);
        Some(self.payload)
    }
}

/// Incremental chunker for ThingsBoard gateway attributes payloads.
///
/// It guarantees that every returned chunk is `<= max_payload_bytes`.
pub struct AttributesChunker {
    device_name: String,
    max_payload_bytes: usize,
    prefix: Vec<u8>,
    payload: Vec<u8>,
    out: Vec<u8>,
    scratch_key: Vec<u8>,
    scratch_val: Vec<u8>,
    wrote_any: bool,
}

impl AttributesChunker {
    /// Create a new attributes chunker.
    pub fn new(device_name: &str, max_payload_bytes: usize) -> NorthwardResult<Self> {
        let max_payload_bytes = max_payload_bytes.max(256);
        let mut prefix = Vec::with_capacity(64);
        build_attributes_prefix(&mut prefix, device_name)?;

        if prefix.len() + ATTRIBUTES_SUFFIX.len() > max_payload_bytes {
            return Err(NorthwardError::ConfigurationError {
                message: format!(
                    "communication.max_payload_bytes too small: prefix({})+suffix({}) > {}",
                    prefix.len(),
                    ATTRIBUTES_SUFFIX.len(),
                    max_payload_bytes
                ),
            });
        }

        let mut payload = Vec::with_capacity(max_payload_bytes.min(64 * 1024));
        payload.extend_from_slice(&prefix);
        let out = Vec::with_capacity(payload.capacity());

        Ok(Self {
            device_name: device_name.to_string(),
            max_payload_bytes,
            prefix,
            payload,
            out,
            scratch_key: Vec::with_capacity(64),
            scratch_val: Vec::with_capacity(64),
            wrote_any: false,
        })
    }

    /// Push one key/value pair. Returns `Some(chunk_bytes)` when a flush happened.
    pub fn push(&mut self, key: &str, value: &NGValue) -> NorthwardResult<Option<Vec<u8>>> {
        self.scratch_key.clear();
        self.scratch_val.clear();
        write_json_str(&mut self.scratch_key, key)?;
        write_ng_value(&mut self.scratch_val, value)?;

        let comma_len = if self.wrote_any { 1 } else { 0 };
        let entry_len = comma_len + self.scratch_key.len() + 1 + self.scratch_val.len();

        if self.payload.len() + entry_len + ATTRIBUTES_SUFFIX.len() > self.max_payload_bytes {
            let flushed = if self.wrote_any {
                self.payload.extend_from_slice(ATTRIBUTES_SUFFIX);
                std::mem::swap(&mut self.payload, &mut self.out);
                let out = std::mem::take(&mut self.out);
                self.payload.clear();
                self.payload.extend_from_slice(&self.prefix);
                self.wrote_any = false;
                Some(out)
            } else {
                None
            };

            if self.payload.len() + entry_len + ATTRIBUTES_SUFFIX.len() > self.max_payload_bytes {
                warn!(
                    device = self.device_name.as_str(),
                    key,
                    entry_bytes = entry_len,
                    max_payload_bytes = self.max_payload_bytes,
                    "ThingsBoard attributes entry too large; dropping the point"
                );
                return Ok(flushed);
            }

            if self.wrote_any {
                self.payload.push(b',');
            }
            self.payload.extend_from_slice(&self.scratch_key);
            self.payload.push(b':');
            self.payload.extend_from_slice(&self.scratch_val);
            self.wrote_any = true;
            return Ok(flushed);
        }

        if self.wrote_any {
            self.payload.push(b',');
        }
        self.payload.extend_from_slice(&self.scratch_key);
        self.payload.push(b':');
        self.payload.extend_from_slice(&self.scratch_val);
        self.wrote_any = true;
        Ok(None)
    }

    /// Finish the current chunk (if any).
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if !self.wrote_any {
            return None;
        }
        self.payload.extend_from_slice(ATTRIBUTES_SUFFIX);
        Some(self.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn telemetry_prefix_suffix_shapes_match_tb_gateway_api() {
        let mut c = TelemetryChunker::new("Device A", 1483228800000, 10 * 1024).unwrap();
        c.push("temperature", &NGValue::Int64(42)).unwrap();
        c.push("humidity", &NGValue::Int64(80)).unwrap();
        let bytes = c.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert!(parsed.get("Device A").is_some());
        let arr = parsed.get("Device A").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let entry = arr[0].as_object().unwrap();
        assert_eq!(entry.get("ts").unwrap().as_i64().unwrap(), 1483228800000);
        let values = entry.get("values").unwrap().as_object().unwrap();
        assert_eq!(values.get("temperature").unwrap().as_i64().unwrap(), 42);
        assert_eq!(values.get("humidity").unwrap().as_i64().unwrap(), 80);
    }

    #[test]
    fn attributes_shape_matches_tb_gateway_api() {
        let mut c = AttributesChunker::new("Device A", 10 * 1024).unwrap();
        c.push("attribute1", &NGValue::String(Arc::<str>::from("value1")))
            .unwrap();
        c.push("attribute2", &NGValue::Int64(42)).unwrap();
        let bytes = c.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let obj = parsed.get("Device A").unwrap().as_object().unwrap();
        assert_eq!(obj.get("attribute1").unwrap().as_str().unwrap(), "value1");
        assert_eq!(obj.get("attribute2").unwrap().as_i64().unwrap(), 42);
    }
}
