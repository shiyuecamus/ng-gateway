use bytes::Bytes;
use chrono::Utc;
use ng_gateway_sdk::{DataType, NGValue, ValueCodec};
use opcua::types::{ByteString, UAString, Variant};
use std::sync::Arc;

/// OPC UA specific codec for converting between UA Variant and serde_json::Value
/// with awareness of declared logical DataType and optional scale.
pub struct OpcUaCodec;

impl OpcUaCodec {
    #[inline]
    fn numeric_as_f64(value: &Variant) -> Option<f64> {
        match value {
            Variant::SByte(n) => Some(*n as f64),
            Variant::Byte(n) => Some(*n as f64),
            Variant::Int16(n) => Some(*n as f64),
            Variant::UInt16(n) => Some(*n as f64),
            Variant::Int32(n) => Some(*n as f64),
            Variant::UInt32(n) => Some(*n as f64),
            Variant::Int64(n) => Some(*n as f64),
            Variant::UInt64(n) => Some(*n as f64),
            Variant::Float(f) => Some(*f as f64),
            Variant::Double(f) => Some(*f),
            _ => None,
        }
    }

    /// Convert UA Variant to a strongly-typed `NGValue`.
    ///
    /// # Hot path
    /// This API is intended for telemetry/attributes encoding paths and avoids creating
    /// `serde_json::Value` intermediates.
    ///
    /// # Limitations
    /// Arrays are currently not supported by `NGValue` and will return `None`.
    #[inline]
    pub fn coerce_variant_value(
        value: &Variant,
        expected: DataType,
        scale: Option<f64>,
    ) -> Option<NGValue> {
        match value {
            Variant::Array(_) => None,
            Variant::Boolean(b) => ValueCodec::coerce_bool_to_value(*b, expected, scale),
            Variant::String(s) => match expected {
                DataType::String => Some(NGValue::String(Arc::<str>::from(s.as_ref()))),
                DataType::Timestamp => chrono::DateTime::parse_from_rfc3339(s.as_ref())
                    .ok()
                    .map(|dt| NGValue::Timestamp(dt.timestamp_millis())),
                DataType::Binary => ValueCodec::hex_string_to_bytes(s.as_ref())
                    .map(Bytes::from)
                    .map(NGValue::Binary),
                _ => {
                    // Numeric fallback: parse as f64 and then coerce.
                    s.as_ref()
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(|n| ValueCodec::coerce_f64_to_value(n, expected, scale))
                }
            },
            Variant::ByteString(b) => match expected {
                DataType::Binary => Some(NGValue::Binary(Bytes::copy_from_slice(b.as_ref()))),
                _ => None,
            },
            Variant::DateTime(dt) => match expected {
                DataType::Timestamp => Some(NGValue::Timestamp(dt.as_chrono().timestamp_millis())),
                DataType::String => Some(NGValue::String(Arc::<str>::from(
                    dt.as_chrono().to_rfc3339(),
                ))),
                _ => None,
            },
            _ => {
                // Numeric (fast) path.
                Self::numeric_as_f64(value).and_then(|n| {
                    ValueCodec::coerce_f64_to_value(n, expected, scale)
                        .or_else(|| ValueCodec::coerce_bool_to_value(n != 0.0, expected, scale))
                })
            }
        }
    }

    /// Convert a strongly-typed `NGValue` into an OPC UA `Variant` according to the expected `DataType`.
    ///
    /// # Best practice
    /// `Driver::write_point` should use this typed API to avoid allocating/interpreting
    /// `serde_json::Value` on the hot path. Keep `value_to_variant` for `execute_action`
    /// which supports object parameters.
    #[inline]
    pub fn value_to_variant(v: &NGValue, dt: DataType) -> Option<Variant> {
        match dt {
            DataType::Boolean => bool::try_from(v).ok().map(Variant::Boolean),
            DataType::Int8 => i8::try_from(v).ok().map(Variant::SByte),
            DataType::UInt8 => u8::try_from(v).ok().map(Variant::Byte),
            DataType::Int16 => i16::try_from(v).ok().map(Variant::Int16),
            DataType::UInt16 => u16::try_from(v).ok().map(Variant::UInt16),
            DataType::Int32 => i32::try_from(v).ok().map(Variant::Int32),
            DataType::UInt32 => u32::try_from(v).ok().map(Variant::UInt32),
            DataType::Int64 => i64::try_from(v).ok().map(Variant::Int64),
            DataType::UInt64 => u64::try_from(v).ok().map(Variant::UInt64),
            DataType::Float32 => f32::try_from(v).ok().map(Variant::Float),
            DataType::Float64 => f64::try_from(v).ok().map(Variant::Double),
            DataType::String => String::try_from(v)
                .ok()
                .map(|s| Variant::String(UAString::from(s))),
            DataType::Binary => v
                .try_into()
                .ok()
                .map(|b: Vec<u8>| Variant::ByteString(ByteString::from(b))),
            DataType::Timestamp => i64::try_from(v).ok().and_then(|ms| {
                chrono::DateTime::<Utc>::from_timestamp_millis(ms)
                    .map(|dt| Variant::DateTime(Box::new(opcua::types::DateTime::from(dt))))
            }),
        }
    }
}
