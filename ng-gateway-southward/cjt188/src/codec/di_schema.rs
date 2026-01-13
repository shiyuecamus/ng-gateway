//! DI Response Schema Registry
//!
//! This module provides a global registry of built-in DI (Data Identifier) response schemas.
//!
//! # Overview
//!
//! The schema registry maps (DI code, MeterTypeFamily) pairs to their corresponding
//! DIResponseSchema definitions. This allows the parser to correctly interpret raw
//! protocol bytes based on the meter type.
//!
//! # Schema Selection
//!
//! When parsing a DI response:
//! 1. Look up schema by (DI, MeterTypeFamily)
//! 2. If not found, try to find a universal schema (applicable_meter_types = None)
//! 3. If still not found, return error
//!
//! # Built-in Schemas
//!
//! Currently supports:
//! - Water/Gas meters: 0x901F, 0x903F, 0x904F, 0x907F
//! - Heat meters: 0x901F (heat-specific), 0x907F
//! - Common: 0x907F (real-time clock, applies to all meter types)

use super::di_types::{DIResponseSchema, DataFormat, ResponseField};
use crate::protocol::frame::defs::{MeterType, MeterTypeFamily};
use std::collections::HashMap;
use std::sync::LazyLock;

/// Key for DI schema lookup
///
/// Combines DI code and meter type family to uniquely identify a schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DISchemaKey {
    /// Data Identifier code (e.g., 0x901F)
    pub di: u16,
    /// Meter type family (Water, Heat, Gas, Custom)
    pub meter_family: MeterTypeFamily,
}

/// Global registry of built-in DI response schemas
///
/// This is initialized lazily on first access and remains immutable thereafter.
/// All schemas are registered at compile time through the initialization function.
pub static DI_SCHEMA_REGISTRY: LazyLock<HashMap<DISchemaKey, DIResponseSchema>> =
    LazyLock::new(|| {
        let mut registry = HashMap::new();

        // Register all built-in schemas
        register_water_gas_schemas(&mut registry);
        register_heat_schemas(&mut registry);
        register_common_schemas(&mut registry);

        registry
    });

/// Get the appropriate schema for a DI and meter type
///
/// # Arguments
/// * `di` - Data Identifier code
/// * `meter_type` - Meter type value
///
/// # Returns
/// Reference to the matching DIResponseSchema, or None if not found
///
/// # Lookup Strategy
/// 1. Try exact match: (di, meter_family)
/// 2. Fallback: schemas with applicable_meter_types = None (universal)
///
/// # Examples
/// ```ignore
/// use ng_driver_cjt188::protocol::frame::defs::MeterType;
/// use ng_driver_cjt188::codec::di_schema::get_di_schema;
///
/// let schema = get_di_schema(0x901F, MeterType::COLD_WATER);
/// assert!(schema.is_some());
/// ```
pub fn get_di_schema(di: u16, meter_type: MeterType) -> Option<&'static DIResponseSchema> {
    let family = meter_type.family();
    let key = DISchemaKey {
        di,
        meter_family: family,
    };

    // Try exact match first
    DI_SCHEMA_REGISTRY.get(&key).or_else(|| {
        // Fallback: find a universal schema (applies to all meter types)
        DI_SCHEMA_REGISTRY
            .iter()
            .find(|(k, schema)| k.di == di && schema.applicable_meter_types.is_none())
            .map(|(_, schema)| schema)
    })
}

