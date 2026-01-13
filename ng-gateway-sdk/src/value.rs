use crate::DataType;
use base64::Engine as _;
use bytes::Bytes;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::{borrow::Cow, sync::Arc};

/// Error returned when converting an `NGValue` into a concrete Rust primitive.
///
/// This is designed for protocol codecs and driver control-plane logic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NGValueCastError {
    /// Value is not a number (int/float).
    #[error("expected numeric value, got {actual:?}")]
    NotNumeric { actual: DataType },
    /// Numeric value is NaN/Inf and cannot be represented in target type.
    #[error("numeric value is not finite")]
    NotFinite,
    /// Numeric value is out of the representable range of the target type.
    #[error("numeric value out of range for {target}")]
    OutOfRange { target: &'static str },
    /// Strict type mismatch for non-numeric conversions.
    #[error("type mismatch: expected {expected:?}, got {actual:?}")]
    TypeMismatch {
        expected: DataType,
        actual: DataType,
    },
    /// String/Binary value cannot be parsed into the target numeric type.
    #[error("failed to parse {target} from string: {value}")]
    ParseError { target: &'static str, value: String },
    /// Binary value cannot be interpreted as UTF-8 for parsing/formatting.
    #[error("binary value is not valid UTF-8 for {target}")]
    InvalidUtf8 { target: &'static str },
    /// Binary value length does not match the expected fixed-width for this cast.
    #[error("binary length mismatch for {target}: expected {expected} bytes, got {len}")]
    InvalidBinaryLength {
        target: &'static str,
        expected: &'static str,
        len: usize,
    },
}

/// JSON encoding strategy for binary values.
///
/// This only affects `NGValue::Binary`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BinaryJsonEncoding {
    /// Encode binary as Base64 string (recommended for northbound protocols).
    #[default]
    Base64,
    /// Encode binary as hex string.
    Hex,
}

/// JSON encoding strategy for timestamp values.
///
/// This only affects `NGValue::Timestamp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TimestampJsonEncoding {
    /// Encode timestamp as Unix milliseconds (number).
    #[default]
    UnixMs,
    /// Encode timestamp as RFC3339 string (UTC).
    ///
    /// NOTE: This requires conversion to `chrono::DateTime<Utc>` at encoding time.
    Rfc3339Utc,
}

/// JSON encoding options for `NGValue`.
///
/// This is intentionally lightweight and copyable so callers can keep it on stack.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NGValueJsonOptions {
    /// Binary encoding strategy.
    pub binary: BinaryJsonEncoding,
    /// Timestamp encoding strategy.
    pub timestamp: TimestampJsonEncoding,
}

/// A strongly-typed runtime value for telemetry/attributes.
///
/// # Performance goals
/// - No `serde_json::Value` on hot paths
/// - Shared string storage (`Arc<str>`) to reduce cloning cost
/// - Zero-copy binary payloads (`Bytes`)
///
/// # Timestamp semantics
/// `NGValue::Timestamp(i64)` represents **Unix time in milliseconds** by default.
#[derive(Clone, Debug, PartialEq)]
pub enum NGValue {
    Boolean(bool),
    Int8(i8),
    UInt8(u8),
    Int16(i16),
    UInt16(u16),
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    Float32(f32),
    Float64(f64),
    String(Arc<str>),
    Binary(Bytes),
    Timestamp(i64),
}

impl NGValue {
    /// Return the corresponding SDK `DataType` for this value.
    #[inline]
    pub fn data_type(&self) -> DataType {
        match self {
            NGValue::Boolean(_) => DataType::Boolean,
            NGValue::Int8(_) => DataType::Int8,
            NGValue::UInt8(_) => DataType::UInt8,
            NGValue::Int16(_) => DataType::Int16,
            NGValue::UInt16(_) => DataType::UInt16,
            NGValue::Int32(_) => DataType::Int32,
            NGValue::UInt32(_) => DataType::UInt32,
            NGValue::Int64(_) => DataType::Int64,
            NGValue::UInt64(_) => DataType::UInt64,
            NGValue::Float32(_) => DataType::Float32,
            NGValue::Float64(_) => DataType::Float64,
            NGValue::String(_) => DataType::String,
            NGValue::Binary(_) => DataType::Binary,
            NGValue::Timestamp(_) => DataType::Timestamp,
        }
    }

    /// Validate whether this value matches the expected `DataType`.
    ///
    /// This is a strict check (no implicit casting).
    #[inline]
    pub fn validate_datatype(&self, expected: DataType) -> bool {
        self.data_type() == expected
    }

