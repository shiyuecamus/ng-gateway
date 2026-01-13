use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::app::{ActiveModel, Entity as NorthwardAppEntity},
    enums::common::Status,
    initializer::SeedableTrait,
};
use chrono::{DateTime, Utc};
use ng_gateway_sdk::{NorthwardConnectionState, QueuePolicy, RetryPolicy};
use sea_orm::{DeriveIntoActiveModel, FromQueryResult, IntoActiveModel};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use validator::Validate;

/// Page query parameters for northward app listing
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct AppPageParams {
    /// Optional fuzzy search by app name
    pub name: Option<String>,
    /// Optional filter by plugin id
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub plugin_id: Option<i32>,
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

/// Northward app information used for read-only responses
#[derive(Debug, Serialize, Clone, Deserialize, FromQueryResult)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    /// App id
    pub id: i32,
    /// Plugin id
    pub plugin_id: i32,
    /// e.g. "thingsboard:0.0.1" (plugin_type:version)
    pub plugin_type: String,
    /// App name
    pub name: String,
    /// App description
    pub description: Option<String>,
    /// App config
    pub config: serde_json::Value,
    /// App retry policy
    pub retry_policy: RetryPolicy,
    /// App queue policy
    pub queue_policy: QueuePolicy,
    /// App status
    pub status: Status,
    /// App created at
    pub created_at: DateTime<Utc>,
    /// App updated at
    pub updated_at: DateTime<Utc>,
    /// Connection state from runtime manager (optional, not from database)
    #[sea_orm(skip)]
    #[serde(skip_deserializing, default)]
    pub connection_state: Option<NorthwardConnectionState>,
}

/// New northward app for insertion
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewApp {
    pub plugin_id: i32,
    pub name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
    pub retry_policy: RetryPolicy,
    pub queue_policy: QueuePolicy,
}

impl SeedableTrait for NewApp {
    type ActiveModel = ActiveModel;
    type Entity = NorthwardAppEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

/// Payload to update an existing northward app
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApp {
    /// App id
    pub id: i32,
    /// App name
    pub name: String,
    /// App description
    pub description: Option<Option<String>>,
    /// App config
    pub config: serde_json::Value,
    /// App retry policy
    pub retry_policy: RetryPolicy,
    /// App queue policy
    pub queue_policy: QueuePolicy,
}

/// Change northward app status payload
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeAppStatus {
    /// App id
    pub id: i32,
    /// Target status
    pub status: Status,
}
