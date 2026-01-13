use super::common::{PageParams, TimeRangeParams};
use crate::{
    entities::role::{ActiveModel, Entity as RoleEntity, Model as RoleModel},
    enums::{common::Status, role::RoleType},
    initializer::SeedableTrait,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    DeriveIntoActiveModel, DerivePartialModel, FromQueryResult, IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use serde_aux::prelude::*;
use serde_json::Value as Json;
use validator::Validate;

#[derive(Debug, Clone, Deserialize, Validate)]
pub struct RolePageParams {
    pub name: Option<String>,
    pub code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_number_from_string")]
    pub status: Option<i32>,
    #[serde(flatten)]
    #[validate(nested)]
    pub page: PageParams,
    #[serde(flatten)]
    #[validate(nested)]
    pub time_range: TimeRangeParams,
}

/// Role information
#[derive(Debug, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::RoleModel as ModelTrait>::Entity")]
pub struct RoleInfo {
    pub id: i32,
    pub name: String,
    pub code: String,
    pub sort: Option<i32>,
    pub r#type: RoleType,
    pub status: Status,
    pub additional_info: Option<Json>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewRole {
    pub name: String,
    pub code: String,
    pub sort: Option<i32>,
    pub r#type: RoleType,
    pub additional_info: Option<Json>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewRoleWithId {
    pub id: i32,
    pub name: String,
    pub code: String,
    pub sort: Option<i32>,
    pub r#type: RoleType,
    pub additional_info: Option<Json>,
}

impl SeedableTrait for NewRoleWithId {
    type ActiveModel = ActiveModel;
    type Entity = RoleEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct UpdateRole {
    pub id: i32,
    pub name: String,
    pub code: String,
    pub sort: Option<Option<i32>>,
    pub r#type: RoleType,
    pub additional_info: Option<Option<Json>>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeRoleStatus {
    pub id: i32,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimpleRole {
    pub id: i32,
    pub code: String,
}

impl From<RoleModel> for SimpleRole {
    fn from(role: RoleModel) -> Self {
        Self {
            id: role.id,
            code: role.code,
        }
    }
}

impl From<RoleInfo> for SimpleRole {
    fn from(role: RoleInfo) -> Self {
        Self {
            id: role.id,
            code: role.code,
        }
    }
}