    /// Convert this typed value into a `serde_json::Value` for northbound protocols.
    ///
    /// This conversion is intended for encoding stage (northbound plugins) and
    /// should not be used on the hot path inside collectors/snapshots.
    pub fn to_json_value(&self, opts: NGValueJsonOptions) -> serde_json::Value {
        match self {
            NGValue::Boolean(v) => serde_json::Value::Bool(*v),
            NGValue::Int8(v) => serde_json::Value::Number((*v as i64).into()),
            NGValue::UInt8(v) => serde_json::Value::Number((*v as u64).into()),
            NGValue::Int16(v) => serde_json::Value::Number((*v as i64).into()),
            NGValue::UInt16(v) => serde_json::Value::Number((*v as u64).into()),
            NGValue::Int32(v) => serde_json::Value::Number((*v as i64).into()),
            NGValue::UInt32(v) => serde_json::Value::Number((*v as u64).into()),
            NGValue::Int64(v) => serde_json::Value::Number((*v).into()),
            NGValue::UInt64(v) => serde_json::Value::Number((*v).into()),
            NGValue::Float32(v) => {
                serde_json::Number::from_f64(*v as f64).map_or(serde_json::Value::Null, Into::into)
            }
            NGValue::Float64(v) => {
                serde_json::Number::from_f64(*v).map_or(serde_json::Value::Null, Into::into)
            }
            NGValue::String(v) => serde_json::Value::String(v.to_string()),
            NGValue::Binary(v) => {
                let s = match opts.binary {
                    BinaryJsonEncoding::Base64 => {
                        base64::engine::general_purpose::STANDARD.encode(v.as_ref())
                    }
                    BinaryJsonEncoding::Hex => hex::encode(v.as_ref()),
                };
                serde_json::Value::String(s)
            }
            NGValue::Timestamp(v) => match opts.timestamp {
                TimestampJsonEncoding::UnixMs => serde_json::Value::Number((*v).into()),
                TimestampJsonEncoding::Rfc3339Utc => {
                    // Avoid panicking on out-of-range timestamp by falling back to number.
                    match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(*v) {
                        Some(dt) => serde_json::Value::String(dt.to_rfc3339()),
                        None => serde_json::Value::Number((*v).into()),
                    }
                }
            },
        }
    }

    /// Try to convert a JSON scalar into a strongly-typed `NGValue` with an expected `DataType`.
    ///
    /// # Purpose
    /// This helper exists as a **transitional compatibility layer** while migrating
    /// southbound drivers away from `serde_json::Value` hot paths.
    ///
    /// # Rules
    /// - Strict: no implicit allocations for non-string inputs (e.g. a number will NOT be
    ///   converted into `NGValue::String`)
    /// - Strict: arrays/objects are rejected (return `None`)
    /// - Numeric conversions perform range checks for the target integer width
    ///
    /// # Performance
    /// - Intended to be used on the encoding boundary only (not inside core snapshot/update hot loops)
    /// - For true zero-allocation hot paths, drivers should produce `NGValue` directly.
    #[inline]
    pub fn try_from_json_scalar(expected: DataType, v: &serde_json::Value) -> Option<Self> {
        match expected {
            DataType::Boolean => {
                if let Some(b) = v.as_bool() {
                    return Some(NGValue::Boolean(b));
                }
                if let Some(i) = v.as_i64() {
                    return Some(NGValue::Boolean(i != 0));
                }
                if let Some(u) = v.as_u64() {
                    return Some(NGValue::Boolean(u != 0));
                }
                None
            }
            DataType::Int8 => v
                .as_i64()
                .and_then(|n| i8::try_from(n).ok())
                .map(NGValue::Int8),
            DataType::UInt8 => v
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .map(NGValue::UInt8),
            DataType::Int16 => v
                .as_i64()
                .and_then(|n| i16::try_from(n).ok())
                .map(NGValue::Int16),
            DataType::UInt16 => v
                .as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .map(NGValue::UInt16),
            DataType::Int32 => v
                .as_i64()
                .and_then(|n| i32::try_from(n).ok())
                .map(NGValue::Int32),
            DataType::UInt32 => v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .map(NGValue::UInt32),
            DataType::Int64 => v.as_i64().map(NGValue::Int64),
            DataType::UInt64 => v.as_u64().map(NGValue::UInt64),
            DataType::Float32 => v.as_f64().map(|n| NGValue::Float32(n as f32)),
            DataType::Float64 => v.as_f64().map(NGValue::Float64),
            DataType::String => v.as_str().map(|s| NGValue::String(Arc::<str>::from(s))),
            DataType::Binary => {
                let s = v.as_str()?;
                let st = s.trim();
                // Prefer hex with 0x prefix (common in industrial drivers).
                let bytes = if st.starts_with("0x") || st.starts_with("0X") {
                    let hex = &st[2..];
                    if hex.is_empty() {
                        Vec::new()
                    } else {
                        hex::decode(hex).ok()?
                    }
                } else {
                    // Fallback: base64
                    base64::engine::general_purpose::STANDARD.decode(st).ok()?
                };
                Some(NGValue::Binary(Bytes::from(bytes)))
            }
            DataType::Timestamp => {
                if let Some(i) = v.as_i64() {
                    return Some(NGValue::Timestamp(i));
                }
                if let Some(u) = v.as_u64() {
                    return Some(NGValue::Timestamp(u as i64));
                }
                if let Some(s) = v.as_str() {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s.trim()) {
                        return Some(NGValue::Timestamp(dt.timestamp_millis()));
                    }
                }
                None
            }
        }
    }
}

