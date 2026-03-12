//! `SeaORM` entity for `model`.

use crate::{
    entities::{
        ai::pipeline::{PostProcessorConfig, PreProcessorConfig},
        NGEntity,
    },
    enums::{
        ai::{ModelFormat, ModelTask, TensorDType},
        common::{EntityType, Status},
    },
};
use ng_gateway_macros::IntoActiveValue;
use sea_orm::{entity::prelude::*, FromJsonQueryResult};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "model")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub key: String,
    pub name: String,
    pub version: String,
    pub task: ModelTask,
    pub format: ModelFormat,
    pub path: String,
    pub labels: Option<Labels>,
    pub default_preprocess: Option<PreProcessorConfig>,
    pub default_postprocess: Option<PostProcessorConfig>,
    pub inputs: Option<TensorDescs>,
    pub outputs: Option<TensorDescs>,
    pub size: u64,
    pub status: Status,
    pub checksum: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Clone, Debug, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct Labels(pub Vec<String>);

#[derive(Clone, Debug, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct TensorDescs(pub Vec<TensorDesc>);

#[derive(Clone, Debug, PartialEq, Serialize, IntoActiveValue, Deserialize, FromJsonQueryResult)]
pub struct TensorDesc {
    /// Tensor name.
    pub name: String,
    /// Tensor shape (e.g., `[1, 3, 640, 640]` for YOLO input).
    /// Negative values indicate dynamic dimensions.
    pub shape: Vec<i64>,
    /// Element data type.
    pub dtype: TensorDType,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl NGEntity for Model {
    fn entity_type(&self) -> EntityType {
        EntityType::Model
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
        EntityType::Model
    }

    fn id(&self) -> Option<i32> {
        self.id.to_owned().take()
    }

    fn status(&self) -> Option<Status> {
        self.status.to_owned().take()
    }
}

impl ActiveModelBehavior for ActiveModel {}
