use super::common::{PageParams, TimeRangeParams};
use crate::{entities::device::ActiveModel, enums::common::Status};
use ng_gateway_sdk::{DriverError, FromValidatedRow, RowMappingContext, ValidatedRow};
use sea_orm::{DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, ModelTrait};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_json::Value as Json;
use validator::Validate;

/// Page query parameters for device listing
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct DevicePageParams {
    /// Optional fuzzy search by device name
    pub device_name: Option<String>,
    /// Optional exact filter by device type
    pub device_type: Option<String>,
    /// Optional filter by channel id
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub channel_id: Option<i32>,
    /// Optional filter by status
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub status: Option<i32>,
    /// Pagination parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    /// Time range parameters
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Device information used for read-only responses
#[derive(Debug, Serialize, Clone, Deserialize, FromQueryResult, DerivePartialModel)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::DeviceModel as ModelTrait>::Entity")]
pub struct DeviceInfo {
    /// Device id
    pub id: i32,
    /// Channel id
    pub channel_id: i32,
    /// Device name
    pub device_name: String,
    /// Device type
    pub device_type: String,
    /// Device status
    pub status: Status,
    /// Driver configuration payload
    pub driver_config: Option<Json>,
}

/// Payload to create a new device
#[derive(Clone, Debug, PartialEq, Deserialize, DeriveIntoActiveModel, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewDevice {
    /// Channel id the device belongs to
    pub channel_id: i32,
    /// Device name
    pub device_name: String,
    /// Device type
    pub device_type: String,
    /// Driver configuration payload
    pub driver_config: Option<Json>,
}

impl FromValidatedRow for NewDevice {
    /// Build `NewDevice` from a validated import row.
    ///
    /// This mapping assumes required fields were enforced during validation
    /// (device_name, device_type). It still performs presence checks to provide
    /// graceful error messages if invariants are broken upstream.
    fn from_validated_row(
        row: &ValidatedRow,
        context: &RowMappingContext,
    ) -> Result<Self, DriverError> {
        let device_name = row
            .values
            .get("device_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or(DriverError::ExecutionError(format!(
                "row {}: missing device_name",
                row.row_index
            )))?
            .to_string();

        let device_type = row
            .values
            .get("device_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or(DriverError::ExecutionError(format!(
                "row {}: missing device_type",
                row.row_index
            )))?
            .to_string();

        let channel_id = context.entity_id;

        let driver_config = row.values.get("driver_config").cloned();

        Ok(NewDevice {
            channel_id,
            device_name,
            device_type,
            driver_config,
        })
    }
}

/// Payload to update an existing device
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDevice {
    /// Device id
    pub id: i32,
    /// Channel id the device belongs to
    pub channel_id: i32,
    /// Device name
    pub device_name: String,
    /// Device type
    pub device_type: String,
    /// Device status
    pub status: Status,
    /// Driver configuration payload, use Some(None) to clear
    pub driver_config: Option<Option<Json>>,
}

/// Change device status payload
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeDeviceStatus {
    /// Device id
    pub id: i32,
    /// Target status
    pub status: Status,
}