// === Best-practice cast APIs for protocol codecs ===
//
// Policy:
// - Integer targets accept integer/float inputs; floats are rounded (`round()`).
// - Boolean is strict: only `NGValue::Boolean` is accepted (no implicit numeric mapping).
#[inline]
fn parse_bool_from_str(s: &str) -> Option<bool> {
    let st = s.trim().to_ascii_lowercase();
    match st.as_str() {
        "true" | "1" | "on" | "yes" | "y" | "t" => Some(true),
        "false" | "0" | "off" | "no" | "n" | "f" => Some(false),
        _ => None,
    }
}

#[inline]
fn binary_len_err(target: &'static str, expected: &'static str, len: usize) -> NGValueCastError {
    NGValueCastError::InvalidBinaryLength {
        target,
        expected,
        len,
    }
}

#[inline]
fn binary_to_u8(b: &Bytes, target: &'static str) -> Result<u8, NGValueCastError> {
    if !b.is_empty() {
        Ok(b[0])
    } else {
        Err(binary_len_err(target, "1", b.len()))
    }
}

#[inline]
fn binary_to_i8(b: &Bytes, target: &'static str) -> Result<i8, NGValueCastError> {
    if !b.is_empty() {
        Ok(i8::from_be_bytes([b[0]]))
    } else {
        Err(binary_len_err(target, "1", b.len()))
    }
}

#[inline]
fn binary_to_i16_be(b: &Bytes, target: &'static str) -> Result<i16, NGValueCastError> {
    if b.len() >= 2 {
        Ok(i16::from_be_bytes([b[0], b[1]]))
    } else {
        Err(binary_len_err(target, "2", b.len()))
    }
}

#[inline]
fn binary_to_i32_be(b: &Bytes, target: &'static str) -> Result<i32, NGValueCastError> {
    if b.len() >= 4 {
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    } else {
        Err(binary_len_err(target, "4", b.len()))
    }
}

#[inline]
fn binary_to_i64_be(b: &Bytes, target: &'static str) -> Result<i64, NGValueCastError> {
    if b.len() >= 8 {
        Ok(i64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    } else {
        Err(binary_len_err(target, "8", b.len()))
    }
}

#[inline]
fn binary_to_u16_be(b: &Bytes, target: &'static str) -> Result<u16, NGValueCastError> {
    if b.len() >= 2 {
        Ok(u16::from_be_bytes([b[0], b[1]]))
    } else {
        Err(binary_len_err(target, "2", b.len()))
    }
}

#[inline]
fn binary_to_u32_be(b: &Bytes, target: &'static str) -> Result<u32, NGValueCastError> {
    if b.len() >= 4 {
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    } else {
        Err(binary_len_err(target, "4", b.len()))
    }
}

#[inline]
fn binary_to_u64_be(b: &Bytes, target: &'static str) -> Result<u64, NGValueCastError> {
    if b.len() >= 8 {
        Ok(u64::from_be_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    } else {
        Err(binary_len_err(target, "8", b.len()))
    }
}

#[inline]
fn parse_f64_from_str(target: &'static str, s: &str) -> Result<f64, NGValueCastError> {
    let st = s.trim();
    if st.is_empty() {
        return Err(NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        });
    }

    // Support common "0x..." hex integers (e.g. configs / PLC parameters).
    let parsed = if let Some(hex) = st.strip_prefix("0x").or_else(|| st.strip_prefix("0X")) {
        let n = u64::from_str_radix(hex.trim(), 16).map_err(|_| NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        })?;
        n as f64
    } else {
        st.parse::<f64>()
            .map_err(|_| NGValueCastError::ParseError {
                target,
                value: st.to_string(),
            })?
    };

    if !parsed.is_finite() {
        return Err(NGValueCastError::NotFinite);
    }
    Ok(parsed)
}

