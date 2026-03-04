//! `SeaORM` entity for `pipeline_binding`.

use crate::enums::common::Status;
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "pipeline_binding")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub channel_id: i32,
    pub pipeline_id: i32,
    pub status: Status,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "crate::entities::channel::Entity",
        from = "Column::ChannelId",
        to = "crate::entities::channel::Column::Id"
    )]
    Channel,
    #[sea_orm(
        belongs_to = "super::pipeline::Entity",
        from = "Column::PipelineId",
        to = "super::pipeline::Column::Id"
    )]
    Pipeline,
}

impl Related<super::super::channel::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Channel.def()
    }
}

impl Related<super::pipeline::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Pipeline.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
