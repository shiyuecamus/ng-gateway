use ng_gateway_macros::IntoActiveValue;
use sea_orm::prelude::StringLen;
use sea_orm::{DeriveActiveEnum, EnumIter};
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize, IntoActiveValue,
)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(20))")]
pub enum RelationType {
    #[sea_orm(string_value = "common")]
    Common,
    #[sea_orm(string_value = "contains")]
    Contains,
}