#[inline]
fn parse_i64_from_str(target: &'static str, s: &str) -> Result<i64, NGValueCastError> {
    let st = s.trim();
    if st.is_empty() {
        return Err(NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        });
    }

    // Fast path: decimal integer.
    if let Ok(n) = st.parse::<i64>() {
        return Ok(n);
    }

    // Hex integer, allow +/-0x...
    let (sign, rest) = match st.as_bytes().first().copied() {
        Some(b'+') => (1i128, &st[1..]),
        Some(b'-') => (-1i128, &st[1..]),
        _ => (1i128, st),
    };
    if let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        let u = u64::from_str_radix(hex.trim(), 16).map_err(|_| NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        })? as i128;
        let signed = sign * u;
        if signed < i64::MIN as i128 || signed > i64::MAX as i128 {
            return Err(NGValueCastError::OutOfRange { target });
        }
        return Ok(signed as i64);
    }

    // Fallback: parse float then apply the same rounding policy as float->int casts.
    let f = parse_f64_from_str(target, st)?;
    let r = f.round();
    if r < i64::MIN as f64 || r > i64::MAX as f64 {
        return Err(NGValueCastError::OutOfRange { target });
    }
    Ok(r as i64)
}

#[inline]
fn parse_u64_from_str(target: &'static str, s: &str) -> Result<u64, NGValueCastError> {
    let st = s.trim();
    if st.is_empty() {
        return Err(NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        });
    }

    // Fast path: decimal integer.
    if let Ok(n) = st.parse::<u64>() {
        return Ok(n);
    }

    // Hex integer (no sign for u64).
    if let Some(hex) = st.strip_prefix("0x").or_else(|| st.strip_prefix("0X")) {
        return u64::from_str_radix(hex.trim(), 16).map_err(|_| NGValueCastError::ParseError {
            target,
            value: st.to_string(),
        });
    }

    // Fallback: parse float then apply rounding.
    let f = parse_f64_from_str(target, st)?;
    let r = f.round();
    if r < 0.0 || r > u64::MAX as f64 {
        return Err(NGValueCastError::OutOfRange { target });
    }
    Ok(r as u64)
}

impl TryFrom<&NGValue> for bool {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(b) => Ok(*b),
            // Numeric-like: 0 => false, non-zero => true
            NGValue::Int8(x) => Ok(*x != 0),
            NGValue::UInt8(x) => Ok(*x != 0),
            NGValue::Int16(x) => Ok(*x != 0),
            NGValue::UInt16(x) => Ok(*x != 0),
            NGValue::Int32(x) => Ok(*x != 0),
            NGValue::UInt32(x) => Ok(*x != 0),
            NGValue::Int64(x) => Ok(*x != 0),
            NGValue::UInt64(x) => Ok(*x != 0),
            NGValue::Float32(x) => {
                let f = *x as f64;
                if !f.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                Ok(f != 0.0)
            }
            NGValue::Float64(x) => {
                if !x.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                Ok(*x != 0.0)
            }
            NGValue::Timestamp(ms) => Ok(*ms != 0),
            NGValue::String(s) => {
                parse_bool_from_str(s.as_ref()).ok_or_else(|| NGValueCastError::ParseError {
                    target: "bool",
                    value: s.to_string(),
                })
            }
            // Binary: treat as raw bytes, not UTF-8 digits.
            // - empty => false
            // - any non-zero byte => true
            NGValue::Binary(b) => Ok(b.as_ref().iter().any(|x| *x != 0)),
        }
    }
}

