use ng_gateway_sdk::{DataType, DriverError, DriverResult, NGValue, NGValueCastError, ValueCodec};
use rust_ethernet_ip::PlcValue;
use std::sync::Arc;

/// Ethernet/IP codec for converting between CIP `PlcValue` and Gateway `NGValue`.
///
/// # Design notes
/// - `PlcValue` carries the **actual** CIP scalar type returned by PLC.
/// - Gateway points/parameters carry the **declared** logical `DataType`.
/// - Best practice is to convert **with awareness of the declared `DataType`**:
///   - Uplink: coerce `PlcValue` -> `NGValue` using `DataType` + optional `scale`.
///   - Downlink: cast `NGValue` -> concrete PLC scalar type using `DataType` (range checked).
pub struct EthernetIpCodec;

impl EthernetIpCodec {
    /// Convert a CIP `PlcValue` into an `NGValue` according to the declared logical `DataType`
    /// and optional point `scale`.
    ///
    /// # Best practice
    /// This is the recommended API for uplink value conversions, because it allows:
    /// - PLC actual scalar type != point declared `DataType` (e.g. PLC UDINT -> point Int32)
    /// - optional numeric scaling (`scale`) applied at the edge
    ///
    /// # Limitations
    /// - Complex CIP containers (arrays/structures) are currently not mapped.
    /// - `DataType::Binary` is not supported in current driver version.
    ///
    /// # Timestamp mapping
    /// `DataType::Timestamp` is treated as **Unix epoch milliseconds** and is encoded/decoded
    /// using integer scalars (recommended: PLC `LINT`).
    pub fn to_ng_value(
        value: PlcValue,
        expected: DataType,
        scale: Option<f64>,
    ) -> DriverResult<NGValue> {
        // Reject binary early: for EtherNet/IP, binary usually means SINT[]/BYTE[] which is not
        // represented as a scalar `PlcValue` here.
        if matches!(expected, DataType::Binary) {
            return Err(DriverError::CodecError(
                "EtherNet/IP does not support DataType::Binary in current driver version".into(),
            ));
        }

        let coerced = match value {
            PlcValue::Bool(b) => ValueCodec::coerce_bool_to_value(b, expected, scale),
            PlcValue::Sint(v) => ValueCodec::coerce_i64_to_value(v as i64, expected, scale),
            PlcValue::Int(v) => ValueCodec::coerce_i64_to_value(v as i64, expected, scale),
            PlcValue::Dint(v) => ValueCodec::coerce_i64_to_value(v as i64, expected, scale),
            PlcValue::Lint(v) => ValueCodec::coerce_i64_to_value(v, expected, scale),
            PlcValue::Usint(v) => ValueCodec::coerce_u64_to_value(v as u64, expected, scale),
            PlcValue::Uint(v) => ValueCodec::coerce_u64_to_value(v as u64, expected, scale),
            PlcValue::Udint(v) => ValueCodec::coerce_u64_to_value(v as u64, expected, scale),
            PlcValue::Ulint(v) => ValueCodec::coerce_u64_to_value(v, expected, scale),
            PlcValue::Real(v) => ValueCodec::coerce_f64_to_value(v as f64, expected, scale),
            PlcValue::Lreal(v) => ValueCodec::coerce_f64_to_value(v, expected, scale),
            PlcValue::String(s) => match expected {
                DataType::String => Some(NGValue::String(Arc::<str>::from(s))),
                DataType::Timestamp => {
                    // Prefer RFC3339 string, fallback to numeric-like string treated as epoch ms.
                    chrono::DateTime::parse_from_rfc3339(s.trim())
                        .ok()
                        .map(|dt| NGValue::Timestamp(dt.timestamp_millis()))
                        .or_else(|| {
                            s.trim().parse::<f64>().ok().and_then(|n| {
                                ValueCodec::coerce_f64_to_value(n, DataType::Timestamp, scale)
                            })
                        })
                }
                DataType::Boolean => {
                    // Reuse SDK cast policy (same accepted tokens as `TryFrom<&NGValue> for bool`).
                    let ng = NGValue::String(Arc::<str>::from(s));
                    bool::try_from(&ng).ok().map(NGValue::Boolean)
                }
                // Numeric expected: parse as f64 then coerce with scale/rounding policy.
                _ => s
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .and_then(|n| ValueCodec::coerce_f64_to_value(n, expected, scale)),
            },
            _ => {
                return Err(DriverError::CodecError(format!(
                    "Unsupported PlcValue type for EtherNet/IP typed conversion: {:?}",
                    value
                )));
            }
        };

        coerced.ok_or_else(|| {
            DriverError::CodecError(format!(
                "EtherNet/IP typed conversion failed: expected={expected:?}, scale={scale:?}"
            ))
        })
    }

    /// Convert an `NGValue` into a CIP `PlcValue` according to the declared logical `DataType`.
    ///
    /// # Best practice
    /// This is the recommended API for downlink conversions (write paths). It performs
    /// cast + range checks via SDK `TryFrom<&NGValue> for T` implementations.
    ///
    /// # Timestamp mapping
    /// `DataType::Timestamp` (epoch ms) is encoded as PLC `LINT` (`PlcValue::Lint`).
    ///
    /// # Limitations
    /// `DataType::Binary` is not supported in current driver version.
    pub fn to_plc_value(value: &NGValue, expected: DataType) -> DriverResult<PlcValue> {
        let cast_err = |e: NGValueCastError| {
            DriverError::ValidationError(format!(
                "EtherNet/IP write value cast failed: expected={expected:?}, actual={:?}, error={e}",
                value.data_type()
            ))
        };

        match expected {
            DataType::Boolean => Ok(PlcValue::Bool(bool::try_from(value).map_err(cast_err)?)),
            DataType::Int8 => Ok(PlcValue::Sint(i8::try_from(value).map_err(cast_err)?)),
            DataType::UInt8 => Ok(PlcValue::Usint(u8::try_from(value).map_err(cast_err)?)),
            DataType::Int16 => Ok(PlcValue::Int(i16::try_from(value).map_err(cast_err)?)),
            DataType::UInt16 => Ok(PlcValue::Uint(u16::try_from(value).map_err(cast_err)?)),
            DataType::Int32 => Ok(PlcValue::Dint(i32::try_from(value).map_err(cast_err)?)),
            DataType::UInt32 => Ok(PlcValue::Udint(u32::try_from(value).map_err(cast_err)?)),
            DataType::Int64 => Ok(PlcValue::Lint(i64::try_from(value).map_err(cast_err)?)),
            DataType::UInt64 => Ok(PlcValue::Ulint(u64::try_from(value).map_err(cast_err)?)),
            DataType::Float32 => Ok(PlcValue::Real(f32::try_from(value).map_err(cast_err)?)),
            DataType::Float64 => Ok(PlcValue::Lreal(f64::try_from(value).map_err(cast_err)?)),
            DataType::String => Ok(PlcValue::String(String::try_from(value).map_err(cast_err)?)),
            DataType::Timestamp => {
                let ms: i64 = i64::try_from(value).map_err(cast_err)?;
                Ok(PlcValue::Lint(ms))
            }
            DataType::Binary => Err(DriverError::ConfigurationError(
                "EtherNet/IP does not support DataType::Binary in current driver version".into(),
            )),
        }
    }
}
