//! DI Response Parser
//!
//! This module provides parsing functionality for CJ/T 188 DI (Data Identifier) responses.
//!
//! # Overview
//!
//! The DIResponseParser interprets raw protocol bytes according to DIResponseSchema definitions.
//! It handles multiple data formats (BCD, Binary, DateTime, Status) and produces typed values.
//!
//! # Key Features
//!
//! - Schema-driven parsing: Automatically selects the correct schema based on DI and meter type
//! - Type-safe value extraction: Returns NGValue for unified value handling
//! - Unit code validation: Validates expected units when present
//! - Error handling: Provides detailed error messages for debugging

use super::di_schema::get_di_schema;
use super::di_types::{DataFormat, ResponseField};
use crate::protocol::{
    codec::{decode_bcd_integer, decode_bcd_to_f64, decode_binary, decode_datetime},
    error::ProtocolError,
    frame::defs::{MeterType, Unit},
};
use crate::types::Cjt188Point;
use ng_gateway_sdk::{NGValue, ValueCodec};
use std::collections::HashMap;

/// Parsed field values with metadata.
///
/// A CJ/T 188 DI response field can be **fan-out** into multiple gateway Points:
/// - Multiple points may reference the same `field_key`.
/// - Each point may have its own `data_type` and `scale`.
///
/// Therefore, a parsed field is represented as a `point_id -> NGValue` map, plus optional
/// `unit_code` extracted from the wire payload.
#[derive(Debug, Clone)]
pub struct ParsedDIValue {
    /// Final coerced values keyed by point id.
    pub values: HashMap<i32, NGValue>,
    /// Unit code extracted from the field (if present)
    pub unit_code: Option<Unit>,
}

/// Parsed DI response for a point group.
///
/// This type is optimized for the driver read loop:
/// - It returns **final point values** (already coerced by each point's `data_type + scale`)
/// - It keeps raw bytes for debugging
#[derive(Debug, Clone)]
pub struct ParsedDIResponse {
    /// Data Identifier (DI) of this response.
    pub di: u16,
    /// Final point values keyed by point id.
    pub point_values: HashMap<i32, NGValue>,
}

impl ParsedDIResponse {
    /// Get number of parsed point values.
    #[inline]
    pub fn value_count(&self) -> usize {
        self.point_values.len()
    }

    /// Get a value by point id.
    #[inline]
    pub fn get_point_value(&self, point_id: i32) -> Option<&NGValue> {
        self.point_values.get(&point_id)
    }
}

/// DI response parser
///
/// Provides stateless parsing functions to convert raw protocol bytes into structured point values.
/// All methods are associated functions (no instance required) since the parser maintains no state.
#[derive(Debug, Clone, Copy)]
pub struct DIResponseParser;

impl DIResponseParser {
    /// Parse raw response data according to DI schema
    ///
    /// # Arguments
    /// * `di` - Data Identifier code (e.g., 0x901F)
    /// * `meter_type` - Meter type value (determines schema selection)
    /// * `data` - Raw response bytes from the protocol
    ///
    /// # Returns
    /// Parsed point values for the given DI group, or error if parsing fails
    ///
    /// # Errors
    /// - Schema not found for the DI/meter type combination
    /// - Data length mismatch
    /// - Invalid BCD encoding
    /// - Invalid datetime format
    /// - Unit code mismatch (warning in logs, but not fatal)
    pub fn parse(
        di: u16,
        meter_type: MeterType,
        data: &[u8],
        points: &[&Cjt188Point],
    ) -> Result<ParsedDIResponse, ProtocolError> {
        // Get the schema for this DI and meter type
        let schema = get_di_schema(di, meter_type).ok_or_else(|| {
            ProtocolError::Semantic(format!(
                "No schema found for DI 0x{:04X} with meter type 0x{:02X} ({})",
                di,
                meter_type.code(),
                meter_type.family().as_str()
            ))
        })?;

        // Build an index: field_key -> points interested in this field.
        //
        // This avoids scanning `points` for each schema field and enables
        // "decode once, coerce N times" for fan-out fields.
        let mut points_by_field_key: HashMap<&str, Vec<&Cjt188Point>> =
            HashMap::with_capacity(points.len().max(1));
        for point in points {
            points_by_field_key
                .entry(point.field_key.as_str())
                .or_default()
                .push(*point);
        }

        // Validate data length
        let expected_len = schema.total_length();
        if data.len() < expected_len {
            return Err(ProtocolError::FrameTooShort(format!(
                "Expected {} bytes for DI 0x{:04X}, got {} bytes",
                expected_len,
                di,
                data.len()
            )));
        }

        // Parse fields sequentially
        let mut point_values: HashMap<i32, NGValue> = HashMap::with_capacity(points.len());
        let mut offset = 0;

        for field in &schema.fields {
            let field_data = &data[offset..offset + field.length];
            offset += field.length;

            // Only decode fields that are referenced by at least one point.
            // This is a hot-path optimization: many DIs have large schemas but a device
            // might only be configured with a small subset of points.
            let Some(points_for_field) = points_by_field_key.get(field.key.as_str()) else {
                continue;
            };

            let parsed = Self::parse_field(field, field_data, points_for_field)?;
            point_values.extend(parsed.values);
        }

        Ok(ParsedDIResponse { di, point_values })
    }

