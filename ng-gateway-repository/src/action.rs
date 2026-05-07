use crate::get_db_connection;
use crate::sort::{apply_sort_with_tiebreaker, effective_order};
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{ActionInfo, ActionPageParams, PageResult, SortParams},
    entities::{
        action::{
            ActiveModel as ActionActiveModel, Column as ActionColumn, Entity as Action,
            Model as ActionModel,
        },
        prelude::{Device, DeviceColumn},
    },
};
use sea_orm::{
    prelude::Expr, sea_query::Query, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend,
    EntityTrait, Order, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};
use std::mem;

/// Resolve `sortBy` field name to an action column.
fn resolve_action_sort_column(sort_by: &str) -> Option<ActionColumn> {
    match sort_by {
        "name" => Some(ActionColumn::Name),
        "id" => Some(ActionColumn::Id),
        _ => None,
    }
}

/// Repository for action operations
pub struct ActionRepository;

impl ActionRepository {
    /// Maximum batch size for action insertion on SQLite.
    ///
    /// # Why
    /// SQLite limits the number of bound variables per statement (commonly 999).
    /// `insert_many` expands to a single multi-row INSERT, which can exceed the limit
    /// for larger imports.
    const SQLITE_INSERT_BATCH_ROWS: usize = 100;

    /// Maximum batch size for action insertion on non-SQLite backends.
    const DEFAULT_INSERT_BATCH_ROWS: usize = 1000;

    /// Maximum `IN (...)` list size for SQLite queries.
    const SQLITE_IN_LIST_BATCH: usize = 900;

    /// Maximum `IN (...)` list size for non-SQLite backends.
    const DEFAULT_IN_LIST_BATCH: usize = 5000;

    #[inline]
    fn insert_batch_rows_for_backend(backend: DbBackend) -> usize {
        match backend {
            DbBackend::Sqlite => Self::SQLITE_INSERT_BATCH_ROWS,
            _ => Self::DEFAULT_INSERT_BATCH_ROWS,
        }
    }

    #[inline]
    fn in_list_batch_for_backend(backend: DbBackend) -> usize {
        match backend {
            DbBackend::Sqlite => Self::SQLITE_IN_LIST_BATCH,
            _ => Self::DEFAULT_IN_LIST_BATCH,
        }
    }

    /// Insert action rows in batches to avoid exceeding backend SQL variable limits.
    async fn insert_many_in_batches<C>(
        conn: &C,
        actions: Vec<ActionActiveModel>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        let batch_rows = Self::insert_batch_rows_for_backend(conn.get_database_backend()).max(1);

        let mut created: Vec<ActionModel> = Vec::with_capacity(actions.len());
        let mut batch: Vec<ActionActiveModel> = Vec::with_capacity(batch_rows);

        for a in actions.into_iter() {
            batch.push(a);
            if batch.len() >= batch_rows {
                let to_insert = mem::take(&mut batch);
                let inserted = Action::insert_many(to_insert)
                    .exec_with_returning_many(conn)
                    .await?;
                created.extend(inserted);
            }
        }

        if !batch.is_empty() {
            let inserted = Action::insert_many(batch)
                .exec_with_returning_many(conn)
                .await?;
            created.extend(inserted);
        }

        Ok(created)
    }

    /// Delete action rows by ids in batches to avoid exceeding backend SQL variable limits.
    async fn delete_by_ids_in_batches<C>(conn: &C, ids: Vec<i32>) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        let batch_size = Self::in_list_batch_for_backend(conn.get_database_backend()).max(1);
        let mut deleted: Vec<ActionModel> = Vec::with_capacity(ids.len());

        for chunk in ids.chunks(batch_size) {
            let removed = Action::delete_many()
                .filter(ActionColumn::Id.is_in(chunk.to_vec()))
                .exec_with_returning(conn)
                .await?;
            deleted.extend(removed);
        }

