use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::channel::ActiveModel,
    enums::common::{CollectionType, ReportType, Status},
};
use ng_gateway_sdk::{ConnectionPolicy, SouthwardConnectionState};
use sea_orm::{DeriveIntoActiveModel, FromQueryResult};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_json::Value as Json;
use validator::Validate;

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPageParams {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub status: Option<i32>,
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// User information
#[derive(Debug, Serialize, Deserialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    /// e.g. "modbus:0.0.1" (driver_type:version)
    pub driver_type: String,
    pub collection_type: CollectionType,
    pub period: Option<u32>,
    pub report_type: ReportType,
    pub status: Status,
    pub connection_policy: ConnectionPolicy,
    pub driver_config: Json,
    /// Connection state from runtime manager (optional, not from database)
    #[sea_orm(skip)]
    #[serde(skip_deserializing, default)]
    pub connection_state: Option<SouthwardConnectionState>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, DeriveIntoActiveModel, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewChannel {
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub period: Option<u32>,
    pub report_type: ReportType,
    pub connection_policy: ConnectionPolicy,
    pub driver_config: Json,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChannel {
    pub id: i32,
    pub name: String,
    pub driver_id: i32,
    pub collection_type: CollectionType,
    pub period: Option<Option<u32>>,
    pub report_type: ReportType,
    pub connection_policy: ConnectionPolicy,
    pub driver_config: Json,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeChannelStatus {
    pub id: i32,
    pub status: Status,
}