impl TryFrom<&NGValue> for i8 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => Ok(if *x { 1 } else { 0 }),
            NGValue::Int8(x) => Ok(*x),
            NGValue::UInt8(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::Int16(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::UInt16(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::Int32(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::UInt32(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::Int64(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::UInt64(x) => {
                i8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i8::MIN as f64 && r <= i8::MAX as f64 {
                    Ok(r as i8)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i8" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i8::MIN as f64 && r <= i8::MAX as f64 {
                    Ok(r as i8)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i8" })
                }
            }
            NGValue::Timestamp(ms) => {
                i8::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::String(s) => {
                let n = parse_i64_from_str("i8", s.as_ref())?;
                i8::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "i8" })
            }
            NGValue::Binary(b) => binary_to_i8(b, "i8"),
        }
    }
}

impl TryFrom<&NGValue> for u8 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => {
                if *x {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            NGValue::UInt8(x) => Ok(*x),
            NGValue::Int8(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::UInt16(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::Int16(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::UInt32(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::Int32(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::UInt64(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::Int64(x) => {
                u8::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u8::MAX as f64 {
                    Ok(r as u8)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u8" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u8::MAX as f64 {
                    Ok(r as u8)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u8" })
                }
            }
            NGValue::Timestamp(ms) => {
                u8::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::String(s) => {
                let n = parse_u64_from_str("u8", s.as_ref())?;
                u8::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "u8" })
            }
            NGValue::Binary(b) => binary_to_u8(b, "u8"),
        }
    }
}

impl TryFrom<&NGValue> for u16 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => Ok(if *x { 1 } else { 0 }),
            NGValue::UInt16(x) => Ok(*x),
            NGValue::UInt8(x) => Ok(*x as u16),
            NGValue::Int8(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::Int16(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::Int32(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::UInt32(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::Int64(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::UInt64(x) => {
                u16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u16::MAX as f64 {
                    Ok(r as u16)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u16" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u16::MAX as f64 {
                    Ok(r as u16)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u16" })
                }
            }
            NGValue::Timestamp(ms) => {
                u16::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::String(s) => {
                let n = parse_u64_from_str("u16", s.as_ref())?;
                u16::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "u16" })
            }
            NGValue::Binary(b) => binary_to_u16_be(b, "u16"),
        }
    }
}

impl TryFrom<&NGValue> for i16 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => {
                if *x {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            NGValue::Int16(x) => Ok(*x),
            NGValue::Int8(x) => Ok(*x as i16),
            NGValue::UInt8(x) => Ok(*x as i16),
            NGValue::UInt16(x) => {
                i16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::Int32(x) => {
                i16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::UInt32(x) => {
                i16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::Int64(x) => {
                i16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::UInt64(x) => {
                i16::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i16::MIN as f64 && r <= i16::MAX as f64 {
                    Ok(r as i16)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i16" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i16::MIN as f64 && r <= i16::MAX as f64 {
                    Ok(r as i16)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i16" })
                }
            }
            NGValue::Timestamp(ms) => {
                i16::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::String(s) => {
                let n = parse_i64_from_str("i16", s.as_ref())?;
                i16::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "i16" })
            }
            NGValue::Binary(b) => binary_to_i16_be(b, "i16"),
        }
    }
}

impl TryFrom<&NGValue> for i32 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => {
                if *x {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            NGValue::Int32(x) => Ok(*x),
            NGValue::Int16(x) => Ok(*x as i32),
            NGValue::Int8(x) => Ok(*x as i32),
            NGValue::UInt8(x) => Ok(*x as i32),
            NGValue::UInt16(x) => Ok(*x as i32),
            NGValue::UInt32(x) => {
                i32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i32" })
            }
            NGValue::Int64(x) => {
                i32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i32" })
            }
            NGValue::UInt64(x) => {
                i32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i32" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i32::MIN as f64 && r <= i32::MAX as f64 {
                    Ok(r as i32)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i32" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i32::MIN as f64 && r <= i32::MAX as f64 {
                    Ok(r as i32)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i32" })
                }
            }
            NGValue::Timestamp(ms) => {
                i32::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "i32" })
            }
            NGValue::String(s) => {
                let n = parse_i64_from_str("i32", s.as_ref())?;
                i32::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "i32" })
            }
            NGValue::Binary(b) => binary_to_i32_be(b, "i32"),
        }
    }
}

impl TryFrom<&NGValue> for u32 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => Ok(if *x { 1 } else { 0 }),
            NGValue::UInt32(x) => Ok(*x),
            NGValue::UInt16(x) => Ok(*x as u32),
            NGValue::UInt8(x) => Ok(*x as u32),
            NGValue::Int8(x) => {
                u32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::Int16(x) => {
                u32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::Int32(x) => {
                u32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::Int64(x) => {
                u32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::UInt64(x) => {
                u32::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u32::MAX as f64 {
                    Ok(r as u32)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u32" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u32::MAX as f64 {
                    Ok(r as u32)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u32" })
                }
            }
            NGValue::Timestamp(ms) => {
                u32::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::String(s) => {
                let n = parse_u64_from_str("u32", s.as_ref())?;
                u32::try_from(n).map_err(|_| NGValueCastError::OutOfRange { target: "u32" })
            }
            NGValue::Binary(b) => binary_to_u32_be(b, "u32"),
        }
    }
}

impl TryFrom<&NGValue> for i64 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => {
                if *x {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            NGValue::Int64(x) => Ok(*x),
            NGValue::Int32(x) => Ok(*x as i64),
            NGValue::Int16(x) => Ok(*x as i64),
            NGValue::Int8(x) => Ok(*x as i64),
            NGValue::UInt8(x) => Ok(*x as i64),
            NGValue::UInt16(x) => Ok(*x as i64),
            NGValue::UInt32(x) => Ok(*x as i64),
            NGValue::UInt64(x) => {
                i64::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "i64" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i64::MIN as f64 && r <= i64::MAX as f64 {
                    Ok(r as i64)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i64" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= i64::MIN as f64 && r <= i64::MAX as f64 {
                    Ok(r as i64)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "i64" })
                }
            }
            NGValue::Timestamp(ms) => Ok(*ms),
            NGValue::String(s) => parse_i64_from_str("i64", s.as_ref()),
            NGValue::Binary(b) => binary_to_i64_be(b, "i64"),
        }
    }
}

impl TryFrom<&NGValue> for u64 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::Boolean(x) => {
                if *x {
                    Ok(1)
                } else {
                    Ok(0)
                }
            }
            NGValue::UInt64(x) => Ok(*x),
            NGValue::UInt32(x) => Ok(*x as u64),
            NGValue::UInt16(x) => Ok(*x as u64),
            NGValue::UInt8(x) => Ok(*x as u64),
            NGValue::Int8(x) => {
                u64::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u64" })
            }
            NGValue::Int16(x) => {
                u64::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u64" })
            }
            NGValue::Int32(x) => {
                u64::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u64" })
            }
            NGValue::Int64(x) => {
                u64::try_from(*x).map_err(|_| NGValueCastError::OutOfRange { target: "u64" })
            }
            NGValue::Float32(x) => {
                let r = (*x as f64).round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u64::MAX as f64 {
                    Ok(r as u64)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u64" })
                }
            }
            NGValue::Float64(x) => {
                let r = x.round();
                if !r.is_finite() {
                    return Err(NGValueCastError::NotFinite);
                }
                if r >= 0.0 && r <= u64::MAX as f64 {
                    Ok(r as u64)
                } else {
                    Err(NGValueCastError::OutOfRange { target: "u64" })
                }
            }
            NGValue::Timestamp(ms) => {
                u64::try_from(*ms).map_err(|_| NGValueCastError::OutOfRange { target: "u64" })
            }
            NGValue::String(s) => parse_u64_from_str("u64", s.as_ref()),
            NGValue::Binary(b) => binary_to_u64_be(b, "u64"),
        }
    }
}

