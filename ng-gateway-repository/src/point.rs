use crate::get_db_connection;
use crate::sort::{apply_sort_with_tiebreaker, effective_order};
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{PageResult, PointInfo, PointPageParams},
    entities::{
        point::{
            ActiveModel as PointActiveModel, Column as PointColumn, Entity as Point,
            Model as PointModel,
        },
        prelude::{Device, DeviceColumn},
    },
    enums::common::{AccessMode, DataPointType, DataType},
};
use sea_orm::{
    prelude::Expr, sea_query::Query, ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend,
    EntityTrait, Order, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};
use std::mem;

/// Resolve `sortBy` field name to a point column.
fn resolve_point_sort_column(sort_by: &str) -> Option<PointColumn> {
    match sort_by {
        "name" => Some(PointColumn::Name),
        "id" => Some(PointColumn::Id),
        _ => None,
    }
}

/// Repository for point operations
pub struct PointRepository;

impl PointRepository {
    /// Maximum batch size for point insertion on SQLite.
    ///
    /// # Why
    /// SQLite has a hard limit on the number of bound variables per SQL statement.
    /// Many distributions compile SQLite with `SQLITE_MAX_VARIABLE_NUMBER = 999`.
    ///
    /// A point insert touches ~14-15 columns, therefore inserting thousands of points in
    /// a single `INSERT ... VALUES (...), (...), ...` statement will fail with:
    /// `too many SQL variables`.
    ///
    /// We keep this value conservative to remain compatible across SQLite builds and
    /// across schema evolution.
    const SQLITE_INSERT_BATCH_ROWS: usize = 100;

    /// Maximum batch size for point insertion on non-SQLite backends.
    ///
    /// This is primarily a safety valve to avoid generating extremely large SQL statements.
    const DEFAULT_INSERT_BATCH_ROWS: usize = 1000;

    /// Maximum `IN (...)` list size for SQLite queries.
    ///
    /// # Why
    /// SQLite limits the number of bound variables per statement (commonly 999).
    /// Queries like `... WHERE id IN (?, ?, ...)` will hit that limit when the list is large.
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

    /// Insert point rows in batches to avoid exceeding backend SQL variable limits.
    ///
    /// # Notes
    /// - On SQLite, `insert_many` can easily exceed `SQLITE_MAX_VARIABLE_NUMBER` (commonly 999).
    /// - We intentionally do not wrap this into a long transaction because the gateway layer
    ///   already implements compensation logic, and smaller statements reduce lock contention.
    async fn insert_many_in_batches<C>(
        conn: &C,
        points: Vec<PointActiveModel>,
    ) -> StorageResult<Vec<PointModel>>
    where
        C: ConnectionTrait,
    {
        let batch_rows = Self::insert_batch_rows_for_backend(conn.get_database_backend()).max(1);
        let mut created: Vec<PointModel> = Vec::with_capacity(points.len());
        let mut batch: Vec<PointActiveModel> = Vec::with_capacity(batch_rows);

        for p in points.into_iter() {
            batch.push(p);
            if batch.len() >= batch_rows {
                let to_insert = mem::take(&mut batch);
                let inserted = Point::insert_many(to_insert)
                    .exec_with_returning_many(conn)
                    .await?;
                created.extend(inserted);
            }
        }

        if !batch.is_empty() {
            let inserted = Point::insert_many(batch)
                .exec_with_returning_many(conn)
                .await?;
            created.extend(inserted);
        }

        Ok(created)
    }

    /// Delete point rows by ids in batches to avoid exceeding backend SQL variable limits.
    async fn delete_by_ids_in_batches<C>(conn: &C, ids: Vec<i32>) -> StorageResult<Vec<PointModel>>
    where
        C: ConnectionTrait,
    {
        let batch_size = Self::in_list_batch_for_backend(conn.get_database_backend()).max(1);
        let mut deleted: Vec<PointModel> = Vec::with_capacity(ids.len());

        for chunk in ids.chunks(batch_size) {
            let removed = Point::delete_many()
                .filter(PointColumn::Id.is_in(chunk.to_vec()))
                .exec_with_returning(conn)
                .await?;
            deleted.extend(removed);
        }

        Ok(deleted)
    }

