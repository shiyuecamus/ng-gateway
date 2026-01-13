use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{DeviceInfo, DevicePageParams, PageResult},
    entities::device::{
        ActiveModel as DeviceActiveModel, Column as DeviceColumn, Entity as Device,
        Model as DeviceModel,
    },
    enums::common::Status,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for device operations
pub struct DeviceRepository;

impl DeviceRepository {
    /// Create many devices and return inserted models
    pub async fn create_many<C>(
        devices: Vec<DeviceActiveModel>,
        db: Option<&C>,
    ) -> StorageResult<Vec<DeviceModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Device::insert_many(devices)
                .exec_with_returning_many(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Device::insert_many(devices)
                    .exec_with_returning_many(&conn)
                    .await?)
            }
        }
    }

    /// Find devices by IDs
    pub async fn find_by_ids(ids: Vec<i32>) -> StorageResult<Vec<DeviceModel>> {
        let conn = get_db_connection().await?;
        Ok(Device::find()
            .filter(DeviceColumn::Id.is_in(ids))
            .all(&conn)
            .await?)
    }

    /// Delete devices by IDs and return deleted rows
    pub async fn delete_by_ids<C>(ids: Vec<i32>, db: Option<&C>) -> StorageResult<Vec<DeviceModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Device::delete_many()
                .filter(DeviceColumn::Id.is_in(ids))
                .exec_with_returning(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Device::delete_many()
                    .filter(DeviceColumn::Id.is_in(ids))
                    .exec_with_returning(&conn)
                    .await?)
            }
        }
    }

    /// Create new device
    pub async fn create<C>(device: DeviceActiveModel, db: Option<&C>) -> StorageResult<DeviceModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(device.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(device.insert(&conn).await?)
            }
        }
    }

    /// Update existing device
    pub async fn update<C>(device: DeviceActiveModel, db: Option<&C>) -> StorageResult<DeviceModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(device.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(device.update(&conn).await?)
            }
        }
    }

    /// Delete device by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Device::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Device::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    pub async fn page(params: DevicePageParams) -> StorageResult<PageResult<DeviceInfo>> {
        let db = get_db_connection().await?;

        let query = Device::find()
            .apply_if(params.device_name.as_ref(), |q, device_name| {
                q.filter(DeviceColumn::DeviceName.like(format!("%{device_name}%")))
            })
            .apply_if(params.device_type.as_ref(), |q, device_type| {
                q.filter(DeviceColumn::DeviceType.like(format!("%{device_type}%")))
            })
            .apply_if(params.channel_id, |q, channel_id| {
                q.filter(DeviceColumn::ChannelId.eq(channel_id))
            })
            .apply_if(params.status, |q, status| {
                q.filter(DeviceColumn::Status.eq(status))
            })
            .order_by(DeviceColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let paginator = query
            .into_partial_model::<DeviceInfo>()
            .paginate(&db, page_size as u64)
            .fetch_page((page - 1) as u64)
            .await?;

        Ok(PageResult {
            records: paginator,
            total,
            pages: ((total as f64) / (page_size as f64)).ceil() as u32,
            page,
            page_size,
        })
    }

    /// Check if a device exists by name
    pub async fn exists_by_name(device_name: String) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Device::find()
            .filter(DeviceColumn::DeviceName.eq(device_name))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Check if a device exists by name excluding the given ID (for update)
    pub async fn exists_by_name_exclude_id(id: i32, device_name: String) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Device::find()
            .filter(DeviceColumn::Id.ne(id))
            .filter(DeviceColumn::DeviceName.eq(device_name))
            .count(&conn)
            .await?;
        Ok(count > 0)
    }

    /// Find device by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<DeviceModel>> {
        let conn = get_db_connection().await?;
        let device = Device::find_by_id(id).one(&conn).await?;
        Ok(device)
    }

    /// Find device info by ID
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<DeviceInfo>> {
        let conn = get_db_connection().await?;
        let device = Device::find_by_id(id)
            .into_partial_model::<DeviceInfo>()
            .one(&conn)
            .await?;
        Ok(device)
    }

    /// Find all devices
    pub async fn find_all() -> StorageResult<Vec<DeviceInfo>> {
        let conn = get_db_connection().await?;
        let devices = Device::find()
            .into_partial_model::<DeviceInfo>()
            .all(&conn)
            .await?;
        Ok(devices)
    }

    /// Find devices by channel ID
    pub async fn find_by_channel_id(channel_id: i32) -> StorageResult<Vec<DeviceInfo>> {
        let conn = get_db_connection().await?;
        Ok(Device::find()
            .filter(DeviceColumn::ChannelId.eq(channel_id))
            .order_by_asc(DeviceColumn::Id)
            .into_partial_model::<DeviceInfo>()
            .all(&conn)
            .await?)
    }

    /// Delete all devices by channel ID
    pub async fn delete_by_channel_id<C>(channel_id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Device::delete_many()
                    .filter(DeviceColumn::ChannelId.eq(channel_id))
                    .exec(conn)
                    .await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Device::delete_many()
                    .filter(DeviceColumn::ChannelId.eq(channel_id))
                    .exec(&conn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Count devices by channel ID
    pub async fn count_by_channel_id<C>(channel_id: i32, db: Option<&C>) -> StorageResult<u64>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let count = Device::find()
                    .filter(DeviceColumn::ChannelId.eq(channel_id))
                    .count(conn)
                    .await?;
                Ok(count)
            }
            None => {
                let conn = get_db_connection().await?;
                let count = Device::find()
                    .filter(DeviceColumn::ChannelId.eq(channel_id))
                    .count(&conn)
                    .await?;
                Ok(count)
            }
        }
    }

    /// Count enabled devices
    pub async fn count_by_status<C>(status: Status, db: Option<&C>) -> StorageResult<u64>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                let count = Device::find()
                    .filter(DeviceColumn::Status.eq(status))
                    .count(conn)
                    .await?;
                Ok(count)
            }
            None => {
                let conn = get_db_connection().await?;
                let count = Device::find()
                    .filter(DeviceColumn::Status.eq(status))
                    .count(&conn)
                    .await?;
                Ok(count)
            }
        }
    }

    /// Check if device exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let conn = get_db_connection().await?;
        let count = Device::find_by_id(id).count(&conn).await?;
        Ok(count > 0)
    }
}
