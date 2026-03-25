//! ThingsBoard Gateway API payload builder (hot path).
//!
//! This module centralizes the JSON framing bytes for TB Gateway API messages.
//! The hot path in `handle.rs` should not be cluttered with ad-hoc "magic" byte
//! literals; instead it calls the helpers here.
//!
//! # Performance
//! - The device prefix is built once per publish batch and is reused for each chunk.
//! - We intentionally write framing bytes directly to `Vec<u8>` for minimal overhead.
//! - All escaping is delegated to `serde_json` to ensure correctness.

use ng_gateway_sdk::{NGValue, NorthwardError, NorthwardResult};
use std::{io::Write as _, mem};
use tracing::warn;

// === Telemetry framing constants ===
//
// Telemetry payload shape (per TB Gateway API):
// `{ "<device>": [ {"ts":<ms>,"values":{ k:v, ... }}, {"ts":<ms2>,"values":{ ... }} ] }`

/// Fixed bytes: opening `:[` after the device key string.
const TEL_ARRAY_OPEN: &[u8] = b":[";

/// Opening of a single ts-group entry: `{"ts":`.
const TEL_ENTRY_OPEN: &[u8] = br#"{"ts":"#;

/// Bytes between the ts value and the values object: `,"values":{`.
const TEL_VALUES_OPEN: &[u8] = br#","values":{"#;

/// Closing of a single ts-group entry: `}}`.
const TEL_ENTRY_CLOSE: &[u8] = b"}}";

/// Suffix closing the array and root object: `]}`.
const TEL_SUFFIX: &[u8] = b"]}";

// === Attributes framing constants ===

/// Fixed bytes after the device key for attributes: `:{`.
const ATTRIBUTES_AFTER_DEVICE: &[u8] = br#":{"#;

/// Attributes payload suffix: `}}`.
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
/// Uses `NGValue`'s `Serialize` implementation (default semantics).
#[inline]
pub fn write_ng_value(buf: &mut Vec<u8>, v: &NGValue) -> NorthwardResult<()> {
    serde_json::to_writer(buf, v).map_err(|e| NorthwardError::SerializationError {
        reason: e.to_string(),
    })
}

/// Build the device-level prefix bytes into `buf`.
///
/// Writes: `{"<device>":[`
fn build_device_prefix(buf: &mut Vec<u8>, device_name: &str) -> NorthwardResult<()> {
    buf.clear();
    buf.push(b'{');
    write_json_str(buf, device_name)?;
    buf.extend_from_slice(TEL_ARRAY_OPEN);
    Ok(())
}

/// Build attributes prefix bytes into `buf`.
///
/// Writes: `{"<device>":{`
pub fn build_attributes_prefix(buf: &mut Vec<u8>, device_name: &str) -> NorthwardResult<()> {
    buf.clear();
    buf.push(b'{');
    write_json_str(buf, device_name)?;
    buf.extend_from_slice(ATTRIBUTES_AFTER_DEVICE);
    Ok(())
}

/// Incremental chunker for ThingsBoard gateway telemetry payloads with per-point
/// source timestamp support.
///
/// # TB Gateway API format
/// ```json
/// { "<device>": [
///   {"ts": 1483228800000, "values": {"temp": 22.5, "humidity": 80}},
///   {"ts": 1483228801000, "values": {"pressure": 101.3}}
/// ] }
/// ```
///
/// Points with the same effective `ts` are grouped into the same `{"ts":..., "values":{...}}`
/// entry. When the ts changes (or a chunk needs to be flushed due to size), the current
/// entry is closed and a new one opened.
///
/// # Guarantees
/// - Every returned chunk is `<= max_payload_bytes`.
/// - Per-point order is preserved within each ts group.
pub struct TelemetryChunker {
    max_payload_bytes: usize,
    /// Device-level prefix: `{"<device>":[`
    device_prefix: Vec<u8>,
    /// Current payload being built.
    payload: Vec<u8>,
    /// Double-buffer for swap-based flushing.
    out: Vec<u8>,
    /// Scratch buffers for key/value serialization.
    scratch_key: Vec<u8>,
    scratch_val: Vec<u8>,
    /// Whether any key-value pair has been written in the **current ts-group entry**.
    entry_has_values: bool,
    /// Whether any ts-group entry has been written in the **current chunk**.
    chunk_has_entries: bool,
    /// The `ts_ms` of the currently open ts-group entry, or `None` if no entry is open.
    current_ts: Option<i64>,
}

