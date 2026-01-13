use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::plugin::{ActiveModel, Entity as NorthwardPluginEntity},
    enums::common::{OsArch, OsType, SourceType},
    initializer::SeedableTrait,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Query params for listing northward plugins
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PluginPageParams {
    pub name: Option<String>,
    pub plugin_type: Option<String>,
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

/// Info view for northward plugin
#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::PluginModel as ModelTrait>::Entity")]
pub struct PluginInfo {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub plugin_type: String,
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

/// New northward plugin for insertion or seeding
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPlugin {
    /// Plugin display name
    pub name: String,
    /// Optional description
    pub description: Option<String>,
    /// Plugin type identifier (e.g., "thingsboard")
    pub plugin_type: String,
    /// Plugin source (built-in or custom)
    pub source: SourceType,
    /// Version string
    pub version: String,
    /// API compatibility version
    pub api_version: u32,
    /// SDK version string
    pub sdk_version: String,
    /// Operating system type
    pub os_type: OsType,
    /// CPU architecture
    pub os_arch: OsArch,
    /// File size in bytes (0 for built-in)
    pub size: i64,
    /// Library path or special scheme (e.g., "builtin:thingsboard")
    pub path: String,
    /// Checksum for library verification (empty for built-in)
    pub checksum: String,
    /// Arbitrary plugin metadata in JSON
    pub metadata: serde_json::Value,
}

impl SeedableTrait for NewPlugin {
    type ActiveModel = ActiveModel;
    type Entity = NorthwardPluginEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePlugin {
    pub id: i32,
    pub name: String,
    pub description: Option<String>,
    pub plugin_type: String,
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
