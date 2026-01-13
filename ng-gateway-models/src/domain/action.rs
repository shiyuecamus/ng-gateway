use super::common::{PageParams, TimeRangeParams};
use crate::entities::action::{ActiveModel, Parameter, Parameters};
use crate::enums::common::DataType;
use ng_gateway_sdk::{DriverError, FromValidatedRow, RowMappingContext, ValidatedRow};
use sea_orm::{DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, ModelTrait};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_json::Value as Json;
use validator::Validate;

/// Page query parameters for action listing
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ActionPageParams {
    /// Optional fuzzy search by action name
    pub name: Option<String>,
    /// Optional exact filter by command
    pub command: Option<String>,
    /// Optional filter by device id
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub device_id: Option<i32>,
    /// Pagination parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Time range parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Action information used for read-only responses
#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[sea_orm(entity = "<crate::entities::prelude::ActionModel as ModelTrait>::Entity")]
#[serde(rename_all = "camelCase")]
pub struct ActionInfo {
    /// Action id
    pub id: i32,
    /// Device id
    pub device_id: i32,
    /// Action name
    pub name: String,
    /// Command name
    pub command: String,
    /// Input parameters
    pub inputs: Parameters,
}

/// Payload to create a new action
#[derive(Clone, Debug, PartialEq, Deserialize, DeriveIntoActiveModel, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewAction {
    /// Device id
    pub device_id: i32,
    /// Action name
    pub name: String,
    /// Command name
    pub command: String,
    /// Input parameters
    pub inputs: Parameters,
}

impl FromValidatedRow for NewAction {
    /// Build `NewAction` from a validated import row. One row corresponds to one parameter.
    /// Caller may aggregate multiple rows (same name/command) into a single action by merging
    /// their `inputs` vectors at the web layer.
    fn from_validated_row(
        row: &ValidatedRow,
        context: &RowMappingContext,
    ) -> Result<Self, DriverError> {
        fn ensure_str(
            map: &serde_json::Map<String, serde_json::Value>,
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

        fn as_i64(v: &serde_json::Value) -> Option<i64> {
            match v {
                serde_json::Value::Number(n) => n.as_i64(),
                serde_json::Value::String(s) => s.parse::<i64>().ok(),
                _ => None,
            }
        }

        fn get_enum_i16(
            map: &serde_json::Map<String, serde_json::Value>,
            key: &str,
        ) -> Option<i16> {
            map.get(key)
                .and_then(as_i64)
                .and_then(|n| i16::try_from(n).ok())
        }

        let device_id = context.entity_id;
        let name = ensure_str(&row.values, "action_name", row.row_index)?;
        let command = ensure_str(&row.values, "command", row.row_index)?;

        // One parameter per row
        let param_name = ensure_str(&row.values, "param_name", row.row_index)?;
        let param_key = ensure_str(&row.values, "param_key", row.row_index)?;
        let dt_code = get_enum_i16(&row.values, "param_data_type").ok_or(
            DriverError::ExecutionError(format!("row {}: invalid param_data_type", row.row_index)),
        )?;
        let data_type = DataType::try_from(dt_code)
            .map_err(|e| DriverError::ExecutionError(format!("row {}: {}", row.row_index, e)))?;

        let required = row
            .values
            .get("param_required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let default_value = row.values.get("param_default_value").cloned();
        let min_value = row.values.get("param_min_value").and_then(|v| v.as_f64());
        let max_value = row.values.get("param_max_value").and_then(|v| v.as_f64());
        let driver_config = row
            .values
            .get("driver_config")
            .cloned()
            .unwrap_or_else(|| Json::Object(serde_json::Map::new()));

        Ok(NewAction {
            device_id,
            name,
            command,
            inputs: Parameters(vec![Parameter {
                name: param_name,
                key: param_key,
                data_type,
                required,
                default_value,
                max_value,
                min_value,
                driver_config,
            }]),
        })
    }
}

/// Payload to update an existing action
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAction {
    /// Action id
    pub id: i32,
    /// Device id
    pub device_id: i32,
    /// Action name
    pub name: String,
    /// Command name
    pub command: String,
    /// Input parameters
    pub inputs: Parameters,
}