/// Register water/gas meter schemas (0x10-0x19, 0x30-0x39)
///
/// These meter types share similar data structures for most DIs.
///
/// # Registered Schemas
/// - 0x901F: Full meter reading (22 bytes)
/// - 0xD120-0xD12B: Historical data 1 (12 schemas, 8 bytes each)
/// - 0xD200-0xD2FF: Historical data 2 (256 schemas, 8 bytes each)
/// - 0xD300-0xD3FF: Timed freeze data (256 schemas, 26 bytes each)
/// - 0xD400-0xD4FF: Instant freeze data (256 schemas, 26 bytes each)
fn register_water_gas_schemas(registry: &mut HashMap<DISchemaKey, DIResponseSchema>) {
    // DI: 0x901F - Full meter reading (water/gas)
    //
    // Structure (19 bytes data payload, DI+SER already extracted by protocol layer):
    // - Current Flow (5 bytes: 4 BCD + 1 unit): Current cumulative flow
    // - Settlement Flow (5 bytes: 4 BCD + 1 unit): Settlement date flow
    // - DateTime (7 bytes BCD): Real-time timestamp (YYYY MM DD hh mm ss)
    // - Status (2 bytes): Status bytes with bit flags
    //
    // Note: DI and SER are already extracted by protocol layer (ReadDataResponse struct)
    // and are NOT included in the data payload.
    let schema_901f = DIResponseSchema {
        di: 0x901F,
        applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
        fields: vec![
            ResponseField {
                key: "current_flow".to_string(),
                name: "当前累积流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5, // 4 bytes BCD + 1 byte unit code
                has_unit_code: true,
            },
            ResponseField {
                key: "settlement_flow".to_string(),
                name: "结算日累积流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5, // 4 bytes BCD + 1 byte unit code
                has_unit_code: true,
            },
            ResponseField {
                key: "datetime".to_string(),
                name: "实时时间".to_string(),
                format: DataFormat::DateTime,
                length: 7, // YYYY(2) + MM(1) + DD(1) + hh(1) + mm(1) + ss(1)
                has_unit_code: false,
            },
            ResponseField {
                key: "status".to_string(),
                name: "状态".to_string(),
                format: DataFormat::Status,
                length: 2, // 2 bytes (D0-D15)
                has_unit_code: false,
            },
        ],
    };

    // Register for both Water and Gas families
    registry.insert(
        DISchemaKey {
            di: 0x901F,
            meter_family: MeterTypeFamily::Water,
        },
        schema_901f.clone(),
    );
    registry.insert(
        DISchemaKey {
            di: 0x901F,
            meter_family: MeterTypeFamily::Gas,
        },
        schema_901f,
    );

    // DI: 0xD120-0xD12B - Historical data 1 (last 1-12 months settlement flow)
    // Structure (8 bytes): DI(2) + SER(1) + Settlement Flow(5)
    for x in 0x00..=0x0B {
        let di = 0xD120 + x;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
            fields: vec![ResponseField {
                key: "settlement_flow".to_string(),
                name: format!("上{}月结算日累积流量", x + 1),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            }],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Water,
            },
            schema.clone(),
        );
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Gas,
            },
            schema,
        );
    }

    // DI: 0xD200-0xD2FF - Historical data 2 (water/gas)
    // Structure (8 bytes): DI(2) + SER(1) + Settlement Flow(5)
    for xx in 0x00..=0xFF {
        let di = 0xD200 + xx;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
            fields: vec![ResponseField {
                key: "settlement_flow".to_string(),
                name: format!("上{}月结算日累积流量", xx + 1),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            }],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Water,
            },
            schema.clone(),
        );
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Gas,
            },
            schema,
        );
    }

    // DI: 0xD300-0xD3FF - Timed freeze data (water/gas)
    // Structure (26 bytes = 0x1A): DI(2) + SER(1) + Freeze DateTime(7) + Cumulative Flow(5) +
    //                               Flow Rate(5) + Temperature(3) + Pressure(3)
    for xx in 0x00..=0xFF {
        let di = 0xD300 + xx;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
            fields: vec![
                ResponseField {
                    key: "freeze_datetime".to_string(),
                    name: format!("上{}次定时冻结实时时间", xx + 1),
                    format: DataFormat::DateTime,
                    length: 7,
                    has_unit_code: false,
                },
                ResponseField {
                    key: "cumulative_flow".to_string(),
                    name: "累积流量".to_string(),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "flow_rate".to_string(),
                    name: "瞬时流量".to_string(),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 4,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "temperature".to_string(),
                    name: "温度".to_string(),
                    format: DataFormat::BCD { decimals: 2 },
                    length: 3,
                    has_unit_code: false,
                },
                ResponseField {
                    key: "pressure".to_string(),
                    name: "压力".to_string(),
                    format: DataFormat::BCD { decimals: 2 },
                    length: 3,
                    has_unit_code: false,
                },
            ],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Water,
            },
            schema.clone(),
        );
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Gas,
            },
            schema,
        );
    }

    // DI: 0xD400-0xD4FF - Instant freeze data (water/gas)
    // Structure (26 bytes = 0x1A): Same as D3XX, just different freeze trigger
    for xx in 0x00..=0xFF {
        let di = 0xD400 + xx;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Water, MeterTypeFamily::Gas]),
            fields: vec![
                ResponseField {
                    key: "freeze_datetime".to_string(),
                    name: format!("上{}次瞬时冻结实时时间", xx + 1),
                    format: DataFormat::DateTime,
                    length: 7,
                    has_unit_code: false,
                },
                ResponseField {
                    key: "cumulative_flow".to_string(),
                    name: "累积流量".to_string(),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "flow_rate".to_string(),
                    name: "瞬时流量".to_string(),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 4,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "temperature".to_string(),
                    name: "温度".to_string(),
                    format: DataFormat::BCD { decimals: 2 },
                    length: 3,
                    has_unit_code: false,
                },
                ResponseField {
                    key: "pressure".to_string(),
                    name: "压力".to_string(),
                    format: DataFormat::BCD { decimals: 2 },
                    length: 3,
                    has_unit_code: false,
                },
            ],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Water,
            },
            schema.clone(),
        );
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Gas,
            },
            schema,
        );
    }
}

