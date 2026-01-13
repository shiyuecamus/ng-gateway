use super::common::{PageParams, TimeRangeParams};
use crate::{
    domain::role::SimpleRole,
    entities::user::{ActiveModel, Entity as UserEntity, Model as UserModel},
    enums::common::Status,
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
pub struct UserPageParams {
    pub username: Option<String>,
    pub email: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, DerivePartialModel, FromQueryResult)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::UserModel as ModelTrait>::Entity")]
pub struct UserInfo {
    pub id: i32,
    pub username: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub status: Status,
    pub additional_info: Option<Json>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl From<UserModel> for UserInfo {
    fn from(user: UserModel) -> Self {
        UserInfo {
            id: user.id,
            username: user.username,
            nickname: user.nickname,
            avatar: user.avatar,
            phone: user.phone,
            email: user.email,
            status: user.status,
            additional_info: user.additional_info,
            created_at: user.created_at,
            updated_at: user.updated_at,
        }
    }
}

/// User information with roles
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInfoWithRoles {
    pub id: i32,
    pub username: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub status: Status,
    pub additional_info: Option<Json>,
    pub roles: Option<Vec<SimpleRole>>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl From<UserInfo> for UserInfoWithRoles {
    fn from(user: UserInfo) -> Self {
        Self {
            id: user.id,
            username: user.username,
            nickname: user.nickname,
            avatar: user.avatar,
            phone: user.phone,
            email: user.email,
            status: user.status,
            additional_info: user.additional_info,
            created_at: user.created_at,
            updated_at: user.updated_at,
            roles: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, DeriveIntoActiveModel, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewUser {
    pub username: String,
    pub password: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub additional_info: Option<Json>,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveIntoActiveModel)]
pub struct NewUserWithId {
    pub id: i32,
    pub username: String,
    pub password: String,
    pub nickname: String,
    pub avatar: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub additional_info: Option<Json>,
}

impl SeedableTrait for NewUserWithId {
    type ActiveModel = ActiveModel;
    type Entity = UserEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
pub struct UpdateUser {
    pub id: i32,
    pub nickname: String,
    pub avatar: Option<Option<String>>,
    pub phone: Option<Option<String>>,
    pub email: Option<Option<String>>,
    pub additional_info: Option<Option<Json>>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ChangeUserPassword {
    pub id: i32,
    #[serde(default)]
    #[validate(required(message = "Old password is required"))]
    pub old_password: Option<String>,
    #[serde(default)]
    #[validate(required(message = "New password is required"))]
    pub new_password: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct ResetUserPassword {
    pub id: i32,
    #[serde(default)]
    #[validate(required(message = "New password is required"))]
    pub new_password: Option<String>,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeUserPasswordWithId {
    pub id: i32,
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct ChangeUserStatus {
    pub id: i32,
    pub status: Status,
}
