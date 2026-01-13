use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// App extension entity
///
/// Represents plugin-specific persistent key-value storage.
/// Each app can store arbitrary JSON data using this table.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "app_ext")]
pub struct Model {
    /// Primary key
    #[sea_orm(primary_key)]
    pub id: i32,

    /// Foreign key to app.id
    pub app_id: i32,

    /// Extension key (unique per app)
    pub key: String,

    /// Extension value as JSON
    pub value: serde_json::Value,
}

/// Entity relations
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// One-to-many relation with app
    /// When app is deleted, all extensions are cascaded
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
