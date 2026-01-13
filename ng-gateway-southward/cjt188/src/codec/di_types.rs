//! DI Response Schema Types
//!
//! This module defines the structure of DI (Data Identifier) responses in CJ/T 188 protocol.
//!
//! # Overview
//!
//! In CJ/T 188, a DI code represents a **data group** rather than a single data point.
//! Each DI defines a specific response structure containing multiple fields.
//! The exact structure may vary based on the meter type family.
//!
//! # Key Concepts
//!
//! - **DIResponseSchema**: Defines the structure of data returned for a specific DI
//! - **ResponseField**: Describes a single field within a DI response
//! - **DataFormat**: Specifies how field data is encoded in the protocol wire
//! - **DIResponse**: Parsed result containing all field values

use crate::protocol::frame::defs::{MeterType, MeterTypeFamily};
use ng_gateway_sdk::NGValue;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Data format types in CJ/T 188 protocol
///
/// Reference: 数据表达格式表.xlsx
///
/// This enum defines how data values are encoded in the protocol wire format.
/// Each format type has specific decoding rules and constraints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataFormat {
    /// BCD encoded numeric value with decimal points
    ///
    /// Format: xxxxxx.xx (configurable decimal places)
    /// Length: 2-6 bytes
    ///
    /// # Example
    /// ```text
    /// Bytes: 0x34 0x12 0x00 0x00 with decimals=2
    /// Result: 1234.00
    /// ```
    BCD {
        /// Number of decimal places (0-6)
        decimals: u8,
    },

    /// BCD encoded numeric value without decimal points (integer)
    ///
    /// Format: xxxxxx
    /// Length: 2-6 bytes
    ///
    /// # Example
    /// ```text
    /// Bytes: 0x34 0x12
    /// Result: 1234
    /// ```
    BCDInteger,

    /// Binary encoded unsigned integer
    ///
    /// Length: 1, 2, or 4 bytes
    /// Encoding: Little-endian
    ///
    /// # Example
    /// ```text
    /// Bytes: 0x1F 0x90
    /// Result: 0x901F (36895)
    /// ```
    Binary,

    /// Date-time value
    ///
    /// Format: YYYY MM DD hh mm ss (7 bytes BCD)
    /// - YYYY: 4-digit year (2 bytes BCD, e.g., 0x20 0x26 -> 2026)
    /// - MM: month (1 byte BCD, 01-12)
    /// - DD: day (1 byte BCD, 01-31)
    /// - hh: hour (1 byte BCD, 00-23)
    /// - mm: minute (1 byte BCD, 00-59)
    /// - ss: second (1 byte BCD, 00-59)
    ///
    /// # Example
    /// ```text
    /// Bytes: 0x20 0x26 0x01 0x08 0x14 0x30 0x00
    /// Result: 2026-01-08 14:30:00
    /// ```
    DateTime,

    /// Status bytes with bit flags
    ///
    /// Length: 2 bytes
    ///
    /// # Byte 0 (D0-D7):
    /// - D0: 阀门开关 (Valve state: 0=open, 1=closed)
    /// - D1: 阀门状态 (Valve status: 0=normal, 1=abnormal)
    /// - D2: 电池电压 (Battery voltage: 0=normal, 1=low)
    /// - D3-D7: 厂商定义 (Vendor-defined)
    ///
    /// # Byte 1 (D8-D15):
    /// - D8-D15: 厂商定义 (Vendor-defined)
    Status,

    /// BCD encoded value with unit code
    ///
    /// Format: [BCD data (n bytes)] + [Unit code (1 byte)]
    /// The unit code byte follows the BCD data.
    ///
    /// # Example
    /// ```text
    /// Bytes: 0x34 0x12 0x00 0x00 0x0A
    /// BCD data: 1234.00
    /// Unit: 0x0A (m³)
    /// ```
    BCDWithUnit {
        /// Number of BCD data bytes (before unit code byte)
        data_length: usize,
        /// Number of decimal places in BCD data
        decimals: u8,
    },
}