impl TryFrom<&NGValue> for f64 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        let f = match v {
            NGValue::Boolean(x) => {
                if *x {
                    1.0
                } else {
                    0.0
                }
            }
            NGValue::Int8(x) => *x as f64,
            NGValue::UInt8(x) => *x as f64,
            NGValue::Int16(x) => *x as f64,
            NGValue::UInt16(x) => *x as f64,
            NGValue::Int32(x) => *x as f64,
            NGValue::UInt32(x) => *x as f64,
            NGValue::Int64(x) => *x as f64,
            NGValue::UInt64(x) => *x as f64,
            NGValue::Float32(x) => *x as f64,
            NGValue::Float64(x) => *x,
            NGValue::Timestamp(ms) => *ms as f64,
            NGValue::String(s) => parse_f64_from_str("f64", s.as_ref())?,
            NGValue::Binary(b) => match b.len() {
                4 => {
                    let f = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                    f as f64
                }
                8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
                other => return Err(binary_len_err("f64", "4 or 8", other)),
            },
        };

        if !f.is_finite() {
            return Err(NGValueCastError::NotFinite);
        }
        Ok(f)
    }
}

impl TryFrom<&NGValue> for f32 {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        let f = f64::try_from(v)?;
        // f64::try_from already checks NotFinite
        if f < f32::MIN as f64 || f > f32::MAX as f64 {
            return Err(NGValueCastError::OutOfRange { target: "f32" });
        }
        Ok(f as f32)
    }
}

