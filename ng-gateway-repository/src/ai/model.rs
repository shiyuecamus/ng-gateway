use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::domain::prelude::{ModelInfo, ModelPageParams, PageResult};
use ng_gateway_models::entities::prelude::{Model, ModelActiveModel, ModelColumn, ModelModel};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QueryTrait,
};

/// Repository for AI model metadata persistence.
pub struct ModelRepository;

impl ModelRepository {
    /// Find a model row by unique `model_key`.
    pub async fn find_by_key(model_key: &str) -> StorageResult<Option<ModelModel>> {
        let db = get_db_connection().await?;
        Ok(Model::find()
            .filter(ModelColumn::ModelKey.eq(model_key))
            .one(&db)
            .await?)
    }

    /// List all model rows ordered by identifier.
    pub async fn list_all() -> StorageResult<Vec<ModelModel>> {
        let db = get_db_connection().await?;
        Ok(Model::find().order_by_asc(ModelColumn::Id).all(&db).await?)
    }

    /// Paginate model rows with optional filters.
    pub async fn page(params: ModelPageParams) -> StorageResult<PageResult<ModelInfo>> {
        let db = get_db_connection().await?;
        let base = Model::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(ModelColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.task, |q, task| q.filter(ModelColumn::Task.eq(task)))
            .apply_if(params.format, |q, format| {
                q.filter(ModelColumn::Format.eq(format))
            })
            .apply_if(params.status, |q, status| {
                q.filter(ModelColumn::Status.eq(status))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(ModelColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(ModelColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by_asc(ModelColumn::Id);
        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = base.clone().count(&db).await?;
        let records = base
            .into_partial_model::<ModelInfo>()
            .paginate(&db, page_size as u64)
            .fetch_page((page - 1) as u64)
            .await?;
        Ok(PageResult {
            records,
            total,
            pages: ((total as f64) / (page_size as f64)).ceil() as u32,
            page,
            page_size,
        })
    }

    /// Insert one model row.
    pub async fn create<C>(model: ModelActiveModel, db: Option<&C>) -> StorageResult<ModelModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(model.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(model.insert(&conn).await?)
            }
        }
    }

    /// Update one model row.
    pub async fn update<C>(model: ModelActiveModel, db: Option<&C>) -> StorageResult<ModelModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(model.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(model.update(&conn).await?)
            }
        }
    }

    /// Delete one model row by `model_key`.
    pub async fn delete_by_key<C>(model_key: &str, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Model::delete_many()
                    .filter(ModelColumn::ModelKey.eq(model_key))
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Model::delete_many()
                    .filter(ModelColumn::ModelKey.eq(model_key))
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }
}
