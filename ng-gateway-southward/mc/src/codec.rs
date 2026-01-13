use bytes::Bytes;
use ng_gateway_sdk::{DataType, DriverError, DriverResult, NGValue, NGValueCastError};
use serde_json::Value;
use std::sync::Arc;

/// MC value codec for converting between raw MC bytes and gateway values.
///
/// This helper focuses on the primitive scalar types used by the first
/// iteration of the MC driver. It intentionally keeps the mapping simple
/// and aligned with the gateway `DataType` enum. Complex container types
/// (arrays/structures) can be added later if needed.
pub struct McCodec;

impl McCodec {
    /// Encode a JSON value into MC raw bytes according to the given data type.
    ///
    /// The current implementation assumes little-endian encoding for all
    /// multi-byte numeric types, which matches the MC binary specification.
    pub fn encode(data_type: DataType, value: &Value) -> DriverResult<Vec<u8>> {
        match data_type {
            DataType::Boolean => {
                let b = value.as_bool().ok_or(DriverError::ExecutionError(
                    "MC encode: expected boolean JSON value for DataType::Boolean".to_string(),
                ))?;
                Ok(vec![if b { 1 } else { 0 }])
            }
            DataType::Int8 => {
                let v = value.as_i64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected integer JSON value for DataType::Int8".to_string(),
                ))? as i8;
                Ok(vec![v as u8])
            }
            DataType::UInt8 => {
                let v = value.as_u64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected unsigned integer JSON value for DataType::UInt8"
                        .to_string(),
                ))? as u8;
                Ok(vec![v])
            }
            DataType::Int16 => {
                let v = value.as_i64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected integer JSON value for DataType::Int16".to_string(),
                ))? as i16;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt16 => {
                let v = value.as_u64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected unsigned integer JSON value for DataType::UInt16"
                        .to_string(),
                ))? as u16;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Int32 => {
                let v = value.as_i64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected integer JSON value for DataType::Int32".to_string(),
                ))? as i32;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt32 => {
                let v = value.as_u64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected unsigned integer JSON value for DataType::UInt32"
                        .to_string(),
                ))? as u32;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Int64 => {
                let v = value.as_i64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected integer JSON value for DataType::Int64".to_string(),
                ))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt64 => {
                let v = value.as_u64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected unsigned integer JSON value for DataType::UInt64"
                        .to_string(),
                ))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Float32 => {
                let v = value.as_f64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected number JSON value for DataType::Float32".to_string(),
                ))? as f32;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Float64 => {
                let v = value.as_f64().ok_or(DriverError::ExecutionError(
                    "MC encode: expected number JSON value for DataType::Float64".to_string(),
                ))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::String => {
                let s = value.as_str().ok_or(DriverError::ExecutionError(
                    "MC encode: expected string JSON value for DataType::String".to_string(),
                ))?;
                Ok(s.as_bytes().to_vec())
            }
            // Binary and Timestamp are not yet mapped to a concrete MC representation
            // in this version of the driver.
            DataType::Binary | DataType::Timestamp => Err(DriverError::ExecutionError(
                "MC encode: DataType::Binary/Timestamp is not supported yet".to_string(),
            )),
        }
    }

    /// Encode a typed `NGValue` into MC raw bytes according to the given data type.
    ///
    /// This avoids a JSON round-trip on the `execute_action` path.
    pub fn encode_typed(data_type: DataType, value: &NGValue) -> DriverResult<Vec<u8>> {
        let err = |msg: &str| DriverError::ExecutionError(msg.to_string());
        match data_type {
            DataType::Boolean => {
                let b: bool = value.try_into().map_err(|e: NGValueCastError| {
                    err(&format!("MC encode: expected bool: {e}"))
                })?;
                Ok(vec![if b { 1 } else { 0 }])
            }
            DataType::Int8 => {
                let v: i8 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected i8: {e}")))?;
                Ok(vec![v as u8])
            }
            DataType::UInt8 => {
                let v: u8 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected u8: {e}")))?;
                Ok(vec![v])
            }
            DataType::Int16 => {
                let v: i16 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected i16: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt16 => {
                let v: u16 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected u16: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Int32 => {
                let v: i32 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected i32: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt32 => {
                let v: u32 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected u32: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Int64 => {
                let v: i64 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected i64: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::UInt64 => {
                let v: u64 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected u64: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Float32 => {
                let v: f32 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected f32: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::Float64 => {
                let v: f64 = value
                    .try_into()
                    .map_err(|e: NGValueCastError| err(&format!("MC encode: expected f64: {e}")))?;
                Ok(v.to_le_bytes().to_vec())
            }
            DataType::String => match value {
                NGValue::String(s) => Ok(s.as_bytes().to_vec()),
                _ => Err(err("MC encode: expected string for DataType::String")),
            },
            DataType::Binary => match value {
                NGValue::Binary(b) => Ok(b.to_vec()),
                _ => Err(err("MC encode: expected binary for DataType::Binary")),
            },
            DataType::Timestamp => {
                let v: i64 = value.try_into().map_err(|e: NGValueCastError| {
                    err(&format!("MC encode: expected timestamp(i64 ms): {e}"))
                })?;
                Ok(v.to_le_bytes().to_vec())
            }
        }
    }

    /// Decode MC raw bytes into a strongly-typed `NGValue` according to the given data type.
    ///
    /// This function assumes that the payload slice contains exactly the
    /// bytes for a single logical value. The caller is responsible for
    /// slicing multi-element payloads appropriately.
    ///
    /// # Performance
    /// This is the preferred hot-path API for southbound drivers: it avoids
    /// allocating `serde_json::Value` intermediates.
    pub fn decode(data_type: DataType, payload: &[u8]) -> DriverResult<NGValue> {
        match data_type {
            DataType::Boolean => {
                let b = payload.first().copied().unwrap_or(0) != 0;
                Ok(NGValue::Boolean(b))
            }
            DataType::Int8 => {
                if payload.is_empty() {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Int8".to_string(),
                    ));
                }
                Ok(NGValue::Int8(payload[0] as i8))
            }
            DataType::UInt8 => {
                if payload.is_empty() {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::UInt8".to_string(),
                    ));
                }
                Ok(NGValue::UInt8(payload[0]))
            }
            DataType::Int16 => {
                if payload.len() < 2 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Int16".to_string(),
                    ));
                }
                let v = i16::from_le_bytes([payload[0], payload[1]]);
                Ok(NGValue::Int16(v))
            }
            DataType::UInt16 => {
                if payload.len() < 2 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::UInt16".to_string(),
                    ));
                }
                let v = u16::from_le_bytes([payload[0], payload[1]]);
                Ok(NGValue::UInt16(v))
            }
            DataType::Int32 => {
                if payload.len() < 4 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Int32".to_string(),
                    ));
                }
                let v = i32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                Ok(NGValue::Int32(v))
            }
            DataType::UInt32 => {
                if payload.len() < 4 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::UInt32".to_string(),
                    ));
                }
                let v = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                Ok(NGValue::UInt32(v))
            }
            DataType::Int64 => {
                if payload.len() < 8 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Int64".to_string(),
                    ));
                }
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&payload[..8]);
                let v = i64::from_le_bytes(buf);
                Ok(NGValue::Int64(v))
            }
            DataType::UInt64 => {
                if payload.len() < 8 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::UInt64".to_string(),
                    ));
                }
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&payload[..8]);
                let v = u64::from_le_bytes(buf);
                Ok(NGValue::UInt64(v))
            }
            DataType::Float32 => {
                if payload.len() < 4 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Float32".to_string(),
                    ));
                }
                let mut buf = [0u8; 4];
                buf.copy_from_slice(&payload[..4]);
                let v = f32::from_le_bytes(buf);
                Ok(NGValue::Float32(v))
            }
            DataType::Float64 => {
                if payload.len() < 8 {
                    return Err(DriverError::ExecutionError(
                        "MC decode: insufficient bytes for DataType::Float64".to_string(),
                    ));
                }
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&payload[..8]);
                let v = f64::from_le_bytes(buf);
                Ok(NGValue::Float64(v))
            }
            DataType::String => {
                // NOTE: `from_utf8_lossy` guarantees valid UTF-8 output; we store it as `Arc<str>`
                // to share allocations when cloning `NGValue::String`.
                let s = String::from_utf8_lossy(payload);
                Ok(NGValue::String(Arc::<str>::from(s.as_ref())))
            }
            DataType::Binary => Ok(NGValue::Binary(Bytes::copy_from_slice(payload))),
            // Timestamp is not yet mapped to a concrete MC representation in this
            // version of the driver.
            DataType::Timestamp => Err(DriverError::ExecutionError(
                "MC decode: DataType::Timestamp is not supported yet".to_string(),
            )),
        }
    }
}