impl TryFrom<&NGValue> for String {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        match v {
            NGValue::String(s) => Ok(s.to_string()),
            NGValue::Binary(b) => Ok(base64::engine::general_purpose::STANDARD.encode(b.as_ref())),
            NGValue::Timestamp(ms) => {
                match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(*ms) {
                    Some(dt) => Ok(dt.to_rfc3339()),
                    None => Ok(ms.to_string()),
                }
            }
            NGValue::Boolean(b) => Ok(b.to_string()),
            NGValue::Int8(n) => Ok(n.to_string()),
            NGValue::UInt8(n) => Ok(n.to_string()),
            NGValue::Int16(n) => Ok(n.to_string()),
            NGValue::UInt16(n) => Ok(n.to_string()),
            NGValue::Int32(n) => Ok(n.to_string()),
            NGValue::UInt32(n) => Ok(n.to_string()),
            NGValue::Int64(n) => Ok(n.to_string()),
            NGValue::UInt64(n) => Ok(n.to_string()),
            NGValue::Float32(n) => Ok(n.to_string()),
            NGValue::Float64(n) => Ok(n.to_string()),
        }
    }
}

/// Best-effort string view for telemetry encoding / logging.
///
/// Unlike `TryFrom<&NGValue> for &str` (which can only borrow `NGValue::String`),
/// this conversion can represent **all** `NGValue` variants by allocating when needed.
impl<'a> TryFrom<&'a NGValue> for Cow<'a, str> {
    type Error = NGValueCastError;

    #[inline]
    fn try_from(v: &'a NGValue) -> Result<Self, Self::Error> {
        Ok(match v {
            NGValue::String(s) => Cow::Borrowed(s.as_ref()),
            NGValue::Binary(b) => {
                Cow::Owned(base64::engine::general_purpose::STANDARD.encode(b.as_ref()))
            }
            NGValue::Timestamp(ms) => {
                match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(*ms) {
                    Some(dt) => Cow::Owned(dt.to_rfc3339()),
                    None => Cow::Owned(ms.to_string()),
                }
            }
            NGValue::Boolean(b) => Cow::Owned(b.to_string()),
            NGValue::Int8(n) => Cow::Owned(n.to_string()),
            NGValue::UInt8(n) => Cow::Owned(n.to_string()),
            NGValue::Int16(n) => Cow::Owned(n.to_string()),
            NGValue::UInt16(n) => Cow::Owned(n.to_string()),
            NGValue::Int32(n) => Cow::Owned(n.to_string()),
            NGValue::UInt32(n) => Cow::Owned(n.to_string()),
            NGValue::Int64(n) => Cow::Owned(n.to_string()),
            NGValue::UInt64(n) => Cow::Owned(n.to_string()),
            NGValue::Float32(n) => Cow::Owned(n.to_string()),
            NGValue::Float64(n) => Cow::Owned(n.to_string()),
        })
    }
}

impl TryFrom<&NGValue> for Bytes {
    type Error = NGValueCastError;

    /// Canonical byte encoding (big-endian for numeric primitives).
    ///
    /// This avoids cloning the whole `NGValue` when the caller already has a reference.
    /// Note: `Bytes::clone()` is cheap (ref-counted).
    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        Ok(match v {
            NGValue::Binary(b) => b.clone(),
            NGValue::String(s) => Bytes::copy_from_slice(s.as_ref().as_bytes()),
            NGValue::Boolean(b) => Bytes::from(vec![if *b { 1 } else { 0 }]),
            NGValue::Int8(n) => Bytes::from(vec![*n as u8]),
            NGValue::UInt8(n) => Bytes::from(vec![*n]),
            NGValue::Int16(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt16(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Int32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Int64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Float32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Float64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Timestamp(ms) => Bytes::copy_from_slice(&ms.to_be_bytes()),
        })
    }
}

impl TryFrom<&NGValue> for Vec<u8> {
    type Error = NGValueCastError;