    /// Parse a single schema field and **fan out** into multiple point values.
    ///
    /// # Key idea
    /// Decode the wire representation **once**, then coerce the decoded scalar into each
    /// point's configured `data_type + scale` using `ng_gateway_sdk::ValueCodec`.
    fn parse_field(
        field: &ResponseField,
        data: &[u8],
        points: &[&Cjt188Point],
    ) -> Result<ParsedDIValue, ProtocolError> {
        #[derive(Debug, Clone, Copy)]
        enum DecodedScalar {
            F64(f64),
            U64(u64),
            I64(i64),
        }

        let (decoded, unit_code) = match &field.format {
            DataFormat::BCD { decimals } => {
                // BCD with decimal places (float-ish numeric source).
                let raw = decode_bcd_to_f64(data, *decimals, false)?;
                (DecodedScalar::F64(raw), None)
            }
            DataFormat::BCDInteger => {
                // BCD integer (precise u64 source).
                let raw = decode_bcd_integer(data)?;
                (DecodedScalar::U64(raw), None)
            }
            DataFormat::Binary => {
                // Binary unsigned integer (precise u64 source).
                let raw = decode_binary(data)?;
                (DecodedScalar::U64(raw), None)
            }
            DataFormat::DateTime => {
                // Date-time (7 bytes BCD) -> epoch-ms (UTC) as i64 source.
                //
                // CJ/T 188 datetime has no timezone; the gateway treats it as UTC to match
                // `NGValue::Timestamp` semantics (unix ms).
                let datetime = decode_datetime(data)?;
                let ts_ms = ValueCodec::datetime_to_epoch_ms(datetime);
                (DecodedScalar::I64(ts_ms), None)
            }
            DataFormat::Status => {
                // Status bytes (2 bytes) as u16 for bit flag analysis (precise u64 source).
                if data.len() != 2 {
                    return Err(ProtocolError::Semantic(format!(
                        "Status field must be 2 bytes, got {}",
                        data.len()
                    )));
                }
                let status = u16::from_le_bytes([data[0], data[1]]) as u64;
                (DecodedScalar::U64(status), None)
            }
            DataFormat::BCDWithUnit {
                data_length,
                decimals,
            } => {
                // BCD + unit code byte.
                if data.len() != data_length + 1 {
                    return Err(ProtocolError::Semantic(format!(
                        "BCDWithUnit field expected {} bytes ({} data + 1 unit), got {}",
                        data_length + 1,
                        data_length,
                        data.len()
                    )));
                }
                let bcd_data = &data[..*data_length];
                let raw = decode_bcd_to_f64(bcd_data, *decimals, false)?;
                let unit_byte = data[*data_length];
                let unit_code = Unit::from_code(unit_byte);
                (DecodedScalar::F64(raw), Some(unit_code))
            }
        };

        let mut values: HashMap<i32, NGValue> = HashMap::with_capacity(points.len().max(1));

        for point in points {
            let coerced = match decoded {
                DecodedScalar::F64(v) => {
                    ValueCodec::coerce_f64_to_value(v, point.data_type, point.scale)
                }
                DecodedScalar::U64(v) => {
                    ValueCodec::coerce_u64_to_value(v, point.data_type, point.scale)
                }
                DecodedScalar::I64(v) => {
                    ValueCodec::coerce_i64_to_value(v, point.data_type, point.scale)
                }
            };

            match coerced {
                Some(v) => {
                    values.insert(point.id, v);
                }
                None => {
                    // Coercion failures are usually configuration errors (datatype/scale mismatch).
                    // We keep parsing other points/fields to maximize partial success.
                    tracing::warn!(
                        point_id = point.id,
                        point_key = %point.key,
                        field_key = %point.field_key,
                        expected = ?point.data_type,
                        scale = ?point.scale,
                        di = format!("0x{:04X}", point.di),
                        "Failed to coerce decoded field value into point data_type"
                    );
                }
            }
        }

        Ok(ParsedDIValue { values, unit_code })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_water_meter_901f() {
        let meter_type = MeterType::COLD_WATER;

        // Mock data payload (DI+SER already extracted by protocol layer)
        // Payload: current(5) + settlement(5) + datetime(7) + status(2) = 19 bytes
        //
        // BCD encoding (little-endian): lowest digits first
        // Example: 1234.00 with decimals=2 → raw integer 123400
        //   In little-endian BCD: [0x00, 0x34, 0x12, 0x00]
        let data = vec![
            0x00, 0x34, 0x12, 0x00, 0x2C, // Current: 1234.00 m³ (4 BCD + unit 0x2C)
            0x00, 0x00, 0x10, 0x00, 0x2C, // Settlement: 1000.00 m³ (4 BCD + unit 0x2C)
            0x00, 0x30, 0x14, 0x08, 0x01, 0x26,
            0x20, // DateTime: 2026-01-08 14:30:00 (SS mm hh DD MM YY CC)
            0x00, 0x00, // Status: OK (2 bytes)
        ];

        // Minimal mock points: each point references one field_key.
        // We set point key == field_key for convenience in this unit test.
        let p_current = Cjt188Point {
            id: 1,
            device_id: 1,
            name: "current_flow".to_string(),
            key: "current_flow".to_string(),
            r#type: ng_gateway_sdk::DataPointType::Telemetry,
            data_type: ng_gateway_sdk::DataType::Float64,
            access_mode: ng_gateway_sdk::AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            di: 0x901F,
            field_key: "current_flow".to_string(),
        };
        let p_datetime = Cjt188Point {
            id: 2,
            device_id: 1,
            name: "datetime".to_string(),
            key: "datetime".to_string(),
            r#type: ng_gateway_sdk::DataPointType::Telemetry,
            data_type: ng_gateway_sdk::DataType::Timestamp,
            access_mode: ng_gateway_sdk::AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            di: 0x901F,
            field_key: "datetime".to_string(),
        };
        let p_status = Cjt188Point {
            id: 3,
            device_id: 1,
            name: "status".to_string(),
            key: "status".to_string(),
            r#type: ng_gateway_sdk::DataPointType::Telemetry,
            data_type: ng_gateway_sdk::DataType::Int64,
            access_mode: ng_gateway_sdk::AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            di: 0x901F,
            field_key: "status".to_string(),
        };

        let result = DIResponseParser::parse(
            0x901F,
            meter_type,
            &data,
            &[&p_current, &p_datetime, &p_status],
        )
        .unwrap();

        assert_eq!(result.di, 0x901F);
        assert_eq!(result.value_count(), 3); // current_flow, datetime, status

        // Check current_flow point value
        let current_flow = result.get_point_value(1).unwrap();
        match current_flow {
            NGValue::Float64(v) => {
                assert!((*v - 1234.00).abs() < 0.01, "Expected 1234.00, got {}", v)
            }
            _ => panic!("Expected Float64 value, got {:?}", current_flow),
        }

        // Check datetime point value
        let datetime = result.get_point_value(2).unwrap();
        match datetime {
            NGValue::Timestamp(ms) => {
                let expected_ndt = chrono::NaiveDate::from_ymd_opt(2026, 1, 8)
                    .unwrap()
                    .and_hms_opt(14, 30, 0)
                    .unwrap();
                let expected_ms = ValueCodec::datetime_to_epoch_ms(expected_ndt);
                assert_eq!(*ms, expected_ms);
            }
            _ => panic!("Expected Timestamp value"),
        }

        // Check status point value
        let status = result.get_point_value(3).unwrap();
        assert_eq!(*status, NGValue::Int64(0));
    }

    #[test]
    fn test_parse_common_907f() {
        let meter_type = MeterType::COLD_WATER;

        // Payload (DI+SER already extracted): datetime(7) = 7 bytes
        let data = vec![
            0x00, 0x30, 0x14, 0x08, 0x01, 0x26,
            0x20, // DateTime: 2026-01-08 14:30:00 (SS mm hh DD MM YY CC)
        ];

        let p_datetime = Cjt188Point {
            id: 1,
            device_id: 1,
            name: "datetime".to_string(),
            key: "datetime".to_string(),
            r#type: ng_gateway_sdk::DataPointType::Telemetry,
            data_type: ng_gateway_sdk::DataType::Timestamp,
            access_mode: ng_gateway_sdk::AccessMode::Read,
            unit: None,
            min_value: None,
            max_value: None,
            scale: None,
            di: 0x907F,
            field_key: "datetime".to_string(),
        };

        let result = DIResponseParser::parse(0x907F, meter_type, &data, &[&p_datetime]).unwrap();

        assert_eq!(result.di, 0x907F);
        assert_eq!(result.value_count(), 1); // Only datetime point

        let datetime = result.get_point_value(1).unwrap();
        match datetime {
            NGValue::Timestamp(ms) => {
                let expected_ndt = chrono::NaiveDate::from_ymd_opt(2026, 1, 8)
                    .unwrap()
                    .and_hms_opt(14, 30, 0)
                    .unwrap();
                let expected_ms = ValueCodec::datetime_to_epoch_ms(expected_ndt);
                assert_eq!(*ms, expected_ms);
            }
            _ => panic!("Expected Timestamp value"),
        }
    }

    #[test]
    fn test_parse_data_too_short() {
        let meter_type = MeterType::COLD_WATER;

        // Only 10 bytes, but schema expects 22 bytes
        let data = vec![0x1F, 0x90, 0x01, 0x34, 0x12, 0x00, 0x00, 0x2C, 0x00, 0x10];

        let result = DIResponseParser::parse(0x901F, meter_type, &data, &[]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Expected 19 bytes"));
    }

    #[test]
    fn test_parse_schema_not_found() {
        let meter_type = MeterType::COLD_WATER;

        let data = vec![0xFF; 10];
        let result = DIResponseParser::parse(0xFFFF, meter_type, &data, &[]);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No schema found"));
    }
}