    /// Create new point
    pub async fn create<C>(point: PointActiveModel, db: Option<&C>) -> StorageResult<PointModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(point.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(point.insert(&conn).await?)
            }
        }
    }

    /// Create new points
    pub async fn create_many<C>(
        points: Vec<PointActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<Vec<PointModel>>
    where
        C: ConnectionTrait,
    {
        if points.is_empty() {
            return Ok(Vec::new());
        }

        match db {
            Some(conn) => Self::insert_many_in_batches(conn, points).await,
            None => {
                let conn = get_db_connection().await?;
                Self::insert_many_in_batches(&conn, points).await
            }
        }
    }

    /// Update existing point
    pub async fn update<C>(point: PointActiveModel, db: Option<&C>) -> StorageResult<PointModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(point.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(point.update(&conn).await?)
            }
        }
    }

    /// Delete point by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Point::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Point::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Delete points by IDs
    pub async fn delete_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<PointModel>>
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

    /// Find point by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find_by_id(id).one(&conn).await?)
    }

    /// Find point info by ID
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<PointInfo>> {
        let conn = get_db_connection().await?;
        Ok(Point::find_by_id(id)
            .into_partial_model::<PointInfo>()
            .one(&conn)
            .await?)
    }

    /// Find points by IDs
    pub async fn find_by_ids(ids: Vec<i32>) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::Id.is_in(ids))
            .all(&conn)
            .await?)
    }

    /// Find points by IDs (read-only info)
    pub async fn find_info_by_ids(ids: Vec<i32>) -> StorageResult<Vec<PointInfo>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::Id.is_in(ids))
            .into_partial_model::<PointInfo>()
            .all(&conn)
            .await?)
    }

    /// Find all points
    pub async fn find_all() -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find all points (read-only info)
    pub async fn find_all_info() -> StorageResult<Vec<PointInfo>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .order_by_asc(PointColumn::Id)
            .into_partial_model::<PointInfo>()
            .all(&conn)
            .await?)
    }

    /// Find points by device ID
    pub async fn find_by_device_id(device_id: i32) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find point IDs by device ID (lightweight).
    ///
    /// This is intended for bulk operations like "clear points", where we only need IDs
    /// and want to avoid loading full models (potentially large columns).
    pub async fn find_ids_by_device_id(device_id: i32) -> StorageResult<Vec<i32>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .select_only()
            .column(PointColumn::Id)
            .filter(PointColumn::DeviceId.eq(device_id))
            .order_by_asc(PointColumn::Id)
            .into_tuple::<i32>()
            .all(&conn)
            .await?)
    }

    /// Find points by device IDs
    pub async fn find_by_device_ids<C>(
        device_ids: Vec<i32>,
        db: Option<&C>,
    ) -> StorageResult<Vec<PointModel>>
    where
        C: ConnectionTrait,
    {
        let points = match db {
            Some(conn) => {
                Point::find()
                    .filter(PointColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(PointColumn::Id)
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                Point::find()
                    .filter(PointColumn::DeviceId.is_in(device_ids))
                    .order_by_asc(PointColumn::Id)
                    .all(&conn)
                    .await?
            }
        };
        Ok(points)
    }

    /// Find points by data point type
    pub async fn find_by_type(point_type: DataPointType) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::Type.eq(point_type))
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find points by data type
    pub async fn find_by_data_type(data_type: DataType) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::DataType.eq(data_type))
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find points by access mode
    pub async fn find_by_access_mode(access_mode: AccessMode) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::AccessMode.eq(access_mode))
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find readable points by device ID
    pub async fn find_readable_by_device_id(device_id: i32) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .filter(
                PointColumn::AccessMode
                    .eq(AccessMode::Read)
                    .or(PointColumn::AccessMode.eq(AccessMode::ReadWrite)),
            )
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find writable points by device ID
    pub async fn find_writable_by_device_id(device_id: i32) -> StorageResult<Vec<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .filter(
                PointColumn::AccessMode
                    .eq(AccessMode::Write)
                    .or(PointColumn::AccessMode.eq(AccessMode::ReadWrite)),
            )
            .order_by_asc(PointColumn::Id)
            .all(&conn)
            .await?)
    }

    /// Find point by device ID and name
    pub async fn find_by_device_and_name(
        device_id: i32,
        name: &str,
    ) -> StorageResult<Option<PointModel>> {
        let conn = get_db_connection().await?;
        Ok(Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .filter(PointColumn::Name.eq(name))
            .one(&conn)
            .await?)
    }

    /// Delete all points by device ID
    pub async fn delete_by_device_id<C>(
        device_id: i32,
        db: Option<&C>,
    ) -> StorageResult<Vec<PointModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Point::delete_many()
                .filter(PointColumn::DeviceId.eq(device_id))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Point::delete_many()
                    .filter(PointColumn::DeviceId.eq(device_id))
                    .exec_with_returning(&conn)
                    .await?)
            }
        }
    }

    /// Count points by device ID
    pub async fn count_by_device_id(device_id: i32) -> StorageResult<u64> {
        let conn = get_db_connection().await?;
        let count = Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .count(&conn)
            .await?;
        Ok(count)
    }

    /// Count points by type
    pub async fn count_by_type(point_type: DataPointType) -> StorageResult<u64> {
        let conn = get_db_connection().await?;
        let count = Point::find()
            .filter(PointColumn::Type.eq(point_type))
            .count(&conn)
            .await?;
        Ok(count)
    }

    /// Check if point exists by ID
    pub async fn exists(id: i32) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Point::find_by_id(id).count(&conn).await?;
        Ok(count > 0)
    }

    /// Check if point exists by device ID and key
    pub async fn exists_by_device_and_key(device_id: i32, key: &str) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Point::find()
            .filter(PointColumn::DeviceId.eq(device_id))
            .filter(PointColumn::Key.eq(key))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Check if point exists by device ID and key excluding the given ID
    pub async fn exists_by_device_and_key_exclude_id(
        id: i32,
        device_id: i32,
        key: &str,
    ) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Point::find()
            .filter(PointColumn::Id.ne(id))
            .filter(PointColumn::DeviceId.eq(device_id))
            .filter(PointColumn::Key.eq(key))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Delete all points by channel ID using subquery
    pub async fn delete_by_channel_id<C>(channel_id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Point::delete_many()
                    .filter(
                        PointColumn::DeviceId.in_subquery(
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
                Point::delete_many()
                    .filter(
                        PointColumn::DeviceId.in_subquery(
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

    /// Page query for points
    pub async fn page(params: PointPageParams) -> StorageResult<PageResult<PointInfo>> {
        let db = get_db_connection().await?;

        let filtered = Point::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(PointColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.key.as_ref(), |q, key| {
                q.filter(PointColumn::Key.eq(key))
            })
            .apply_if(params.device_id, |q, device_id| {
                q.filter(PointColumn::DeviceId.eq(device_id))
            })
            .apply_if(params.r#type, |q, t| q.filter(PointColumn::Type.eq(t)))
            .apply_if(params.data_type, |q, dt| {
                q.filter(PointColumn::DataType.eq(dt))
            })
            .apply_if(params.access_mode, |q, am| {
                q.filter(PointColumn::AccessMode.eq(am))
            });

        let query = if let Some(col) = params
            .sort
            .sort_by
            .as_deref()
            .and_then(resolve_point_sort_column)
        {
            let order = effective_order(&params.sort, Order::Asc);
            apply_sort_with_tiebreaker(filtered, col, order, PointColumn::Id)
        } else {
            filtered.order_by(PointColumn::Id, Order::Desc)
        };

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<PointInfo>()
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
