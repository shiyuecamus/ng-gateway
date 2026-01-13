use crate::get_db_connection;
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
    prelude::Expr, sea_query::Query, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait,
    Order, PaginatorTrait, QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for point operations
pub struct PointRepository;

impl PointRepository {
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
        match db {
            Some(conn) => Ok(Point::insert_many(points)
                .exec_with_returning_many(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Point::insert_many(points)
                    .exec_with_returning_many(&conn)
                    .await?)
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
        match db {
            Some(conn) => Ok(Point::delete_many()
                .filter(PointColumn::Id.is_in(ids))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Point::delete_many()
                    .filter(PointColumn::Id.is_in(ids))
                    .exec_with_returning(&conn)
                    .await?)
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

        let query = Point::find()
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
            })
            .order_by(PointColumn::Id, Order::Desc);

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