/// Register heat meter schemas (0x20-0x29)
///
/// Heat meters have richer data structures compared to water/gas meters.
fn register_heat_schemas(registry: &mut HashMap<DISchemaKey, DIResponseSchema>) {
    // DI: 0x901F - Full meter reading (heat)
    //
    // Structure (43 bytes total):
    // - DI (2 bytes Binary)
    // - SER (1 byte BCD)
    // - Settlement Heat (5 bytes: 4 BCD + 1 unit)
    // - Current Heat (5 bytes: 4 BCD + 1 unit)
    // - Heat Power (4 bytes: 3 BCD + 1 unit)
    // - Flow Rate (4 bytes: 3 BCD + 1 unit)
    // - Cumulative Flow (5 bytes: 4 BCD + 1 unit)
    // - Supply Temperature (2 bytes BCD, no unit)
    // - Return Temperature (2 bytes BCD, no unit)
    // - Working Hours (4 bytes BCD, no unit)
    // - DateTime (7 bytes BCD)
    // - Status (2 bytes)
    let schema_901f_heat = DIResponseSchema {
        di: 0x901F,
        applicable_meter_types: Some(vec![MeterTypeFamily::Heat]),
        fields: vec![
            ResponseField {
                key: "settlement_heat".to_string(),
                name: "结算日热量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "current_heat".to_string(),
                name: "当前热量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "heat_power".to_string(),
                name: "热功率".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 3,
                    decimals: 2,
                },
                length: 4,
                has_unit_code: true,
            },
            ResponseField {
                key: "flow_rate".to_string(),
                name: "流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 3,
                    decimals: 3,
                },
                length: 4,
                has_unit_code: true,
            },
            ResponseField {
                key: "cumulative_flow".to_string(),
                name: "累积流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "supply_temp".to_string(),
                name: "供水温度".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "return_temp".to_string(),
                name: "回水温度".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "working_hours".to_string(),
                name: "累积工作时间".to_string(),
                format: DataFormat::BCDInteger,
                length: 4,
                has_unit_code: false,
            },
            ResponseField {
                key: "datetime".to_string(),
                name: "实时时间".to_string(),
                format: DataFormat::DateTime,
                length: 7,
                has_unit_code: false,
            },
            ResponseField {
                key: "status".to_string(),
                name: "状态".to_string(),
                format: DataFormat::Status,
                length: 2,
                has_unit_code: false,
            },
        ],
    };

    registry.insert(
        DISchemaKey {
            di: 0x901F,
            meter_family: MeterTypeFamily::Heat,
        },
        schema_901f_heat,
    );

    // DI: 0x911F - Full meter reading 2 (heat+cooling)
    // Structure (62 bytes = 0x3E): DI(2) + SER(1) + Settlement Heat(5) + Settlement Cooling(5) +
    // Current Heat(5) + Current Cooling(5) + Heat Power(4) + Flow Rate(4) + Cumulative Flow(5) +
    // Supply Temp(2) + Return Temp(2) + Supply Pressure(2) + Return Pressure(2) + Working Hours(4) + DateTime(7) + Status(2)
    let schema_911f_heat = DIResponseSchema {
        di: 0x911F,
        applicable_meter_types: Some(vec![MeterTypeFamily::Heat]),
        fields: vec![
            ResponseField {
                key: "settlement_heat".to_string(),
                name: "结算日热量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "settlement_cooling".to_string(),
                name: "结算日冷量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "current_heat".to_string(),
                name: "当前热量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "current_cooling".to_string(),
                name: "当前冷量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "heat_power".to_string(),
                name: "热功率".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 3,
                    decimals: 2,
                },
                length: 4,
                has_unit_code: true,
            },
            ResponseField {
                key: "flow_rate".to_string(),
                name: "瞬时流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 3,
                    decimals: 4,
                },
                length: 4,
                has_unit_code: true,
            },
            ResponseField {
                key: "cumulative_flow".to_string(),
                name: "累积流量".to_string(),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            },
            ResponseField {
                key: "supply_temp".to_string(),
                name: "供水温度".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "return_temp".to_string(),
                name: "回水温度".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "supply_pressure".to_string(),
                name: "供水压力".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "return_pressure".to_string(),
                name: "回水压力".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 2,
                has_unit_code: false,
            },
            ResponseField {
                key: "working_hours".to_string(),
                name: "累积工作时间".to_string(),
                format: DataFormat::BCDInteger,
                length: 4,
                has_unit_code: false,
            },
            ResponseField {
                key: "datetime".to_string(),
                name: "实时时间".to_string(),
                format: DataFormat::DateTime,
                length: 7,
                has_unit_code: false,
            },
            ResponseField {
                key: "status".to_string(),
                name: "状态".to_string(),
                format: DataFormat::Status,
                length: 2,
                has_unit_code: false,
            },
        ],
    };

    registry.insert(
        DISchemaKey {
            di: 0x911F,
            meter_family: MeterTypeFamily::Heat,
        },
        schema_911f_heat,
    );

    // DI: 0xD120-0xD12B - Historical data 1 (heat - last 1-12 months settlement heat)
    for x in 0x00..=0x0B {
        let di = 0xD120 + x;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Heat]),
            fields: vec![ResponseField {
                key: "settlement_heat".to_string(),
                name: format!("上{}月结算日热量", x + 1),
                format: DataFormat::BCDWithUnit {
                    data_length: 4,
                    decimals: 2,
                },
                length: 5,
                has_unit_code: true,
            }],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Heat,
            },
            schema,
        );
    }

    // DI: 0xD200-0xD2FF - Historical data 2 (heat)
    // Structure (18 bytes = 0x12): DI(2) + SER(1) + Settlement Heat(5) + Settlement Cooling(5) + Cumulative Flow(5)
    for xx in 0x00..=0xFF {
        let di = 0xD200 + xx;
        let schema = DIResponseSchema {
            di,
            applicable_meter_types: Some(vec![MeterTypeFamily::Heat]),
            fields: vec![
                ResponseField {
                    key: "settlement_heat".to_string(),
                    name: format!("上{}月结算日热量", xx + 1),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "settlement_cooling".to_string(),
                    name: format!("上{}月结算日冷量", xx + 1),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
                ResponseField {
                    key: "settlement_flow".to_string(),
                    name: format!("上{}月结算日累积流量", xx + 1),
                    format: DataFormat::BCDWithUnit {
                        data_length: 4,
                        decimals: 2,
                    },
                    length: 5,
                    has_unit_code: true,
                },
            ],
        };
        registry.insert(
            DISchemaKey {
                di,
                meter_family: MeterTypeFamily::Heat,
            },
            schema,
        );
    }
}

