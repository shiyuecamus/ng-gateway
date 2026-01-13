use crate::{
    domain::common::{PageParams, TimeRangeParams},
    entities::{
        app_sub::{ActiveModel, Entity as AppSubEntity},
        prelude::AppSubIds,
    },
    initializer::SeedableTrait,
};
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Device node within a channel device tree
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTreeNode {
    pub id: i32,
    pub name: String,
}

/// Channel with its devices for selection trees
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDeviceTree {
    pub id: i32,
    pub name: String,
    pub devices: Vec<DeviceTreeNode>,
}

/// Query params for listing northward plugins
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct AppSubPageParams {
    pub app_id: Option<i32>,
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Northward subscription information used for read-only responses
#[derive(Debug, Serialize, Clone, Deserialize, FromQueryResult, DerivePartialModel)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::AppSubModel as ModelTrait>::Entity")]
pub struct AppSubInfo {
    pub id: i32,
    pub app_id: i32,
    pub all_devices: bool,
    pub device_ids: Option<AppSubIds>,
    pub priority: i16,
}

/// New northward subscription for insertion
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewAppSub {
    pub app_id: i32,
    pub all_devices: bool,
    pub device_ids: Option<AppSubIds>,
    pub priority: i16,
}

impl SeedableTrait for NewAppSub {
    type ActiveModel = ActiveModel;
    type Entity = AppSubEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

/// Payload to update an existing northward subscription
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAppSub {
    pub id: i32,
    pub all_devices: bool,
    pub device_ids: Option<Option<AppSubIds>>,
    pub priority: i16,
}
