use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::domain::prelude::{AlgorithmInfo, AlgorithmPageParams, PageResult};
use ng_gateway_models::entities::prelude::{
    Algorithm, AlgorithmActiveModel, AlgorithmColumn, AlgorithmModel,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, QueryTrait,
};

/// Repository for AI algorithm metadata persistence.
pub struct AlgorithmRepository;

impl AlgorithmRepository {
    /// Find an algorithm row by unique `algorithm_key`.
    pub async fn find_by_key(algorithm_key: &str) -> StorageResult<Option<AlgorithmModel>> {
        let db = get_db_connection().await?;
        Ok(Algorithm::find()
            .filter(AlgorithmColumn::Key.eq(algorithm_key))
            .one(&db)
            .await?)
    }

    /// List all algorithm rows ordered by identifier.
    pub async fn list_all<C>(db: Option<&C>) -> StorageResult<Vec<AlgorithmModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Algorithm::find()
                .order_by_asc(AlgorithmColumn::Id)
                .all(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Algorithm::find()
                    .order_by_asc(AlgorithmColumn::Id)
                    .all(&conn)
                    .await?)
            }
        }
    }

    /// Paginate algorithm rows with optional filters.
    pub async fn page(params: AlgorithmPageParams) -> StorageResult<PageResult<AlgorithmInfo>> {
        let db = get_db_connection().await?;
        let base = Algorithm::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(AlgorithmColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.module_type, |q, module_type| {
                q.filter(AlgorithmColumn::ModuleType.eq(module_type))
            })
            .apply_if(params.status, |q, status| {
                q.filter(AlgorithmColumn::Status.eq(status))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(AlgorithmColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(AlgorithmColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by_asc(AlgorithmColumn::Id);
        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let paginator = base
            .into_partial_model::<AlgorithmInfo>()
            .paginate(&db, page_size);
        let total = paginator.num_items().await?;
        let records = paginator.fetch_page(page.saturating_sub(1)).await?;
        Ok(PageResult {
            records,
            total,
            pages: if page_size > 0 {
                total.div_ceil(page_size)
            } else {
                0
            },
            page,
            page_size,
        })
    }

    /// Insert one algorithm row.
    pub async fn create<C>(
        algorithm: AlgorithmActiveModel,
        db: Option<&C>,
    ) -> StorageResult<AlgorithmModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(algorithm.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(algorithm.insert(&conn).await?)
            }
        }
    }

    /// Update one algorithm row.
    pub async fn update<C>(
        algorithm: AlgorithmActiveModel,
        db: Option<&C>,
    ) -> StorageResult<AlgorithmModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(algorithm.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(algorithm.update(&conn).await?)
            }
        }
    }

    /// Delete one algorithm row by `algorithm_key`.
    pub async fn delete_by_key<C>(algorithm_key: &str, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Algorithm::delete_many()
                    .filter(AlgorithmColumn::Key.eq(algorithm_key))
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Algorithm::delete_many()
                    .filter(AlgorithmColumn::Key.eq(algorithm_key))
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }
}
