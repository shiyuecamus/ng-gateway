use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{AppSubInfo, AppSubPageParams, PageResult},
    entities::prelude::{AppSub, AppSubActiveModel, AppSubColumn, AppSubModel},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for northward sub operations
pub struct AppSubRepository;

impl AppSubRepository {
    /// Create new northward sub
    pub async fn create<C>(sub: AppSubActiveModel, db: Option<&C>) -> StorageResult<AppSubModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(sub.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(sub.insert(&conn).await?)
            }
        }
    }

    /// Update existing northward sub
    pub async fn update<C>(sub: AppSubActiveModel, db: Option<&C>) -> StorageResult<AppSubModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(sub.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(sub.update(&conn).await?)
            }
        }
    }

    /// Delete northward sub by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                AppSub::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                AppSub::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Paginate northward subs with optional filters
    pub async fn page(params: AppSubPageParams) -> StorageResult<PageResult<AppSubInfo>> {
        let db = get_db_connection().await?;

        let query = AppSub::find()
            .apply_if(params.app_id.as_ref(), |q, app_id| {
                q.filter(AppSubColumn::AppId.eq(*app_id))
            })
            .order_by(AppSubColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<AppSubInfo>()
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

    /// Find northward sub by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<AppSubModel>> {
        let db = get_db_connection().await?;
        Ok(AppSub::find_by_id(id).one(&db).await?)
    }

    /// Find northward sub info by ID (partial projection)
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<AppSubInfo>> {
        let db = get_db_connection().await?;
        Ok(AppSub::find_by_id(id)
            .into_partial_model::<AppSubInfo>()
            .one(&db)
            .await?)
    }

    /// Find all northward subs (as info projection)
    pub async fn find_all() -> StorageResult<Vec<AppSubInfo>> {
        let db = get_db_connection().await?;
        Ok(AppSub::find()
            .order_by_asc(AppSubColumn::Id)
            .into_partial_model::<AppSubInfo>()
            .all(&db)
            .await?)
    }

    /// Check if northward sub exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(AppSub::find_by_id(id).count(&db).await? > 0)
    }

    /// Find northward subs by northward app ID
    pub async fn find_by_app_id(app_id: i32) -> StorageResult<Option<AppSubModel>> {
        let db = get_db_connection().await?;
        Ok(AppSub::find()
            .filter(AppSubColumn::AppId.eq(app_id))
            .order_by_asc(AppSubColumn::Id)
            .one(&db)
            .await?)
    }

    /// Find northward subs by IDs
    pub async fn find_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<AppSubModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(AppSub::find()
                .filter(AppSubColumn::Id.is_in(ids))
                .all(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(AppSub::find()
                    .filter(AppSubColumn::Id.is_in(ids))
                    .all(&conn)
                    .await?)
            }
        }
    }

    /// Batch create northward subs
    pub async fn create_many<C>(
        subs: Vec<AppSubActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<Vec<AppSubModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let mut results = Vec::new();
                for sub in subs {
                    results.push(sub.insert(conn).await?);
                }
                Ok(results)
            }
            None => {
                let conn = get_db_connection().await?;
                let mut results = Vec::new();
                for sub in subs {
                    results.push(sub.insert(&conn).await?);
                }
                Ok(results)
            }
        }
    }

    /// Batch delete northward subs by IDs and return deleted models
    pub async fn delete_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<AppSubModel>>
    where
        C: ConnectionTrait,
    {
        // First fetch the models to return
        let models = match db {
            Some(conn) => {
                AppSub::find()
                    .filter(AppSubColumn::Id.is_in(ids.clone()))
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppSub::find()
                    .filter(AppSubColumn::Id.is_in(ids.clone()))
                    .all(&conn)
                    .await?
            }
        };

        // Then delete them
        match db {
            Some(conn) => {
                for id in ids {
                    AppSub::delete_by_id(id).exec(conn).await?;
                }
            }
            None => {
                let conn = get_db_connection().await?;
                for id in ids {
                    AppSub::delete_by_id(id).exec(&conn).await?;
                }
            }
        }

        Ok(models)
    }
}
