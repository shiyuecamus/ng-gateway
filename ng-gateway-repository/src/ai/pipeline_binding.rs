use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::entities::prelude::{
    PipelineBinding, PipelineBindingActiveModel, PipelineBindingColumn, PipelineBindingModel,
};
use ng_gateway_models::enums::common::Status;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
};

/// Repository for AI pipeline binding persistence.
pub struct PipelineBindingRepository;

impl PipelineBindingRepository {
    /// Find binding row by channel identifier.
    pub async fn find_by_channel_id(
        channel_id: i32,
    ) -> StorageResult<Option<PipelineBindingModel>> {
        let db = get_db_connection().await?;
        Ok(PipelineBinding::find()
            .filter(PipelineBindingColumn::ChannelId.eq(channel_id))
            .one(&db)
            .await?)
    }

    /// List all enabled bindings.
    pub async fn list_enabled() -> StorageResult<Vec<PipelineBindingModel>> {
        let db = get_db_connection().await?;
        Ok(PipelineBinding::find()
            .filter(PipelineBindingColumn::Status.eq(Status::Enabled))
            .order_by_asc(PipelineBindingColumn::ChannelId)
            .all(&db)
            .await?)
    }

    /// Insert one binding row.
    pub async fn create<C>(
        binding: PipelineBindingActiveModel,
        db: Option<&C>,
    ) -> StorageResult<PipelineBindingModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(binding.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(binding.insert(&conn).await?)
            }
        }
    }

    /// Update one binding row.
    pub async fn update<C>(
        binding: PipelineBindingActiveModel,
        db: Option<&C>,
    ) -> StorageResult<PipelineBindingModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(binding.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(binding.update(&conn).await?)
            }
        }
    }

    /// Delete one binding by `channel_id`.
    pub async fn delete_by_channel_id(channel_id: i32) -> StorageResult<()> {
        let conn = get_db_connection().await?;
        PipelineBinding::delete_many()
            .filter(PipelineBindingColumn::ChannelId.eq(channel_id))
            .exec(&conn)
            .await?;
        Ok(())
    }

    /// Delete all bindings for a pipeline.
    pub async fn delete_by_pipeline_id(pipeline_id: i32) -> StorageResult<()> {
        let conn = get_db_connection().await?;
        PipelineBinding::delete_many()
            .filter(PipelineBindingColumn::PipelineId.eq(pipeline_id))
            .exec(&conn)
            .await?;
        Ok(())
    }

    /// Upsert a binding: if exists for channel_id, update; otherwise insert.
    pub async fn upsert(
        channel_id: i32,
        pipeline_id: i32,
        status: Status,
    ) -> StorageResult<PipelineBindingModel> {
        let db = get_db_connection().await?;
        let existing = PipelineBinding::find()
            .filter(PipelineBindingColumn::ChannelId.eq(channel_id))
            .one(&db)
            .await?;

        match existing {
            Some(row) => {
                let mut active: PipelineBindingActiveModel = row.into();
                active.pipeline_id = sea_orm::ActiveValue::Set(pipeline_id);
                active.status = sea_orm::ActiveValue::Set(status);
                Ok(active.update(&db).await?)
            }
            None => {
                let active = PipelineBindingActiveModel {
                    id: sea_orm::ActiveValue::NotSet,
                    channel_id: sea_orm::ActiveValue::Set(channel_id),
                    pipeline_id: sea_orm::ActiveValue::Set(pipeline_id),
                    status: sea_orm::ActiveValue::Set(status),
                    ..Default::default()
                };
                Ok(active.insert(&db).await?)
            }
        }
    }
}
