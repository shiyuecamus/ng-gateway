use crate::{entities::credentials::ActiveModel, enums::credentials::CredentialsType};
use sea_orm::DeriveIntoActiveModel;
use serde::Deserialize;
use validator::Validate;

/// Domain model for creating new device credentials
/// Since we only store one set of gateway credentials, device_id is removed
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
pub struct NewCredentials {
    pub r#type: CredentialsType,
    pub certificate: Option<String>,
    pub client_id: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub access_token: Option<String>,
}

/// Domain model for updating existing device credentials
#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Validate, Deserialize)]
#[non_exhaustive]
pub struct UpdateCredentials {
    pub id: i32,
    pub r#type: CredentialsType,
    pub certificate: Option<Option<String>>,
    pub client_id: Option<Option<String>>,
    pub username: Option<Option<String>>,
    pub password: Option<Option<String>>,
    pub access_token: Option<Option<String>>,
}
