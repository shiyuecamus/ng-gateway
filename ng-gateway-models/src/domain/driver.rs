use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::driver::{ActiveModel, Entity as DriverEntity},
    enums::common::{OsArch, OsType, SourceType},
    initializer::SeedableTrait,
};
use chrono::{DateTime, Utc};
use ng_gateway_sdk::FieldError;
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct DriverPageParams {
    pub name: Option<String>,
    pub driver_type: Option<String>,
    pub source: Option<SourceType>,
    pub version: Option<String>,
    pub sdk_version: Option<String>,
    pub os_type: Option<OsType>,
    pub os_arch: Option<OsArch>,
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::DriverModel as ModelTrait>::Entity")]
pub struct DriverInfo {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub driver_type: String,
    pub source: SourceType,
    pub version: String,
    pub api_version: u32,
    pub sdk_version: String,
    pub os_type: OsType,
    pub os_arch: OsArch,
    pub size: i64,
    pub path: String,
    pub checksum: String,
    pub metadata: serde_json::Value,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewDriver {
    pub name: String,
    pub description: Option<String>,
    pub driver_type: String,
    pub source: SourceType,
    pub version: String,
    pub api_version: u32,
    pub sdk_version: String,
    pub os_type: OsType,
    pub os_arch: OsArch,
    pub size: i64,
    pub path: String,
    pub checksum: String,
    pub metadata: serde_json::Value,
}

impl SeedableTrait for NewDriver {
    type ActiveModel = ActiveModel;
    type Entity = DriverEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct UpdateDriver {
    pub id: i32,
    pub name: String,
    pub description: Option<Option<String>>,
    pub driver_type: String,
    pub source: SourceType,
    pub version: String,
    pub api_version: u32,
    pub sdk_version: String,
    pub os_type: OsType,
    pub os_arch: OsArch,
    pub size: i64,
    pub path: String,
    pub checksum: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PathEntityId {
    pub entity: String,
    pub id: i32,
}

#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct TemplateQuery {
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub total_rows: usize,
    /// Number of valid rows (no blocking errors)
    #[serde(default)]
    pub valid: usize,
    /// Number of invalid rows (with blocking errors)
    #[serde(default)]
    pub invalid: usize,
    /// Number of warnings encountered (non-blocking)
    #[serde(default)]
    pub warn: usize,
    /// A small subset of field errors for preview
    #[serde(default)]
    pub errors: Vec<FieldError>,
}

#[derive(Debug, Clone, Serialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub total_rows: usize,
    pub inserted: usize,
    /// Number of valid rows (no blocking errors)
    #[serde(default)]
    pub valid: usize,
    /// Number of invalid rows (with blocking errors)
    #[serde(default)]
    pub invalid: usize,
    /// Number of warnings encountered (non-blocking)
    #[serde(default)]
    pub warn: usize,
    /// A small subset of field errors for preview
    #[serde(default)]
    pub errors: Vec<FieldError>,
}
