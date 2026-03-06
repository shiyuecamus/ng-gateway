use crate::{
    entities::ai::pipeline_stage::{
        ActiveModel, Entity as PipelineStageEntity, Model as PipelineStageModel, StageConfig,
    },
    initializer::SeedableTrait,
};
use sea_orm::{
    prelude::DateTimeUtc, DeriveIntoActiveModel, DerivePartialModel, FromQueryResult,
    IntoActiveModel, ModelTrait,
};
use serde::{Deserialize, Serialize};
use validator::Validate;

/// Model metadata stored in the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, FromQueryResult, DerivePartialModel)]
#[serde(rename_all = "camelCase")]
#[sea_orm(entity = "<crate::entities::prelude::PipelineStageModel as ModelTrait>::Entity")]
pub struct PipelineStageInfo {
    /// Pipeline stage unique identifier.
    pub id: i32,
    /// Pipeline ID.
    pub pipeline_id: i32,
    /// Pipeline stage order.
    pub stage_order: i32,
    /// Pipeline stage config.
    pub config: StageConfig,
    /// Created at timestamp.
    pub created_at: DateTimeUtc,
    /// Updated at timestamp.
    pub updated_at: DateTimeUtc,
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct NewPipelineStage {
    /// Pipeline ID.
    pub pipeline_id: i32,
    /// Pipeline stage order.
    pub stage_order: i32,
    /// Pipeline stage config.
    pub config: StageConfig,
}

impl SeedableTrait for NewPipelineStage {
    type ActiveModel = ActiveModel;
    type Entity = PipelineStageEntity;

    fn get_active_model(&self) -> Self::ActiveModel {
        self.clone().into_active_model()
    }
}

#[derive(Clone, Debug, PartialEq, DeriveIntoActiveModel, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePipelineStage {
    /// Pipeline stage unique identifier.
    pub id: i32,
    /// Pipeline ID.
    pub pipeline_id: i32,
    /// Pipeline stage order.
    pub stage_order: i32,
    /// Pipeline stage config.
    pub config: StageConfig,
}

impl PipelineStageInfo {
    /// Build an active model ready for DB insertion as part of a pipeline
    /// create/update operation. Caller provides the owning `pipeline_id`
    /// and the positional `order` within the pipeline.
    pub fn to_insert_active_model(&self, pipeline_id: i32, order: i32) -> ActiveModel {
        NewPipelineStage {
            pipeline_id,
            stage_order: order,
            config: self.config.clone(),
        }
        .into_active_model()
    }
}

impl From<PipelineStageModel> for PipelineStageInfo {
    fn from(model: PipelineStageModel) -> Self {
        Self {
            id: model.id,
            pipeline_id: model.pipeline_id,
            stage_order: model.stage_order,
            config: model.config,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}
