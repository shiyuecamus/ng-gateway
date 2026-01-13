use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::point::ActiveModel,
    enums::common::{AccessMode, DataPointType, DataType},
};
use ng_gateway_sdk::{DriverError, FromValidatedRow, RowMappingContext, ValidatedRow};
use sea_orm::{DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, ModelTrait};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_json::Value as Json;
use validator::Validate;

/// Page query parameters for point listing
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PointPageParams {
    /// Optional fuzzy search by name
    pub name: Option<String>,
    /// Optional exact search by key
    pub key: Option<String>,
    /// Optional filter by device id
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub device_id: Option<i32>,
    /// Optional filter by data point type
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub r#type: Option<i32>,
    /// Optional filter by data type
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub data_type: Option<i32>,
    /// Optional filter by access mode
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub access_mode: Option<i32>,
    /// Pagination parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Time range parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Point information used for read-only responses
#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[sea_orm(entity = "<crate::entities::prelude::PointModel as ModelTrait>::Entity")]
#[serde(rename_all = "camelCase")]
pub struct PointInfo {
    /// Point id
    pub id: i32,
    /// Device id
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Type
    pub r#type: DataPointType,
    /// Data type
    pub data_type: DataType,
    /// Access mode
    pub access_mode: AccessMode,
    /// Unit
    pub unit: Option<String>,
    /// Min value
    pub min_value: Option<f64>,
    /// Max value
    pub max_value: Option<f64>,
    /// Scale
    pub scale: Option<f64>,
    /// Driver configuration payload
    pub driver_config: Json,
}

/// Payload to create a new point
#[derive(Clone, Debug, PartialEq, Deserialize, DeriveIntoActiveModel, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewPoint {
    /// Device id
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Type
    pub r#type: DataPointType,
    /// Data type
    pub data_type: DataType,
    /// Access mode
    pub access_mode: AccessMode,
    /// Unit
    pub unit: Option<String>,
    /// Min value
    pub min_value: Option<f64>,
    /// Max value
    pub max_value: Option<f64>,
    /// Scale
    pub scale: Option<f64>,
    /// Driver configuration payload
    pub driver_config: Json,
}

impl FromValidatedRow for NewPoint {
    /// Build `NewPoint` from a validated import row.
    ///
    /// Assumes field-level validation has ensured presence and basic type constraints
    /// for the required fields. Performs minimal checks and maps enum codes.
    fn from_validated_row(
        row: &ValidatedRow,
        context: &RowMappingContext,
    ) -> Result<Self, DriverError> {
        fn ensure_str(
            map: &serde_json::Map<String, Json>,
            key: &str,
            row_idx: usize,
        ) -> Result<String, DriverError> {
            map.get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .ok_or(DriverError::ExecutionError(format!(
                    "row {}: missing {}",
                    row_idx, key
                )))
        }

        fn as_i64(v: &Json) -> Option<i64> {
            match v {
                Json::Number(n) => n.as_i64(),
                Json::String(s) => s.parse::<i64>().ok(),
                _ => None,
            }
        }

        fn get_enum_i16(map: &serde_json::Map<String, Json>, key: &str) -> Option<i16> {
            map.get(key)
                .and_then(as_i64)
                .and_then(|n| i16::try_from(n).ok())
        }

        let device_id = context.entity_id;
        let name = ensure_str(&row.values, "name", row.row_index)?;
        let key = ensure_str(&row.values, "key", row.row_index)?;

        let typ_code = get_enum_i16(&row.values, "type").ok_or(DriverError::ExecutionError(
            format!("row {}: invalid type", row.row_index),
        ))?;

        let data_type_code = get_enum_i16(&row.values, "data_type").ok_or(
            DriverError::ExecutionError(format!("row {}: invalid data_type", row.row_index)),
        )?;
        let access_mode_code = get_enum_i16(&row.values, "access_mode").ok_or(
            DriverError::ExecutionError(format!("row {}: invalid access_mode", row.row_index)),
        )?;

        let r#type = DataPointType::try_from(typ_code)
            .map_err(|e| DriverError::ExecutionError(format!("row {}: {}", row.row_index, e)))?;

        let data_type = DataType::try_from(data_type_code)
            .map_err(|e| DriverError::ExecutionError(format!("row {}: {}", row.row_index, e)))?;

        let access_mode = AccessMode::try_from(access_mode_code)
            .map_err(|e| DriverError::ExecutionError(format!("row {}: {}", row.row_index, e)))?;

        let unit = row
            .values
            .get("unit")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let min_value = row.values.get("min_value").and_then(|v| v.as_f64());
        let max_value = row.values.get("max_value").and_then(|v| v.as_f64());
        let scale = row.values.get("scale").and_then(|v| v.as_f64());

        let driver_config = row
            .values
            .get("driver_config")
            .cloned()
            .unwrap_or_else(|| Json::Object(serde_json::Map::new()));

        Ok(NewPoint {
            device_id,
            name,
            key,
            r#type,
            data_type,
            access_mode,
            unit,
            min_value,
            max_value,
            scale,
            driver_config,
        })
    }
}

/// Payload to update an existing point
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePoint {
    /// Point id
    pub id: i32,
    /// Device ID
    pub device_id: i32,
    /// Name
    pub name: String,
    /// Key
    pub key: String,
    /// Type
    pub r#type: DataPointType,
    /// Data type
    pub data_type: DataType,
    /// Access mode
    pub access_mode: AccessMode,
    /// Unit (use Some(None) to clear)
    pub unit: Option<Option<String>>,
    /// Min value (use Some(None) to clear)
    pub min_value: Option<Option<f64>>,
    /// Max value (use Some(None) to clear)
    pub max_value: Option<Option<f64>>,
    /// Scale (use Some(None) to clear)
    pub scale: Option<Option<f64>>,
    /// Driver configuration payload
    pub driver_config: Json,
}