/// Register common schemas (applicable to all meter types)
///
/// These DIs return the same structure regardless of meter type.
fn register_common_schemas(registry: &mut HashMap<DISchemaKey, DIResponseSchema>) {
    // DI: 0x907F - Real-time clock only (9 bytes total)
    //
    // Structure:
    // - DI (2 bytes Binary)
    // - DateTime (7 bytes BCD)
    let schema_907f = DIResponseSchema {
        di: 0x907F,
        applicable_meter_types: None, // Applies to all meter types
        fields: vec![ResponseField {
            key: "datetime".to_string(),
            name: "实时时间".to_string(),
            format: DataFormat::DateTime,
            length: 7,
            has_unit_code: false,
        }],
    };

    // DI: 0x8102 - Price table (18 bytes = 0x12)
    let schema_8102 = DIResponseSchema {
        di: 0x8102,
        applicable_meter_types: None,
        fields: vec![
            ResponseField {
                key: "price_1".to_string(),
                name: "价格一".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 3,
                has_unit_code: false,
            },
            ResponseField {
                key: "volume_1".to_string(),
                name: "用量一".to_string(),
                format: DataFormat::BCDInteger,
                length: 3,
                has_unit_code: false,
            },
            ResponseField {
                key: "price_2".to_string(),
                name: "价格二".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 3,
                has_unit_code: false,
            },
            ResponseField {
                key: "volume_2".to_string(),
                name: "用量二".to_string(),
                format: DataFormat::BCDInteger,
                length: 3,
                has_unit_code: false,
            },
            ResponseField {
                key: "price_3".to_string(),
                name: "价格三".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 3,
                has_unit_code: false,
            },
        ],
    };

    // DI: 0x8103 - Settlement date (4 bytes)
    let schema_8103 = DIResponseSchema {
        di: 0x8103,
        applicable_meter_types: None,
        fields: vec![ResponseField {
            key: "settlement_day".to_string(),
            name: "结算日".to_string(),
            format: DataFormat::BCDInteger,
            length: 1,
            has_unit_code: false,
        }],
    };

    // DI: 0x8104 - Meter reading date (4 bytes)
    let schema_8104 = DIResponseSchema {
        di: 0x8104,
        applicable_meter_types: None,
        fields: vec![ResponseField {
            key: "reading_day".to_string(),
            name: "抄表日".to_string(),
            format: DataFormat::BCDInteger,
            length: 1,
            has_unit_code: false,
        }],
    };

    // DI: 0x8105 - Purchase amount (18 bytes = 0x12)
    let schema_8105 = DIResponseSchema {
        di: 0x8105,
        applicable_meter_types: None,
        fields: vec![
            ResponseField {
                key: "purchase_seq".to_string(),
                name: "本次购买序号".to_string(),
                format: DataFormat::BCDInteger,
                length: 1,
                has_unit_code: false,
            },
            ResponseField {
                key: "this_purchase".to_string(),
                name: "本次购入金额".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 4,
                has_unit_code: false,
            },
            ResponseField {
                key: "total_purchase".to_string(),
                name: "累计购入金额".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 4,
                has_unit_code: false,
            },
            ResponseField {
                key: "remaining".to_string(),
                name: "剩余金额".to_string(),
                format: DataFormat::BCD { decimals: 2 },
                length: 4,
                has_unit_code: false,
            },
            ResponseField {
                key: "status".to_string(),
                name: "状态".to_string(),
                format: DataFormat::Status,
                length: 2,
                has_unit_code: false,
            },
        ],
    };

    // Register for all meter families
    for family in [
        MeterTypeFamily::Water,
        MeterTypeFamily::Heat,
        MeterTypeFamily::Gas,
        MeterTypeFamily::Custom,
    ] {
        registry.insert(
            DISchemaKey {
                di: 0x907F,
                meter_family: family,
            },
            schema_907f.clone(),
        );
        registry.insert(
            DISchemaKey {
                di: 0x8102,
                meter_family: family,
            },
            schema_8102.clone(),
        );
        registry.insert(
            DISchemaKey {
                di: 0x8103,
                meter_family: family,
            },
            schema_8103.clone(),
        );
        registry.insert(
            DISchemaKey {
                di: 0x8104,
                meter_family: family,
            },
            schema_8104.clone(),
        );
        registry.insert(
            DISchemaKey {
                di: 0x8105,
                meter_family: family,
            },
            schema_8105.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_registry_initialization() {
        // Registry should be initialized and non-empty
        assert!(!DI_SCHEMA_REGISTRY.is_empty());

        // Print registry size for verification
        println!("Total schemas registered: {}", DI_SCHEMA_REGISTRY.len());

        // Expected counts:
        // Water/Gas:
        //   - 0x901F: 2 (Water + Gas)
        //   - 0xD120-0xD12B: 12 * 2 = 24
        //   - 0xD200-0xD2FF: 256 * 2 = 512
        //   - 0xD300-0xD3FF: 256 * 2 = 512
        //   - 0xD400-0xD4FF: 256 * 2 = 512
        // Heat:
        //   - 0x901F: 1
        //   - 0x911F: 1
        //   - 0xD120-0xD12B: 12
        //   - 0xD200-0xD2FF: 256
        //   - 0xD300-0xD3FF: 256
        //   - 0xD400-0xD4FF: 256
        // Common (4 meter types each):
        //   - 0x907F: 4
        //   - 0x8102: 4
        //   - 0x8103: 4
        //   - 0x8104: 4
        //   - 0x8105: 4
        //
        // Water/Gas: 0x901F(2) + D12X(24) + D2XX(512) + D3XX(512) + D4XX(512) = 1562
        // Heat: 0x901F(1) + 0x911F(1) + D12X(12) + D2XX(256) = 270
        // Common: 5 DIs × 4 families = 20
        // Total: 1562 + 270 + 20 = 1852

        let expected = 1852;
        assert_eq!(
            DI_SCHEMA_REGISTRY.len(),
            expected,
            "Expected {} schemas, found {}",
            expected,
            DI_SCHEMA_REGISTRY.len()
        );
    }

    #[test]
    fn test_get_di_schema_water_901f() {
        let schema = get_di_schema(0x901F, MeterType::COLD_WATER);
        assert!(schema.is_some());

        let schema = schema.unwrap();
        assert_eq!(schema.di, 0x901F);
        assert_eq!(schema.fields.len(), 4); // current_flow, settlement_flow, datetime, status
        assert_eq!(schema.total_length(), 19); // 5 + 5 + 7 + 2 = 19 (DI+SER excluded)

        // Verify field keys (DI and SER are handled by protocol layer)
        assert!(schema.find_field("current_flow").is_some());
        assert!(schema.find_field("settlement_flow").is_some());
        assert!(schema.find_field("datetime").is_some());
        assert!(schema.find_field("status").is_some());
    }

    #[test]
    fn test_get_di_schema_heat_901f() {
        let schema = get_di_schema(0x901F, MeterType::HEAT);
        assert!(schema.is_some());

        let schema = schema.unwrap();
        assert_eq!(schema.di, 0x901F);
        // Python script removed both di and ser from heat schema, but missed one
        // Let's count: settlement_heat, current_heat, heat_power, flow_rate, cumulative_flow,
        //              supply_temp, return_temp, working_hours, datetime, status = 10 fields
        assert_eq!(schema.fields.len(), 10); // Excluded di and ser
        assert_eq!(schema.total_length(), 40); // 5+5+4+4+5+2+2+4+7+2 = 40

        // Verify heat-specific fields
        assert!(schema.find_field("settlement_heat").is_some());
        assert!(schema.find_field("current_heat").is_some());
        assert!(schema.find_field("heat_power").is_some());
        assert!(schema.find_field("supply_temp").is_some());
        assert!(schema.find_field("return_temp").is_some());
    }

    #[test]
    fn test_get_di_schema_common_907f() {
        // 0x907F should work for any meter type
        let schema_water = get_di_schema(0x907F, MeterType::COLD_WATER);
        let schema_heat = get_di_schema(0x907F, MeterType::HEAT);
        let schema_gas = get_di_schema(0x907F, MeterType::GAS);

        assert!(schema_water.is_some());
        assert!(schema_heat.is_some());
        assert!(schema_gas.is_some());

        // All should have the same structure (only datetime field, DI+SER excluded)
        assert_eq!(schema_water.unwrap().fields.len(), 1);
        assert_eq!(schema_heat.unwrap().fields.len(), 1);
        assert_eq!(schema_gas.unwrap().fields.len(), 1);
    }

    #[test]
    fn test_get_di_schema_not_found() {
        let schema = get_di_schema(0xFFFF, MeterType::COLD_WATER);
        assert!(schema.is_none());
    }

    #[test]
    fn test_schema_field_keys() {
        let schema = get_di_schema(0x901F, MeterType::COLD_WATER).unwrap();
        let keys = schema.field_keys();

        assert_eq!(keys.len(), 4); // current_flow, settlement_flow, datetime, status
        assert!(keys.contains(&"current_flow"));
        assert!(keys.contains(&"datetime"));
    }
}
