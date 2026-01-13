//! `SeaORM` Entity definition for northward subscriptions

use ng_gateway_macros::IntoActiveValue;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Serialize, Deserialize)]
#[sea_orm(table_name = "app_sub")]
pub struct Model {
    /// Primary key
    #[sea_orm(primary_key)]
    pub id: i32,
    pub app_id: i32,
    pub all_devices: bool,
    pub device_ids: Option<Ids>,
    pub priority: i16,
}

#[derive(
    Clone, Debug, PartialEq, Eq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult,
)]
pub struct Ids(pub Vec<i32>);

/// Relations for northward subscription
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to a northward app
    #[sea_orm(
        belongs_to = "super::app::Entity",
        from = "Column::AppId",
        to = "super::app::Column::Id"
    )]
    App,
}

impl Related<super::app::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::App.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