impl DataFormat {
    /// Get the default total length in bytes for this format
    ///
    /// Note: Some formats have variable length - this returns a typical default.
    /// The actual length should be specified in the field schema.
    pub fn default_length(&self) -> usize {
        match self {
            DataFormat::BCD { .. } => 4,
            DataFormat::BCDInteger => 4,
            DataFormat::Binary => 2,
            DataFormat::DateTime => 7, // YYYY(2) + MM(1) + DD(1) + hh(1) + mm(1) + ss(1)
            DataFormat::Status => 2,
            DataFormat::BCDWithUnit { data_length, .. } => data_length + 1, // +1 for unit code
        }
    }
}

/// Single field definition in a DI response
///
/// Represents one data field within a DI response structure.
/// Fields are defined in the order they appear in the protocol wire format.
///
/// # Field Extraction
///
/// Each field has a unique `key` that is used by Points to extract specific values
/// from a parsed DI response. For example:
///
/// ```text
/// DI 0x901F for water meter contains:
/// - "di": Data Identifier
/// - "ser": Serial number
/// - "current_flow": Current cumulative flow
/// - "settlement_flow": Settlement date flow
/// - "datetime": Real-time timestamp
/// - "status": Status bytes
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseField {
    /// Field identifier/key (e.g., "current_flow", "settlement_flow")
    ///
    /// This key is used by Points to extract specific values from the DI response.
    /// Must be unique within a DI response schema.
    pub key: String,

    /// Human-readable field name (Chinese)
    ///
    /// Used for logging, error messages, and documentation
    pub name: String,

    /// Data format in protocol wire
    ///
    /// Defines how to decode the raw bytes for this field
    pub format: DataFormat,

    /// Field length in bytes (total length including unit code if present)
    ///
    /// This is the number of bytes to consume from the wire format.
    /// For BCDWithUnit, this includes both data bytes and the unit code byte.
    pub length: usize,

    /// Whether this field includes a unit code byte
    ///
    /// If true, the last byte of the field is the unit code.
    /// The unit code will be parsed but not validated.
    pub has_unit_code: bool,
}

/// DI Response Schema - defines the structure of data returned for a specific DI
///
/// A schema describes how to parse the raw bytes returned by a DI read operation.
/// Different meter types may have different schemas for the same DI.
///
/// # Schema Selection
///
/// The parser selects the appropriate schema based on:
/// 1. DI code (e.g., 0x901F)
/// 2. Meter type family (Water, Heat, Gas, Custom)
///
/// # Example
///
/// ```text
/// DI 0x901F for Water Meter:
/// - Field 1: di (2 bytes Binary)
/// - Field 2: ser (1 byte BCD)
/// - Field 3: current_flow (5 bytes BCDWithUnit)
/// - Field 4: settlement_flow (5 bytes BCDWithUnit)
/// - Field 5: datetime (7 bytes DateTime)
/// - Field 6: status (2 bytes Status)
/// Total: 22 bytes
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DIResponseSchema {
    /// Data Identifier (DI code)
    ///
    /// The 16-bit DI value (e.g., 0x901F) that this schema applies to
    pub di: u16,

    /// Applicable meter types for this schema
    ///
    /// - None: applies to all meter types
    /// - Some([...]): applies only to specified meter type families
    ///
    /// This allows different parsing rules for the same DI across meter types.
    ///
    /// # Example
    /// ```text
    /// DI 0x901F has different structures for:
    /// - Water/Gas meters: 22 bytes (6 fields)
    /// - Heat meters: 43 bytes (12 fields)
    /// ```
    pub applicable_meter_types: Option<Vec<MeterTypeFamily>>,

    /// Ordered list of fields in the response
    ///
    /// Fields must be in the exact order they appear in the protocol wire.
    /// The parser will decode fields sequentially from the raw byte stream.
    pub fields: Vec<ResponseField>,
}

impl DIResponseSchema {
    /// Calculate the expected total length of the response in bytes
    ///
    /// # Returns
    /// The sum of all field lengths in this schema
    pub fn total_length(&self) -> usize {
        self.fields.iter().map(|f| f.length).sum()
    }

