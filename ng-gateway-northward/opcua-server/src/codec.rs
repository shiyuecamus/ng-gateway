use chrono::{TimeZone, Utc};
use ng_gateway_sdk::NGValue;
use opcua::types::{ByteString, DateTime, UAString, Variant};

/// Convert NGValue to OPC UA Variant.
///
/// Notes:
/// - Timestamp is treated as Unix time in milliseconds (SDK contract)
/// - String and Binary try to avoid extra copies where possible, but OPC UA types
///   may still require owned buffers.
pub fn value_to_variant(v: &NGValue) -> Variant {
    match v {
        NGValue::Boolean(x) => Variant::Boolean(*x),
        NGValue::Int8(x) => Variant::SByte(*x),
        NGValue::UInt8(x) => Variant::Byte(*x),
        NGValue::Int16(x) => Variant::Int16(*x),
        NGValue::UInt16(x) => Variant::UInt16(*x),
        NGValue::Int32(x) => Variant::Int32(*x),
        NGValue::UInt32(x) => Variant::UInt32(*x),
        NGValue::Int64(x) => Variant::Int64(*x),
        NGValue::UInt64(x) => Variant::UInt64(*x),
        NGValue::Float32(x) => Variant::Float(*x),
        NGValue::Float64(x) => Variant::Double(*x),
        NGValue::String(s) => Variant::String(UAString::from(s.as_ref())),
        NGValue::Binary(b) => Variant::ByteString(ByteString::from(b.as_ref())),
        NGValue::Timestamp(ms) => {
            // best-effort conversion; invalid timestamps map to "now"
            let dt = Utc
                .timestamp_millis_opt(*ms)
                .single()
                .unwrap_or_else(Utc::now);
            Variant::DateTime(Box::new(DateTime::from(dt)))
        }
    }
}
