use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::entities::prelude::{
    PipelineStage, PipelineStageActiveModel, PipelineStageColumn, PipelineStageModel,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};

/// Repository for AI pipeline stage persistence.
pub struct PipelineStageRepository;

impl PipelineStageRepository {
    /// List pipeline stages by `pipeline_id`, ordered by `stage_order`.
    pub async fn list_by_pipeline_id(pipeline_id: i32) -> StorageResult<Vec<PipelineStageModel>> {
        let db = get_db_connection().await?;
        Ok(PipelineStage::find()
            .filter(PipelineStageColumn::PipelineId.eq(pipeline_id))
            .order_by_asc(PipelineStageColumn::StageOrder)
            .all(&db)
            .await?)
    }

    /// Replace all stage rows for one pipeline in a single connection context.
    pub async fn replace_by_pipeline_id<C>(
        pipeline_id: i32,
        stages: Vec<PipelineStageActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                PipelineStage::delete_many()
                    .filter(PipelineStageColumn::PipelineId.eq(pipeline_id))
                    .exec(conn)
                    .await?;
                if !stages.is_empty() {
                    PipelineStage::insert_many(stages).exec(conn).await?;
                }
            }
            None => {
                let conn = get_db_connection().await?;
                PipelineStage::delete_many()
                    .filter(PipelineStageColumn::PipelineId.eq(pipeline_id))
                    .exec(&conn)
                    .await?;
                if !stages.is_empty() {
                    PipelineStage::insert_many(stages).exec(&conn).await?;
                }
            }
        }
        Ok(())
    }

    /// Delete all stage rows for one pipeline.
    pub async fn delete_by_pipeline_id(pipeline_id: i32) -> StorageResult<()> {
        let db = get_db_connection().await?;
        PipelineStage::delete_many()
            .filter(PipelineStageColumn::PipelineId.eq(pipeline_id))
            .exec(&db)
            .await?;
        Ok(())
    }
}