    /// Check if this schema applies to a given meter type
    ///
    /// # Arguments
    /// * `meter_type` - The meter type to check
    ///
    /// # Returns
    /// `true` if this schema applies to the given meter type
    pub fn applies_to(&self, meter_type: MeterType) -> bool {
        match &self.applicable_meter_types {
            None => true, // applies to all meter types
            Some(families) => families.contains(&meter_type.family()),
        }
    }

    /// Find a field by key
    ///
    /// # Arguments
    /// * `key` - The field key to search for
    ///
    /// # Returns
    /// Reference to the field if found, None otherwise
    pub fn find_field(&self, key: &str) -> Option<&ResponseField> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// Get all available field keys in this schema
    ///
    /// Useful for error messages when a requested field is not found.
    ///
    /// # Returns
    /// Vector of all field keys
    pub fn field_keys(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.key.as_str()).collect()
    }
}

/// Parsed DI response containing structured field values
///
/// This is the result of parsing raw protocol bytes according to a DIResponseSchema.
/// Each field value is stored in an NGValue for type-safe access.
#[derive(Debug, Clone)]
pub struct DIResponse {
    /// Data Identifier
    pub di: u16,

    /// Field values mapped by field key
    ///
    /// Keys correspond to the `key` field in ResponseField definitions.
    /// Values are parsed and typed according to the field's DataFormat.
    pub fields: HashMap<String, NGValue>,

    /// Raw data bytes (for debugging and logging)
    ///
    /// Contains the original protocol bytes that were parsed.
    /// Useful for troubleshooting parsing issues.
    pub raw_data: Vec<u8>,
}

impl DIResponse {
    /// Get a specific field value by key
    ///
    /// # Arguments
    /// * `key` - The field key to retrieve
    ///
    /// # Returns
    /// Reference to the NGValue if found, None otherwise
    pub fn get_field(&self, key: &str) -> Option<&NGValue> {
        self.fields.get(key)
    }

    /// Extract a specific field, returning error if not found
    ///
    /// # Arguments
    /// * `key` - The field key to extract
    ///
    /// # Returns
    /// Cloned NGValue on success, error message on failure
    ///
    /// # Errors
    /// Returns error string if the field key does not exist in the response
    pub fn extract_field(&self, key: &str) -> Result<NGValue, String> {
        self.fields.get(key).cloned().ok_or_else(|| {
            format!(
                "Field '{}' not found in DI 0x{:04X} response. Available fields: {:?}",
                key,
                self.di,
                self.fields.keys().collect::<Vec<_>>()
            )
        })
    }

    /// Get all field keys in this response
    ///
    /// # Returns
    /// Vector of all field keys
    pub fn keys(&self) -> Vec<&String> {
        self.fields.keys().collect()
    }

    /// Get number of fields in this response
    ///
    /// # Returns
    /// Number of parsed fields
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_format_default_length() {
        assert_eq!(DataFormat::BCD { decimals: 2 }.default_length(), 4);
        assert_eq!(DataFormat::BCDInteger.default_length(), 4);
        assert_eq!(DataFormat::Binary.default_length(), 2);
        assert_eq!(DataFormat::DateTime.default_length(), 7);
        assert_eq!(DataFormat::Status.default_length(), 2);
        assert_eq!(
            DataFormat::BCDWithUnit {
                data_length: 4,
                decimals: 2
            }
            .default_length(),
            5
        );
    }

    #[test]
    fn test_schema_total_length() {
        let schema = DIResponseSchema {
            di: 0x901F,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water]),
            fields: vec![
                ResponseField {
                    key: "di".to_string(),
                    name: "数据标识".to_string(),
                    format: DataFormat::Binary,
                    length: 2,
                    has_unit_code: false,
                },
                ResponseField {
                    key: "current_flow".to_string(),
                    name: "当前累积流量".to_string(),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
            ],
        };

        assert_eq!(schema.total_length(), 7);
    }

    #[test]
    fn test_schema_applies_to() {
        let schema = DIResponseSchema {
            di: 0x901F,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
            fields: vec![],
        };

        assert!(schema.applies_to(MeterType::COLD_WATER));
        assert!(schema.applies_to(MeterType::GAS));
        assert!(!schema.applies_to(MeterType::HEAT));
    }
}