        Ok(deleted)
    }

    /// Create new action
    pub async fn create<C>(action: ActionActiveModel, db: Option<&C>) -> StorageResult<ActionModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(action.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(action.insert(&conn).await?)
            }
        }
    }

    /// Create new actions (batch)
    pub async fn create_many<C>(
        actions: Vec<ActionActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        if actions.is_empty() {
            return Ok(Vec::new());
        }

        match db {
            Some(conn) => Self::insert_many_in_batches(conn, actions).await,
            None => {
                let conn = get_db_connection().await?;
                Self::insert_many_in_batches(&conn, actions).await
            }
        }
    }

    /// Update existing action
    pub async fn update<C>(action: ActionActiveModel, db: Option<&C>) -> StorageResult<ActionModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(action.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(action.update(&conn).await?)
            }
        }
    }

    /// Delete action by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Action::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Action::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Delete actions by IDs and return deleted rows
    pub async fn delete_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        match db {
            Some(conn) => Self::delete_by_ids_in_batches(conn, ids).await,
            None => {
                let conn = get_db_connection().await?;
                Self::delete_by_ids_in_batches(&conn, ids).await
            }
        }
    }

    /// Find action by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find_by_id(id).one(&conn).await?)
    }

    /// Find action info by ID
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find_by_id(id)
            .into_partial_model::<ActionInfo>()
            .one(&conn)
            .await?)
    }

    /// Find actions by IDs
    pub async fn find_by_ids(ids: Vec<i32>) -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::Id.is_in(ids))
            .all(&conn)
            .await?)
    }

    /// Find actions by IDs (read-only info)
    pub async fn find_info_by_ids(ids: Vec<i32>) -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::Id.is_in(ids))
            .into_partial_model::<ActionInfo>()
            .all(&conn)
            .await?)
    }

    /// Find all actions
    pub async fn find_all() -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .order_by_asc(ActionColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find all actions (read-only info)
    pub async fn find_all_info() -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .order_by_asc(ActionColumn::Id)
            .into_partial_model::<ActionInfo>()
            .all(&conn)
            .await?)
    }

    /// Find actions by device ID
    pub async fn find_by_device_id(device_id: i32) -> StorageResult<Vec<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .order_by_asc(ActionColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find action IDs by device ID (lightweight).
    pub async fn find_ids_by_device_id(device_id: i32) -> StorageResult<Vec<i32>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .select_only()
            .column(ActionColumn::Id)
            .filter(ActionColumn::DeviceId.eq(device_id))
            .order_by_asc(ActionColumn::Id)
            .into_tuple::<i32>()
            .all(&conn)
            .await?)
    }

    /// Find action infos by device ID with optional sort (defaults to name asc).
    pub async fn find_info_by_device_id(
        device_id: i32,
        sort: Option<&SortParams>,
    ) -> StorageResult<Vec<ActionInfo>> {
        let conn = get_db_connection().await?;
        let base = Action::find().filter(ActionColumn::DeviceId.eq(device_id));

        let sorted = if let Some(col) = sort
            .and_then(|s| s.sort_by.as_deref())
            .and_then(resolve_action_sort_column)
        {
            let order = effective_order(sort.unwrap(), Order::Asc);
            apply_sort_with_tiebreaker(base, col, order, ActionColumn::Id)
        } else {
            apply_sort_with_tiebreaker(base, ActionColumn::Name, Order::Asc, ActionColumn::Id)
        };

        Ok(sorted.into_partial_model::<ActionInfo>().all(&conn).await?)
    }

    /// Find actions by device IDs
    pub async fn find_by_device_ids<C>(
        device_ids: Vec<i32>,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        let actions = match db {
            Some(conn) => {
                Action::find()
                    .filter(ActionColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(ActionColumn::Id)
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                Action::find()
                    .filter(ActionColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(ActionColumn::Id)
                    .all(&conn)
                    .await?
            }
        };
        Ok(actions)
    }

    /// Find action by device ID and name
    pub async fn find_by_device_and_name(
        device_id: i32,
        name: &str,
    ) -> StorageResult<Option<ActionModel>> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Name.eq(name))
            .one(&conn)
            .await?)
    }

    /// Delete all actions by device ID
    pub async fn delete_by_device_id<C>(
        device_id: i32,
        db: Option<&C>,
    ) -> StorageResult<Vec<ActionModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Action::delete_many()
                .filter(ActionColumn::DeviceId.eq(device_id))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Action::delete_many()
                    .filter(ActionColumn::DeviceId.eq(device_id))
                    .exec_with_returning(&conn)
                    .await?)
            }
        }
    }

    /// Count actions by device ID
    pub async fn count_by_device_id(device_id: i32) -> StorageResult<u64> {
        let conn = get_db_connection().await?;
        Ok(Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .count(&conn)
            .await?)
    }

    /// Check if action exists by ID
    pub async fn exists(id: i32) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find_by_id(id).count(&conn).await?;
        Ok(count > 0)
    }

    /// Check if action exists by device ID and command
    pub async fn exists_by_device_and_command(
        device_id: i32,
        command: &str,
    ) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find()
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Command.eq(command))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Check if action exists by device ID and command excluding the given ID
    pub async fn exists_by_device_and_command_exclude_id(
        id: i32,
        device_id: i32,
        command: &str,
    ) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Action::find()
            .filter(ActionColumn::Id.ne(id))
            .filter(ActionColumn::DeviceId.eq(device_id))
            .filter(ActionColumn::Command.eq(command))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Delete all actions by channel ID using subquery
    pub async fn delete_by_channel_id<C>(channel_id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Action::delete_many()
                    .filter(
                        ActionColumn::DeviceId.in_subquery(
                            Query::select()
                                .column(DeviceColumn::Id)
                                .from(Device)
                                .and_where(Expr::col(DeviceColumn::ChannelId).eq(channel_id))
                                .to_owned(),
                        ),
                    )
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Action::delete_many()
                    .filter(
                        ActionColumn::DeviceId.in_subquery(
                            Query::select()
                                .column(DeviceColumn::Id)
                                .from(Device)
                                .and_where(Expr::col(DeviceColumn::ChannelId).eq(channel_id))
                                .to_owned(),
                        ),
                    )
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Page query for actions
    pub async fn page(params: ActionPageParams) -> StorageResult<PageResult<ActionInfo>> {
        let db = get_db_connection().await?;

        let filtered = Action::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(ActionColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.command.as_ref(), |q, command| {
                q.filter(ActionColumn::Command.eq(command))
            })
            .apply_if(params.device_id, |q, device_id| {
                q.filter(ActionColumn::DeviceId.eq(device_id))
            });

        let query = if let Some(col) = params
            .sort
            .sort_by
            .as_deref()
            .and_then(resolve_action_sort_column)
        {
            let order = effective_order(&params.sort, Order::Asc);
            apply_sort_with_tiebreaker(filtered, col, order, ActionColumn::Id)
        } else {
            filtered.order_by(ActionColumn::Id, Order::Desc)
        };

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<ActionInfo>()
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
}