    /// Canonical byte encoding (big-endian for numeric primitives).
    ///
    /// This avoids cloning the whole `NGValue` when the caller already has a reference.
    #[inline]
    fn try_from(v: &NGValue) -> Result<Self, Self::Error> {
        Ok(match v {
            NGValue::Binary(b) => b.as_ref().to_vec(),
            NGValue::String(s) => s.as_ref().as_bytes().to_vec(),
            NGValue::Boolean(b) => vec![if *b { 1 } else { 0 }],
            NGValue::Int8(n) => vec![*n as u8],
            NGValue::UInt8(n) => vec![*n],
            NGValue::Int16(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt16(n) => n.to_be_bytes().to_vec(),
            NGValue::Int32(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt32(n) => n.to_be_bytes().to_vec(),
            NGValue::Int64(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt64(n) => n.to_be_bytes().to_vec(),
            NGValue::Float32(n) => n.to_be_bytes().to_vec(),
            NGValue::Float64(n) => n.to_be_bytes().to_vec(),
            NGValue::Timestamp(ms) => ms.to_be_bytes().to_vec(),
        })
    }
}

impl TryFrom<NGValue> for Bytes {
    type Error = NGValueCastError;

    /// Canonical byte encoding (big-endian for numeric primitives).
    #[inline]
    fn try_from(v: NGValue) -> Result<Self, Self::Error> {
        Ok(match v {
            NGValue::Binary(b) => b,
            NGValue::String(s) => Bytes::copy_from_slice(s.as_ref().as_bytes()),
            NGValue::Boolean(b) => Bytes::from(vec![if b { 1 } else { 0 }]),
            NGValue::Int8(n) => Bytes::from(vec![n as u8]),
            NGValue::UInt8(n) => Bytes::from(vec![n]),
            NGValue::Int16(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt16(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Int32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Int64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::UInt64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Float32(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Float64(n) => Bytes::copy_from_slice(&n.to_be_bytes()),
            NGValue::Timestamp(ms) => Bytes::copy_from_slice(&ms.to_be_bytes()),
        })
    }
}

impl TryFrom<NGValue> for Vec<u8> {
    type Error = NGValueCastError;

    /// Canonical byte encoding (big-endian for numeric primitives).
    #[inline]
    fn try_from(v: NGValue) -> Result<Self, Self::Error> {
        Ok(match v {
            NGValue::Binary(b) => b.as_ref().to_vec(),
            NGValue::String(s) => s.as_ref().as_bytes().to_vec(),
            NGValue::Boolean(b) => vec![if b { 1 } else { 0 }],
            NGValue::Int8(n) => vec![n as u8],
            NGValue::UInt8(n) => vec![n],
            NGValue::Int16(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt16(n) => n.to_be_bytes().to_vec(),
            NGValue::Int32(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt32(n) => n.to_be_bytes().to_vec(),
            NGValue::Int64(n) => n.to_be_bytes().to_vec(),
            NGValue::UInt64(n) => n.to_be_bytes().to_vec(),
            NGValue::Float32(n) => n.to_be_bytes().to_vec(),
            NGValue::Float64(n) => n.to_be_bytes().to_vec(),
            NGValue::Timestamp(ms) => ms.to_be_bytes().to_vec(),
        })
    }
}

impl Serialize for NGValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Default encoding: binary base64 + timestamp unix_ms.
        let v = self.to_json_value(NGValueJsonOptions::default());
        v.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NGValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // This deserializer is only intended for non-hot-path usages (e.g. debugging).
        // It cannot precisely infer integer widths, so it chooses Int64/Float64 where needed.
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::Bool(b) => Ok(NGValue::Boolean(b)),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(NGValue::Int64(i))
                } else if let Some(u) = n.as_u64() {
                    Ok(NGValue::UInt64(u))
                } else if let Some(f) = n.as_f64() {
                    Ok(NGValue::Float64(f))
                } else {
                    Err(de::Error::custom("invalid JSON number"))
                }
            }
            serde_json::Value::String(s) => Ok(NGValue::String(Arc::<str>::from(s))),
            serde_json::Value::Null => {
                Err(de::Error::custom("null cannot be converted to NGValue"))
            }
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => Err(de::Error::custom(
                "array/object cannot be converted to NGValue without type information",
            )),
        }
    }
}

/// A single point update.
///
/// `point_id` is the primary key for all hot-path operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointValue {
    /// Point identifier (primary key).
    pub point_id: i32,
    /// Stable point key within a device (string identifier).
    pub point_key: Arc<str>,
    /// Strongly-typed value.
    pub value: NGValue,
}