impl TelemetryChunker {
    /// Create a new telemetry chunker.
    ///
    /// # Arguments
    /// - `device_name`: sub-device name in TB gateway API.
    /// - `max_payload_bytes`: hard cap for the serialized JSON bytes.
    pub fn new(device_name: &str, max_payload_bytes: usize) -> NorthwardResult<Self> {
        let max_payload_bytes = max_payload_bytes.max(256);
        let mut device_prefix = Vec::with_capacity(128);
        build_device_prefix(&mut device_prefix, device_name)?;

        let overhead = device_prefix.len()
            + TEL_ENTRY_OPEN.len()
            + 20 // max digits for i64 ts
            + TEL_VALUES_OPEN.len()
            + TEL_ENTRY_CLOSE.len()
            + TEL_SUFFIX.len();
        if overhead > max_payload_bytes {
            return Err(NorthwardError::ConfigurationError {
                message: format!(
                    "communication.max_payload_bytes too small: minimum overhead ({overhead}) > {max_payload_bytes}",
                ),
            });
        }

        let mut payload = Vec::with_capacity(max_payload_bytes.min(64 * 1024));
        payload.extend_from_slice(&device_prefix);
        let out = Vec::with_capacity(payload.capacity());

        Ok(Self {
            max_payload_bytes,
            device_prefix,
            payload,
            out,
            scratch_key: Vec::with_capacity(64),
            scratch_val: Vec::with_capacity(64),
            entry_has_values: false,
            chunk_has_entries: false,
            current_ts: None,
        })
    }

    /// Push one key/value pair with a per-point timestamp.
    ///
    /// `ts_ms` is the effective timestamp for this point. Points with the same `ts_ms`
    /// are grouped into the same `{"ts":..., "values":{...}}` entry.
    ///
    /// Returns `Some(chunk_bytes)` when the current chunk was flushed due to size limit.
    pub fn push(
        &mut self,
        key: &str,
        value: &NGValue,
        ts_ms: i64,
    ) -> NorthwardResult<Option<Vec<u8>>> {
        // If ts changed from the current open entry, close the current entry first.
        if self.current_ts != Some(ts_ms) && self.current_ts.is_some() {
            self.close_entry();
        }

        // Pre-serialize key and value into scratch buffers.
        self.scratch_key.clear();
        self.scratch_val.clear();
        write_json_str(&mut self.scratch_key, key)?;
        write_ng_value(&mut self.scratch_val, value)?;

        // Calculate the space needed for this push.
        let needs_new_entry = self.current_ts != Some(ts_ms);
        let new_entry_overhead = if needs_new_entry {
            let comma = if self.chunk_has_entries { 1 } else { 0 };
            comma + TEL_ENTRY_OPEN.len() + 20 + TEL_VALUES_OPEN.len()
        } else {
            0
        };
        let comma_kv = if !needs_new_entry && self.entry_has_values {
            1
        } else {
            0
        };
        let kv_len = comma_kv + self.scratch_key.len() + 1 + self.scratch_val.len();
        let close_overhead = TEL_ENTRY_CLOSE.len() + TEL_SUFFIX.len();
        let total_needed = new_entry_overhead + kv_len + close_overhead;

        // If it doesn't fit, flush current chunk and try again.
        if self.payload.len() + total_needed > self.max_payload_bytes {
            let flushed = self.flush_chunk();

            // Recalculate after flush (entry is always new after flush).
            let fresh_entry_overhead = TEL_ENTRY_OPEN.len() + 20 + TEL_VALUES_OPEN.len();
            let fresh_total = fresh_entry_overhead
                + self.scratch_key.len()
                + 1
                + self.scratch_val.len()
                + TEL_ENTRY_CLOSE.len()
                + TEL_SUFFIX.len();

            if self.payload.len() + fresh_total > self.max_payload_bytes {
                warn!(
                    key,
                    entry_bytes = fresh_total,
                    max_payload_bytes = self.max_payload_bytes,
                    "ThingsBoard telemetry entry too large; dropping the point"
                );
                return Ok(flushed);
            }

            self.open_entry(ts_ms);
            self.append_kv();
            return Ok(flushed);
        }

        // Normal path: open new entry if needed, then append kv.
        if needs_new_entry {
            self.open_entry(ts_ms);
        }
        self.append_kv();
        Ok(None)
    }

