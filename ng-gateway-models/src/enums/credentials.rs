use ng_gateway_macros::IntoActiveValue;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde_repr::{Deserialize_repr, Serialize_repr};

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Serialize_repr,
    Deserialize_repr,
    IntoActiveValue,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[repr(i16)]
pub enum CredentialsType {
    Basic = 0,
    AccessToken = 1,
    X509Certificate = 2,
    Lwm2mCredentials = 3,
}
