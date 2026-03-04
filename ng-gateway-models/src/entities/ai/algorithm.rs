//! `SeaORM` entity for `algorithm`.

use crate::{
    entities::NGEntity,
    enums::{
        ai::AlgorithmModuleType,
        common::{EntityType, Status},
    },
};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "algorithm")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub module_type: AlgorithmModuleType,
    pub path: String,
    pub config_schema: Option<Json>,
    pub size: u64,
    pub status: Status,
    pub checksum: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl NGEntity for Model {
    fn entity_type(&self) -> EntityType {
        EntityType::Algorithm
    }

    fn id(&self) -> Option<i32> {
        Some(self.id)
    }

    fn status(&self) -> Option<Status> {
        Some(self.status)
    }
}

impl NGEntity for ActiveModel {
    fn entity_type(&self) -> EntityType {
        EntityType::Algorithm
    }

    fn id(&self) -> Option<i32> {
        self.id.to_owned().take()
    }

    fn status(&self) -> Option<Status> {
        self.status.to_owned().take()
    }
}

impl ActiveModelBehavior for ActiveModel {}