    /// Finish the current chunk (if any ts-group entries exist).
    pub fn finish(mut self) -> Option<Vec<u8>> {
        if !self.chunk_has_entries && !self.entry_has_values {
            return None;
        }
        self.close_entry();
        self.payload.extend_from_slice(TEL_SUFFIX);
        Some(self.payload)
    }

    /// Open a new `{"ts":<ts_ms>,"values":{` entry.
    fn open_entry(&mut self, ts_ms: i64) {
        if self.chunk_has_entries {
            self.payload.push(b',');
        }
        self.payload.extend_from_slice(TEL_ENTRY_OPEN);
        write!(self.payload, "{ts_ms}").ok();
        self.payload.extend_from_slice(TEL_VALUES_OPEN);
        self.current_ts = Some(ts_ms);
        self.entry_has_values = false;
        self.chunk_has_entries = true;
    }

    /// Close the current `}}` entry.
    fn close_entry(&mut self) {
        if self.current_ts.is_some() {
            self.payload.extend_from_slice(TEL_ENTRY_CLOSE);
            self.current_ts = None;
            self.entry_has_values = false;
        }
    }

    /// Append the pre-serialized key:value from scratch buffers.
    fn append_kv(&mut self) {
        if self.entry_has_values {
            self.payload.push(b',');
        }
        self.payload.extend_from_slice(&self.scratch_key);
        self.payload.push(b':');
        self.payload.extend_from_slice(&self.scratch_val);
        self.entry_has_values = true;
    }

    /// Flush the current chunk and reset for a new one.
    ///
    /// Returns `Some(bytes)` if there was content to flush, `None` otherwise.
    fn flush_chunk(&mut self) -> Option<Vec<u8>> {
        if !self.chunk_has_entries && !self.entry_has_values {
            return None;
        }
        self.close_entry();
        self.payload.extend_from_slice(TEL_SUFFIX);

        mem::swap(&mut self.payload, &mut self.out);
        let out = mem::take(&mut self.out);

        self.payload.clear();
        self.payload.extend_from_slice(&self.device_prefix);
        self.chunk_has_entries = false;
        self.entry_has_values = false;
        self.current_ts = None;

        Some(out)
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
                mem::swap(&mut self.payload, &mut self.out);
                let out = mem::take(&mut self.out);
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
    fn telemetry_single_ts_group() {
        let mut c = TelemetryChunker::new("Device A", 10 * 1024).unwrap();
        c.push("temperature", &NGValue::Int64(42), 1483228800000)
            .unwrap();
        c.push("humidity", &NGValue::Int64(80), 1483228800000)
            .unwrap();
        let bytes = c.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let arr = parsed.get("Device A").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let entry = arr[0].as_object().unwrap();
        assert_eq!(entry.get("ts").unwrap().as_i64().unwrap(), 1483228800000);
        let values = entry.get("values").unwrap().as_object().unwrap();
        assert_eq!(values.get("temperature").unwrap().as_i64().unwrap(), 42);
        assert_eq!(values.get("humidity").unwrap().as_i64().unwrap(), 80);
    }

    #[test]
    fn telemetry_multiple_ts_groups() {
        let mut c = TelemetryChunker::new("Device A", 10 * 1024).unwrap();
        c.push("temp", &NGValue::Float64(22.5), 1000).unwrap();
        c.push("pressure", &NGValue::Float64(101.3), 2000).unwrap();
        c.push("humidity", &NGValue::Int64(80), 2000).unwrap();
        let bytes = c.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let arr = parsed.get("Device A").unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2);

        assert_eq!(arr[0]["ts"].as_i64().unwrap(), 1000);
        assert!(arr[0]["values"]["temp"].as_f64().is_some());

        assert_eq!(arr[1]["ts"].as_i64().unwrap(), 2000);
        assert!(arr[1]["values"]["pressure"].as_f64().is_some());
        assert_eq!(arr[1]["values"]["humidity"].as_i64().unwrap(), 80);
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
