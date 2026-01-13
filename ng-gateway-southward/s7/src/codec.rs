use super::protocol::frame::{S7DataValue, S7TransportSize};
use bytes::Bytes;
use chrono::{DateTime, NaiveDateTime, NaiveTime, Utc};
use ng_gateway_sdk::{DataType, DriverError, DriverResult, NGValue, ValueCodec};
use std::sync::Arc;

/// S7 protocol specific codec helpers.
///
/// This module centralizes conversions between S7 typed values and
/// gateway-facing JSON values with optional scaling. Keeping this logic
/// here avoids duplication across drivers and preserves a clean separation
/// of concerns in the driver implementation.
pub struct S7Codec;

impl S7Codec {
    /// Convert an S7 typed value into a strongly-typed `NGValue` according to the expected `DataType`.
    ///
    /// # Performance
    /// This is the recommended hot-path conversion API for telemetry/attributes.
    /// It avoids building `serde_json::Value` intermediates.
    #[inline]
    pub fn to_value(
        value: &S7DataValue,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        match value {
            S7DataValue::Bit(b) => ValueCodec::coerce_bool_to_value(*b, expected, scale),
            S7DataValue::Byte(v) => match expected {
                DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(&[*v]))),
                _ => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            },
            S7DataValue::Word(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::Int(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::DWord(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::DInt(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::Real(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::Counter(v) => ValueCodec::coerce_f64_to_value(*v as f64, expected, scale),
            S7DataValue::Char(c) => match expected {
                DataType::String => Some(NGValue::String(Arc::<str>::from(c.to_string()))),
                DataType::Boolean => Some(NGValue::Boolean(*c != '\0')),
                DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(&[(*c as u32)
                    .min(0xFF)
                    as u8]))),
                _ => {
                    ValueCodec::coerce_f64_to_value(((*c as u32).min(0xFF)) as f64, expected, scale)
                }
            },
            S7DataValue::String(s) | S7DataValue::WString(s) => match expected {
                DataType::String => Some(NGValue::String(Arc::<str>::from(s.as_str()))),
                DataType::Timestamp => chrono::DateTime::parse_from_rfc3339(s.as_str())
                    .ok()
                    .map(|dt| NGValue::Timestamp(dt.timestamp_millis())),
                _ => s
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .and_then(|n| ValueCodec::coerce_f64_to_value(n, expected, scale)),
            },
            S7DataValue::Date(d) => match expected {
                DataType::Timestamp => ValueCodec::date_to_epoch_ms(*d).map(NGValue::Timestamp),
                DataType::String => Some(NGValue::String(Arc::<str>::from(d.to_string()))),
                _ => None,
            },
            S7DataValue::DateTime(ndt) | S7DataValue::DateTimeLong(ndt) => match expected {
                DataType::Timestamp => {
                    Some(NGValue::Timestamp(ValueCodec::datetime_to_epoch_ms(*ndt)))
                }
                DataType::UInt64 => {
                    let ms = ValueCodec::datetime_to_epoch_ms(*ndt);
                    if ms >= 0 {
                        Some(NGValue::UInt64(ms as u64))
                    } else {
                        None
                    }
                }
                DataType::Int64 => Some(NGValue::Int64(ValueCodec::datetime_to_epoch_ms(*ndt))),
                DataType::String => Some(NGValue::String(Arc::<str>::from(ndt.to_string()))),
                _ => {
                    let ms = ValueCodec::datetime_to_epoch_ms(*ndt);
                    ValueCodec::coerce_f64_to_value(ms as f64, expected, scale)
                }
            },
            S7DataValue::TimeOfDay(t) => match expected {
                DataType::Timestamp => {
                    Some(NGValue::Timestamp(ValueCodec::time_of_day_to_ms(*t) as i64))
                }
                DataType::String => Some(NGValue::String(Arc::<str>::from(
                    t.format("%H:%M:%S").to_string(),
                ))),
                _ => {
                    let ms = ValueCodec::time_of_day_to_ms(*t) as f64;
                    ValueCodec::coerce_f64_to_value(ms, expected, scale)
                }
            },
            S7DataValue::Time(dur) | S7DataValue::S5Time(dur) | S7DataValue::Timer(dur) => {
                match expected {
                    DataType::Timestamp => {
                        Some(NGValue::Timestamp(ValueCodec::duration_to_ms(*dur)))
                    }
                    DataType::String => Some(NGValue::String(Arc::<str>::from(format!(
                        "{}ms",
                        ValueCodec::duration_to_ms(*dur)
                    )))),
                    _ => {
                        let ms = ValueCodec::duration_to_ms(*dur) as f64;
                        ValueCodec::coerce_f64_to_value(ms, expected, scale)
                    }
                }
            }
        }
    }

    /// Convert a strongly-typed `NGValue` into an S7 typed value according to `S7TransportSize`.
    ///
    /// # Best practice
    /// `Driver::write_point` receives `NGValue` and should avoid round-tripping through
    /// `serde_json::Value` on the hot path. This API keeps `execute_action` flexible (JSON object
    /// parameters) while letting `write_point` stay strongly-typed.
    #[inline]
    pub fn from_value(v: &NGValue, ts: S7TransportSize) -> DriverResult<S7DataValue> {
        match ts {
            S7TransportSize::Bit => Ok(bool::try_from(v).map(S7DataValue::Bit).map_err(|_| {
                DriverError::CodecError("Expected boolean value for S7 Bit".to_string())
            })?),
            S7TransportSize::Char => {
                match v {
                    NGValue::String(s) => s
                        .chars()
                        .next()
                        .map(S7DataValue::Char)
                        .ok_or(DriverError::CodecError("Empty string for CHAR".to_string())),
                    // Allow numeric (0..255) as char code for compatibility.
                    _ => {
                        let n = f64::try_from(v).map_err(|_| {
                            DriverError::CodecError(
                                "Expected string or numeric for CHAR".to_string(),
                            )
                        })?;
                        let r = n.round().clamp(0.0, u8::MAX as f64) as u8;
                        Ok(S7DataValue::Char(r as char))
                    }
                }
            }
            S7TransportSize::Byte => Ok(u8::try_from(v)
                .map(S7DataValue::Byte)
                .map_err(|_| DriverError::CodecError("Expected numeric for u8".to_string()))?),
            S7TransportSize::Word => Ok(u16::try_from(v)
                .map(S7DataValue::Word)
                .map_err(|_| DriverError::CodecError("Expected numeric for u16".to_string()))?),
            S7TransportSize::Int => Ok(i16::try_from(v)
                .map(S7DataValue::Int)
                .map_err(|_| DriverError::CodecError("Expected numeric for i16".to_string()))?),
            S7TransportSize::DWord => Ok(u32::try_from(v)
                .map(S7DataValue::DWord)
                .map_err(|_| DriverError::CodecError("Expected numeric for u32".to_string()))?),
            S7TransportSize::DInt => Ok(i32::try_from(v)
                .map(S7DataValue::DInt)
                .map_err(|_| DriverError::CodecError("Expected numeric for i32".to_string()))?),
            S7TransportSize::Real => Ok(f32::try_from(v)
                .map(S7DataValue::Real)
                .map_err(|_| DriverError::CodecError("Expected numeric for f32".to_string()))?),
            S7TransportSize::String => {
                Ok(String::try_from(v).map(S7DataValue::String).map_err(|_| {
                    DriverError::CodecError("Expected string for S7 String".to_string())
                })?)
            }
            S7TransportSize::WString => {
                Ok(String::try_from(v).map(S7DataValue::WString).map_err(|_| {
                    DriverError::CodecError("Expected string for S7 WString".to_string())
                })?)
            }
            S7TransportSize::Date => {
                // Treat any numeric-like value as unix milliseconds (same semantic as NGValue::Timestamp).
                let ms = i64::try_from(v).map_err(|_| {
                    DriverError::CodecError("Expected timestamp(ms) for S7 Date".to_string())
                })?;
                let dt = DateTime::<Utc>::from_timestamp_millis(ms)
                    .ok_or_else(|| DriverError::CodecError("Invalid timestamp".to_string()))?;
                Ok(S7DataValue::Date(dt.naive_utc().date()))
            }
            S7TransportSize::DateTime | S7TransportSize::DateTimeLong => {
                // Treat any numeric-like value as unix milliseconds (same semantic as NGValue::Timestamp).
                let ms = i64::try_from(v).map_err(|_| {
                    DriverError::CodecError("Expected timestamp(ms) for S7 DateTime".to_string())
                })?;
                let dt = DateTime::<Utc>::from_timestamp_millis(ms)
                    .ok_or_else(|| DriverError::CodecError("Invalid timestamp".to_string()))?;
                let ndt: NaiveDateTime = dt.naive_utc();
                if matches!(ts, S7TransportSize::DateTime) {
                    Ok(S7DataValue::DateTime(ndt))
                } else {
                    Ok(S7DataValue::DateTimeLong(ndt))
                }
            }
            S7TransportSize::TimeOfDay => {
                let n = f64::try_from(v).map_err(|_| {
                    DriverError::CodecError("Expected numeric ms for TimeOfDay".to_string())
                })?;
                let total_ms = n.round().clamp(0.0, (24 * 60 * 60 * 1000 - 1) as f64) as u32;
                let secs = total_ms / 1000;
                let nanos = (total_ms % 1000) * 1_000_000;
                let t = NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
                    .ok_or(DriverError::CodecError("Invalid TimeOfDay".to_string()))?;
                Ok(S7DataValue::TimeOfDay(t))
            }
            S7TransportSize::Time | S7TransportSize::S5Time | S7TransportSize::Timer => {
                let n = f64::try_from(v).map_err(|_| {
                    DriverError::CodecError("Expected numeric ms for TIME/S5TIME/TIMER".to_string())
                })?;
                let ms = n.round() as i64;
                let dur = chrono::Duration::milliseconds(ms);
                match ts {
                    S7TransportSize::Time => Ok(S7DataValue::Time(dur)),
                    S7TransportSize::S5Time => Ok(S7DataValue::S5Time(dur)),
                    S7TransportSize::Timer => Ok(S7DataValue::Timer(dur)),
                    _ => unreachable!(),
                }
            }
            S7TransportSize::Counter => {
                Ok(i32::try_from(v).map(S7DataValue::Counter).map_err(|_| {
                    DriverError::CodecError("Expected numeric for COUNTER".to_string())
                })?)
            }
            S7TransportSize::IECTimer | S7TransportSize::IECEvent | S7TransportSize::HSCounter => {
                Err(DriverError::CodecError(
                    "Unsupported transport size for write".to_string(),
                ))
            }
        }
    }
}
