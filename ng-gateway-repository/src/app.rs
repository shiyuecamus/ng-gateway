use crate::get_db_connection;
use ng_gateway_error::{storage::StorageError, StorageResult};
use ng_gateway_models::{
    domain::prelude::{AppInfo, AppPageParams, PageResult},
    entities::prelude::{
        App, AppActiveModel, AppColumn, AppModel, AppSub, AppSubColumn, AppSubModel, Plugin,
    },
};
use sea_orm::{
    prelude::Expr, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait, TransactionError,
    TransactionTrait,
};

/// Repository for northward app operations
pub struct AppRepository;

impl AppRepository {
    /// Create new northward app
    pub async fn create<C>(app: AppActiveModel, db: Option<&C>) -> StorageResult<AppModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(app.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(app.insert(&conn).await?)
            }
        }
    }

    /// Update existing northward app
    pub async fn update<C>(app: AppActiveModel, db: Option<&C>) -> StorageResult<AppModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(app.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(app.update(&conn).await?)
            }
        }
    }

    /// Delete northward app by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                App::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                App::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Delete northward app and all its subscriptions in a single transaction
    pub async fn delete_deep(id: i32) -> StorageResult<()> {
        let conn = get_db_connection().await?;
        conn.transaction::<_, _, StorageError>(|txn| {
            Box::pin(async move {
                AppSub::delete_many()
                    .filter(AppSubColumn::AppId.eq(id))
                    .exec(txn)
                    .await?;

                App::delete_by_id(id).exec(txn).await?;

                Ok(())
            })
        })
        .await
        .map_err(|e| match e {
            TransactionError::Connection(db_err) => StorageError::from(db_err),
            TransactionError::Transaction(err) => err,
        })?;
        Ok(())
    }

    /// Paginate northward apps with optional filters
    pub async fn page(params: AppPageParams) -> StorageResult<PageResult<AppInfo>> {
        let db = get_db_connection().await?;

        let base = App::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(AppColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.plugin_id.as_ref(), |q, plugin_id| {
                q.filter(AppColumn::PluginId.eq(*plugin_id))
            })
            .apply_if(params.status, |q, status| {
                q.filter(AppColumn::Status.eq(status))
            })
            .apply_if(params.time_range.start_time, |q, start_time| {
                q.filter(AppColumn::CreatedAt.gte(start_time.naive_utc()))
            })
            .apply_if(params.time_range.end_time, |q, end_time| {
                q.filter(AppColumn::CreatedAt.lte(end_time.naive_utc()))
            })
            .order_by(AppColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = base.clone().count(&db).await?;

        let select = base
            .left_join(Plugin)
            .select_only()
            .column_as(AppColumn::Id, "id")
            .column_as(AppColumn::Name, "name")
            .column_as(AppColumn::PluginId, "plugin_id")
            .expr_as(
                Expr::cust("plugin.plugin_type || ':' || plugin.version"),
                "plugin_type",
            )
            .column_as(AppColumn::Description, "description")
            .column_as(AppColumn::Config, "config")
            .column_as(AppColumn::RetryPolicy, "retry_policy")
            .column_as(AppColumn::QueuePolicy, "queue_policy")
            .column_as(AppColumn::Status, "status")
            .column_as(AppColumn::CreatedAt, "created_at")
            .column_as(AppColumn::UpdatedAt, "updated_at");

        let records = select
            .into_model::<AppInfo>()
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

    /// Find northward app by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<AppModel>> {
        let db = get_db_connection().await?;
        Ok(App::find_by_id(id).one(&db).await?)
    }

    /// Find northward app info by ID (partial projection)
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<AppInfo>> {
        let db = get_db_connection().await?;
        Ok(App::find_by_id(id)
            .left_join(Plugin)
            .select_only()
            .column_as(AppColumn::Id, "id")
            .column_as(AppColumn::Name, "name")
            .column_as(AppColumn::PluginId, "plugin_id")
            .expr_as(
                Expr::cust("plugin.plugin_type || ':' || plugin.version"),
                "plugin_type",
            )
            .column_as(AppColumn::Description, "description")
            .column_as(AppColumn::Config, "config")
            .column_as(AppColumn::RetryPolicy, "retry_policy")
            .column_as(AppColumn::QueuePolicy, "queue_policy")
            .column_as(AppColumn::Status, "status")
            .column_as(AppColumn::CreatedAt, "created_at")
            .column_as(AppColumn::UpdatedAt, "updated_at")
            .into_model::<AppInfo>()
            .one(&db)
            .await?)
    }

    /// Find all northward apps (as info projection)
    pub async fn find_all() -> StorageResult<Vec<AppInfo>> {
        let db = get_db_connection().await?;
        Ok(App::find()
            .left_join(Plugin)
            .select_only()
            .column_as(AppColumn::Id, "id")
            .column_as(AppColumn::Name, "name")
            .column_as(AppColumn::PluginId, "plugin_id")
            .expr_as(
                Expr::cust("plugin.plugin_type || ':' || plugin.version"),
                "plugin_type",
            )
            .column_as(AppColumn::Description, "description")
            .column_as(AppColumn::Config, "config")
            .column_as(AppColumn::RetryPolicy, "retry_policy")
            .column_as(AppColumn::QueuePolicy, "queue_policy")
            .column_as(AppColumn::Status, "status")
            .column_as(AppColumn::CreatedAt, "created_at")
            .column_as(AppColumn::UpdatedAt, "updated_at")
            .into_model::<AppInfo>()
            .all(&db)
            .await?)
    }

    pub async fn find_with_sub<C>(
        db: Option<&C>,
    ) -> StorageResult<Vec<(AppModel, Option<AppSubModel>)>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(App::find().find_also_related(AppSub).all(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(App::find().find_also_related(AppSub).all(&conn).await?)
            }
        }
    }

    /// Check if northward app exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(App::find_by_id(id).count(&db).await? > 0)
    }

    /// Find northward apps by plugin ID
    pub async fn find_by_plugin_id(plugin_id: i32) -> StorageResult<Vec<AppModel>> {
        let db = get_db_connection().await?;
        Ok(App::find()
            .filter(AppColumn::PluginId.eq(plugin_id))
            .order_by_asc(AppColumn::Id)
            .all(&db)
            .await?)
    }

    pub async fn exists_by_plugin_id(plugin_id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(App::find()
            .filter(AppColumn::PluginId.eq(plugin_id))
            .count(&db)
            .await?
            > 0)
    }
}
