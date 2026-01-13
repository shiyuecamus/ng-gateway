use crate::{
    protocol::{
        codec::{decode_bcd_to_f64, encode_f64_to_bcd},
        frame::Dl645TypedFrame,
    },
    types::{Dl645Parameter, Dl645Point, Dl645Version},
};
use bytes::Bytes;
use ng_gateway_sdk::{DataType, DriverError, DriverResult, NGValue, NGValueCastError, ValueCodec};
use std::sync::Arc;

/// High-level DL/T 645 value codec.
///
/// This codec bridges protocol-level frames (after 0x33 offset removal) and
/// gateway-facing JSON values based on the point / parameter metadata. Keeping
/// this logic here avoids bloating the driver implementation and mirrors the
/// `S7Codec` design in the S7 driver.
pub struct Dl645Codec;

impl Dl645Codec {
    /// Decode a single point value from a DL/T 645 response frame.
    pub fn decode_point_value(
        version: Dl645Version,
        point: &Dl645Point,
        frame: &Dl645TypedFrame,
    ) -> DriverResult<NGValue> {
        let di_len: usize = match version {
            Dl645Version::V1997 => 2,
            _ => 4,
        };

        let data = frame.body.data_payload().unwrap_or(&[]);

        if data.len() <= di_len {
            return Err(DriverError::CodecError(
                "DL/T 645 response payload too short".to_string(),
            ));
        }

        // Optional DI echo validation (best-effort).
        {
            let expected_di = point.di;
            let di_bytes = &data[..di_len];

            // Reconstruct DI from the on-wire little-endian bytes. For
            // DL/T 645-1997 only the lower 16 bits (DI1, DI0) are present on
            // the wire, while configuration still uses a 32-bit value. In that
            // case we compare only the low 16 bits for a best-effort check.
            let mut di_val: u32 = 0;
            for (i, b) in di_bytes.iter().enumerate() {
                di_val |= (*b as u32) << (8 * i);
            }

            let (expected_cmp, actual_cmp) = match version {
                Dl645Version::V1997 => (expected_di & 0xFFFF, di_val & 0xFFFF),
                _ => (expected_di, di_val),
            };

            if actual_cmp != expected_cmp {
                tracing::warn!(
                    expected = format!("{expected_di:08X}"),
                    actual = format!("{di_val:08X}"),
                    "DL/T 645 DI mismatch in response"
                );
            }
        }

        let payload = &data[di_len..];

        match point.data_type {
            DataType::Float32 | DataType::Float64 => {
                let decimals = point.decimals.unwrap_or(2);
                let v = decode_bcd_to_f64(payload, decimals, true)?;
                ValueCodec::coerce_f64_to_value(v, point.data_type, point.scale).ok_or(
                    DriverError::CodecError(
                        "DL/T 645 value out of range after scaling".to_string(),
                    ),
                )
            }
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
                let decimals = point.decimals.unwrap_or(0);
                let v = decode_bcd_to_f64(payload, decimals, true)?;
                ValueCodec::coerce_f64_to_value(v, point.data_type, point.scale).ok_or(
                    DriverError::CodecError(
                        "DL/T 645 value out of range after scaling".to_string(),
                    ),
                )
            }
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                let decimals = point.decimals.unwrap_or(0);
                let v = decode_bcd_to_f64(payload, decimals, false)?;
                ValueCodec::coerce_f64_to_value(v, point.data_type, point.scale).ok_or(
                    DriverError::CodecError(
                        "DL/T 645 value out of range after scaling".to_string(),
                    ),
                )
            }
            DataType::Boolean => {
                let b = payload.first().copied().unwrap_or(0);
                ValueCodec::coerce_bool_to_value((b & 0x01) != 0, DataType::Boolean, None).ok_or(
                    DriverError::CodecError("DL/T 645 bool coercion failed".to_string()),
                )
            }
            DataType::String => {
                // Best-effort: treat payload as ASCII/UTF-8, trim trailing zeros.
                let mut buf = payload.to_vec();
                while let Some(true) = buf.last().map(|b| *b == 0) {
                    buf.pop();
                }
                match String::from_utf8(buf) {
                    Ok(s) => Ok(NGValue::String(Arc::<str>::from(s))),
                    Err(_) => Ok(NGValue::Binary(Bytes::copy_from_slice(payload))),
                }
            }
            DataType::Timestamp => {
                let decimals = point.decimals.unwrap_or(0);
                let v = decode_bcd_to_f64(payload, decimals, false)?;
                ValueCodec::coerce_f64_to_value(v, DataType::Timestamp, point.scale).ok_or_else(
                    || {
                        DriverError::CodecError(
                            "DL/T 645 value out of range for timestamp".to_string(),
                        )
                    },
                )
            }
            DataType::Binary => Ok(NGValue::Binary(Bytes::copy_from_slice(payload))),
        }
    }

    /// Typed version of `encode_parameter_value` that accepts `NGValue` directly.
    ///
    /// # Best practice
    /// `write_point` receives `NGValue` (already typed) and should avoid allocating a JSON value
    /// only to parse primitives back out.
    pub fn encode_parameter_value(
        param: &Dl645Parameter,
        value: &NGValue,
    ) -> DriverResult<Vec<u8>> {
        match param.data_type {
            DataType::Float32 | DataType::Float64 => {
                let v = f64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected floating value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                let decimals = param.decimals.unwrap_or(2);
                encode_f64_to_bcd(v, decimals, true)
            }
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
                let v = f64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected numeric value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                let decimals = param.decimals.unwrap_or(0);
                encode_f64_to_bcd(v, decimals, true)
            }
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                let v = f64::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected numeric value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                let decimals = param.decimals.unwrap_or(0);
                encode_f64_to_bcd(v, decimals, false)
            }
            DataType::Boolean => {
                let b = bool::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected boolean value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(vec![if b { 0x01 } else { 0x00 }])
            }
            DataType::String => {
                let s = String::try_from(value).map_err(|e: NGValueCastError| {
                    DriverError::ValidationError(format!(
                        "Expected string value, got {:?}: {e}",
                        value.data_type()
                    ))
                })?;
                Ok(s.as_bytes().to_vec())
            }
            _ => Err(DriverError::ConfigurationError(format!(
                "Unsupported parameter data type for DL/T 645 action: {:?}",
                param.data_type
            ))),
        }
    }
}
