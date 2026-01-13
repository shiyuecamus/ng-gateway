use crate::{DataType, NGValue, NGValueJsonOptions};
use bytes::Bytes;
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use serde_json::json;
use std::sync::Arc;

/// Protocol-agnostic value coercion utilities.
/// Centralizes scaling, rounding, clamping, and common string/boolean parsing.
pub struct ValueCodec;

impl ValueCodec {
    #[inline]
    pub fn apply_scale(value: f64, scale: Option<f64>) -> f64 {
        match scale {
            Some(s) => value * s,
            None => value,
        }
    }

    /// Coerce a boolean source into an expected `DataType` with optional scale.
    ///
    /// # Compatibility
    /// This returns `serde_json::Value` and is kept for non-hot-path usages.
    /// Hot-path data collection should use `coerce_bool_to_value`.
    #[inline]
    pub fn coerce_scalar_bool(
        value: bool,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<serde_json::Value> {
        Self::coerce_bool_to_value(value, expected, scale)
            .map(|v| v.to_json_value(NGValueJsonOptions::default()))
    }

    /// Coerce a numeric source (`f64`) into an expected `DataType` with optional scale.
    ///
    /// # Compatibility
    /// This returns `serde_json::Value` and is kept for non-hot-path usages.
    /// Hot-path data collection should use `coerce_f64_to_value`.
    #[inline]
    pub fn coerce_scalar_f64(
        value: f64,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<serde_json::Value> {
        Self::coerce_f64_to_value(value, expected, scale)
            .map(|v| v.to_json_value(NGValueJsonOptions::default()))
    }

    /// Apply scaling for numeric values and return an `NGValue` in the expected `DataType`.
    ///
    /// # Notes
    /// - This is the **recommended** hot-path conversion API for drivers.
    /// - Timestamp/Binary require protocol-specific parsing and are not supported here.
    #[inline]
    pub fn coerce_bool_to_value(
        value: bool,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value)),
            DataType::Int8 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= i8::MIN as f64 && v <= i8::MAX as f64 {
                    Some(NGValue::Int8(v as i8))
                } else {
                    None
                }
            }
            DataType::UInt8 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= 0.0 && v <= u8::MAX as f64 {
                    Some(NGValue::UInt8(v as u8))
                } else {
                    None
                }
            }
            DataType::Int16 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= i16::MIN as f64 && v <= i16::MAX as f64 {
                    Some(NGValue::Int16(v as i16))
                } else {
                    None
                }
            }
            DataType::UInt16 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= 0.0 && v <= u16::MAX as f64 {
                    Some(NGValue::UInt16(v as u16))
                } else {
                    None
                }
            }
            DataType::Int32 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= i32::MIN as f64 && v <= i32::MAX as f64 {
                    Some(NGValue::Int32(v as i32))
                } else {
                    None
                }
            }
            DataType::UInt32 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                if v >= 0.0 && v <= u32::MAX as f64 {
                    Some(NGValue::UInt32(v as u32))
                } else {
                    None
                }
            }
            DataType::Int64 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                Some(NGValue::Int64(v as i64))
            }
            DataType::UInt64 => {
                let v = Self::apply_scale(if value { 1.0 } else { 0.0 }, scale).round();
                let v = if v < 0.0 { 0.0 } else { v };
                Some(NGValue::UInt64(v as u64))
            }
            DataType::Float32 => Some(NGValue::Float32(Self::apply_scale(
                if value { 1.0 } else { 0.0 },
                scale,
            ) as f32)),
            DataType::Float64 => Some(NGValue::Float64(Self::apply_scale(
                if value { 1.0 } else { 0.0 },
                scale,
            ))),
            DataType::String => Some(NGValue::String(Arc::<str>::from(if value {
                "true"
            } else {
                "false"
            }))),
            DataType::Binary => Some(NGValue::Binary(Bytes::from_static(if value {
                &[1u8; 1]
            } else {
                &[0u8; 1]
            }))),
            DataType::Timestamp => None,
        }
    }

    /// Coerce a numeric value (`f64`) into an expected `DataType` with optional scale.
    ///
    /// # Performance
    /// This avoids `serde_json::Value` allocations and should be used in hot paths.
    #[inline]
    pub fn coerce_f64_to_value(
        value: f64,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value != 0.0)),
            DataType::Int8 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= i8::MIN as f64 && v <= i8::MAX as f64 {
                    Some(NGValue::Int8(v as i8))
                } else {
                    None
                }
            }
            DataType::UInt8 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= 0.0 && v <= u8::MAX as f64 {
                    Some(NGValue::UInt8(v as u8))
                } else {
                    None
                }
            }
            DataType::Int16 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= i16::MIN as f64 && v <= i16::MAX as f64 {
                    Some(NGValue::Int16(v as i16))
                } else {
                    None
                }
            }
            DataType::UInt16 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= 0.0 && v <= u16::MAX as f64 {
                    Some(NGValue::UInt16(v as u16))
                } else {
                    None
                }
            }
            DataType::Int32 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= i32::MIN as f64 && v <= i32::MAX as f64 {
                    Some(NGValue::Int32(v as i32))
                } else {
                    None
                }
            }
            DataType::UInt32 => {
                let v = Self::apply_scale(value, scale).round();
                if v >= 0.0 && v <= u32::MAX as f64 {
                    Some(NGValue::UInt32(v as u32))
                } else {
                    None
                }
            }
            DataType::Int64 => {
                let v = Self::apply_scale(value, scale).round();
                Some(NGValue::Int64(v as i64))
            }
            DataType::UInt64 => {
                let v = Self::apply_scale(value, scale).round();
                let v = if v < 0.0 { 0.0 } else { v };
                Some(NGValue::UInt64(v as u64))
            }
            DataType::Float32 => Some(NGValue::Float32(Self::apply_scale(value, scale) as f32)),
            DataType::Float64 => Some(NGValue::Float64(Self::apply_scale(value, scale))),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &Self::apply_scale(value, scale).to_be_bytes(),
            ))),
            DataType::Timestamp => {
                // Timestamp is represented as Unix epoch milliseconds in `i64`.
                // Avoid float-to-int wrap on extreme values.
                let v = Self::apply_scale(value, scale).round();
                if !v.is_finite() {
                    return None;
                }
                if v < i64::MIN as f64 || v > i64::MAX as f64 {
                    return None;
                }
                Some(NGValue::Timestamp(v as i64))
            }
        }
    }

    /// Coerce an unsigned integer source into an expected `DataType` with optional scale.
    ///
    /// # Performance & correctness
    /// - When `scale` is `None` and the target is an integer type, this avoids any `f64`
    ///   roundtrip and therefore preserves full integer precision.
    /// - When `scale` is `Some(_)`, we apply scaling in `f64` and then delegate to
    ///   `coerce_f64_to_value` for consistent rounding behavior.
    #[inline]
    pub fn coerce_u64_to_value(
        value: u64,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        if scale.is_some() {
            return Self::coerce_f64_to_value(value as f64, expected, scale);
        }
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value != 0)),
            DataType::UInt8 => u8::try_from(value).ok().map(NGValue::UInt8),
            DataType::UInt16 => u16::try_from(value).ok().map(NGValue::UInt16),
            DataType::UInt32 => u32::try_from(value).ok().map(NGValue::UInt32),
            DataType::UInt64 => Some(NGValue::UInt64(value)),
            DataType::Int8 => i8::try_from(value).ok().map(NGValue::Int8),
            DataType::Int16 => i16::try_from(value).ok().map(NGValue::Int16),
            DataType::Int32 => i32::try_from(value).ok().map(NGValue::Int32),
            DataType::Int64 => i64::try_from(value).ok().map(NGValue::Int64),
            DataType::Float32 => Some(NGValue::Float32(value as f32)),
            DataType::Float64 => Some(NGValue::Float64(value as f64)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &value.to_be_bytes(),
            ))),
            DataType::Timestamp => {
                // Avoid u64 -> i64 wrap.
                if value > i64::MAX as u64 {
                    None
                } else {
                    Some(NGValue::Timestamp(value as i64))
                }
            }
        }
    }

    /// Coerce a signed integer source into an expected `DataType` with optional scale.
    ///
    /// See `coerce_u64_to_value` for performance semantics.
    #[inline]
    pub fn coerce_i64_to_value(
        value: i64,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        if scale.is_some() {
            return Self::coerce_f64_to_value(value as f64, expected, scale);
        }
        match expected {
            DataType::Boolean => Some(NGValue::Boolean(value != 0)),
            DataType::Int8 => i8::try_from(value).ok().map(NGValue::Int8),
            DataType::Int16 => i16::try_from(value).ok().map(NGValue::Int16),
            DataType::Int32 => i32::try_from(value).ok().map(NGValue::Int32),
            DataType::Int64 => Some(NGValue::Int64(value)),
            DataType::UInt8 => u8::try_from(value).ok().map(NGValue::UInt8),
            DataType::UInt16 => u16::try_from(value).ok().map(NGValue::UInt16),
            DataType::UInt32 => u32::try_from(value).ok().map(NGValue::UInt32),
            DataType::UInt64 => u64::try_from(value).ok().map(NGValue::UInt64),
            DataType::Float32 => Some(NGValue::Float32(value as f32)),
            DataType::Float64 => Some(NGValue::Float64(value as f64)),
            DataType::String => Some(NGValue::String(Arc::<str>::from(value.to_string()))),
            DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(
                &value.to_be_bytes(),
            ))),
            DataType::Timestamp => Some(NGValue::Timestamp(value)),
        }
    }

    #[inline]
    pub fn json_to_bool(v: &serde_json::Value) -> Option<bool> {
        if let Some(b) = v.as_bool() {
            return Some(b);
        }
        if let Some(n) = v.as_i64() {
            return Some(n != 0);
        }
        if let Some(n) = v.as_u64() {
            return Some(n != 0);
        }
        if let Some(s) = v.as_str() {
            let sl = s.trim().to_ascii_lowercase();
            return match sl.as_str() {
                "true" | "1" | "on" | "yes" | "y" | "t" => Some(true),
                "false" | "0" | "off" | "no" | "n" | "f" => Some(false),
                _ => None,
            };
        }
        None
    }

    #[inline]
    pub fn json_to_f64(v: &serde_json::Value) -> Option<f64> {
        // Fast paths for numeric JSON
        if let Some(n) = v.as_f64() {
            return Some(n);
        }
        if let Some(n) = v.as_i64() {
            return Some(n as f64);
        }
        if let Some(n) = v.as_u64() {
            return Some(n as f64);
        }
        // String parsing: support decimal and 0x-prefixed hex
        if let Some(s) = v.as_str() {
            let st = s.trim();
            if st.starts_with("0x") || st.starts_with("0X") {
                let hex = &st[2..];
                if hex.is_empty() {
                    return None;
                }
                let mut acc: u128 = 0;
                for ch in hex.as_bytes() {
                    let val = (*ch as char).to_digit(16)? as u128;
                    acc = acc.saturating_mul(16).saturating_add(val);
                }
                return Some(acc as f64);
            }
            return s.parse::<f64>().ok();
        }
        None
    }

    /// Coerce a JSON value into the expected DataType with optional scale.
    /// Returns None if conversion is not possible.
    #[inline]
    pub fn coerce_json_to_data_type(
        v: &serde_json::Value,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<serde_json::Value> {
        match expected {
            DataType::String => {
                if let Some(s) = v.as_str() {
                    Some(json!(s))
                } else {
                    Some(json!(v.to_string()))
                }
            }
            DataType::Binary | DataType::Timestamp => None,
            DataType::Boolean => Self::json_to_bool(v).map(|b| json!(b)),
            _ => {
                if let Some(b) = v.as_bool() {
                    Self::coerce_scalar_bool(b, expected, scale)
                } else if let Some(n) = Self::json_to_f64(v) {
                    Self::coerce_scalar_f64(n, expected, scale)
                } else {
                    None
                }
            }
        }
    }

    /// Time helpers: centralize rendering to string or epoch-ms conversions for drivers.
    #[inline]
    pub fn time_of_day_to_ms(t: NaiveTime) -> u64 {
        (t.num_seconds_from_midnight() as u64) * 1000 + (t.nanosecond() / 1_000_000) as u64
    }

    #[inline]
    pub fn duration_to_ms(d: Duration) -> i64 {
        d.num_milliseconds()
    }

    #[inline]
    pub fn date_to_epoch_ms(d: NaiveDate) -> Option<i64> {
        let ndt = d.and_time(NaiveTime::from_hms_opt(0, 0, 0)?);
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc);
        Some(dt.timestamp_millis())
    }

    #[inline]
    pub fn datetime_to_epoch_ms(ndt: NaiveDateTime) -> i64 {
        let dt = DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc);
        dt.timestamp_millis()
    }

    /// Convert a byte slice into a lower-case hex string with "0x" prefix.
    #[inline]
    pub fn bytes_to_hex_string(bytes: &[u8]) -> String {
        const LUT: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(2 + bytes.len() * 2);
        out.push_str("0x");
        for &b in bytes {
            out.push(LUT[(b >> 4) as usize] as char);
            out.push(LUT[(b & 0x0F) as usize] as char);
        }
        out
    }

    /// Decode a hex string into bytes. Accepts optional "0x"/"0X" prefix and odd length (pads low nibble with 0).
    #[inline]
    pub fn hex_string_to_bytes(s: &str) -> Option<Vec<u8>> {
        let st = s.trim();
        let hex = if st.starts_with("0x") || st.starts_with("0X") {
            &st[2..]
        } else {
            st
        };
        if hex.is_empty() {
            return Some(Vec::new());
        }
        let bytes = hex.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len().div_ceil(2));
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16)? as u8;
            let lo = (bytes[i + 1] as char).to_digit(16)? as u8;
            out.push((hi << 4) | (lo & 0x0F));
            i += 2;
        }
        if i < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16)? as u8;
            out.push(hi << 4);
        }
        Some(out)
    }

    /// Parse a JSON value as epoch milliseconds (UTC).
    /// - String: RFC3339 parsed in any offset, converted to UTC ms
    /// - Number: treated as epoch milliseconds
    #[inline]
    pub fn json_to_timestamp_ms(v: &serde_json::Value) -> Option<i64> {
        if let Some(s) = v.as_str() {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s.trim()) {
                return Some(dt.timestamp_millis());
            }
        }
        if let Some(n) = v.as_i64() {
            return Some(n);
        }
        if let Some(n) = v.as_u64() {
            return Some(n as i64);
        }
        if let Some(n) = v.as_f64() {
            return Some(n.round() as i64);
        }
        None
    }
}
