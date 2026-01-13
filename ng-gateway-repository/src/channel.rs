use crate::get_db_connection;
use crate::{ActionRepository, DeviceRepository, PointRepository};
use ng_gateway_error::{storage::StorageError, StorageResult};
use ng_gateway_models::{
    domain::prelude::{ChannelInfo, ChannelPageParams, PageResult},
    entities::prelude::{
        Channel, ChannelActiveModel, ChannelColumn, ChannelModel, Device, DeviceModel, Driver,
    },
    enums::common::{CollectionType, ReportType, Status},
};
use sea_orm::{
    sea_query::Expr, ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, QueryTrait,
};
use sea_orm::{TransactionError, TransactionTrait};

/// Repository for channel operations
pub struct ChannelRepository;

impl ChannelRepository {
    /// Create new channel
    pub async fn create<C>(
        channel: ChannelActiveModel,
        db: Option<&C>,
    ) -> StorageResult<ChannelModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(channel.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(channel.insert(&conn).await?)
            }
        }
    }

    /// Update existing channel
    pub async fn update<C>(
        channel: ChannelActiveModel,
        db: Option<&C>,
    ) -> StorageResult<ChannelModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(channel.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(channel.update(&conn).await?)
            }
        }
    }

    /// Delete a channel and cascade delete its devices, points and actions in a single transaction
    pub async fn delete_deep(id: i32) -> StorageResult<()> {
        let conn = get_db_connection().await?;
        conn.transaction::<_, _, StorageError>(|txn| {
            Box::pin(async move {
                // 1) delete points by channel id (via device subquery)
                PointRepository::delete_by_channel_id(id, Some(txn)).await?;
                // 2) delete actions by channel id (via device subquery)
                ActionRepository::delete_by_channel_id(id, Some(txn)).await?;
                // 3) delete devices by channel id
                DeviceRepository::delete_by_channel_id(id, Some(txn)).await?;
                // 4) delete channel itself
                Channel::delete_by_id(id).exec(txn).await?;
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

    /// Delete channel by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Channel::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Channel::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    pub async fn page(params: ChannelPageParams) -> StorageResult<PageResult<ChannelInfo>> {
        let db = get_db_connection().await?;

        let base = Channel::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(ChannelColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.status, |q, status| {
                q.filter(ChannelColumn::Status.eq(status))
            })
            .order_by(ChannelColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = base.clone().count(&db).await?;

        let select = base
            .left_join(Driver)
            .select_only()
            .column_as(ChannelColumn::Id, "id")
            .column_as(ChannelColumn::Name, "name")
            .column_as(ChannelColumn::DriverId, "driver_id")
            .expr_as(
                Expr::cust("driver.driver_type || ':' || driver.version"),
                "driver_type",
            )
            .column_as(ChannelColumn::CollectionType, "collection_type")
            .column_as(ChannelColumn::Period, "period")
            .column_as(ChannelColumn::ReportType, "report_type")
            .column_as(ChannelColumn::Status, "status")
            .column_as(ChannelColumn::ConnectionPolicy, "connection_policy")
            .column_as(ChannelColumn::DriverConfig, "driver_config");

        let records = select
            .into_model::<ChannelInfo>()
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

    /// Find channel by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::find_by_id(id).one(&db).await?)
    }

    /// Find channel info by ID
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<ChannelInfo>> {
        let db = get_db_connection().await?;
        Ok(Channel::find_by_id(id)
            .left_join(Driver)
            .select_only()
            .column_as(ChannelColumn::Id, "id")
            .column_as(ChannelColumn::Name, "name")
            .column_as(ChannelColumn::DriverId, "driver_id")
            .expr_as(
                Expr::cust("driver.driver_type || ':' || driver.version"),
                "driver_type",
            )
            .column_as(ChannelColumn::CollectionType, "collection_type")
            .column_as(ChannelColumn::Period, "period")
            .column_as(ChannelColumn::ReportType, "report_type")
            .column_as(ChannelColumn::Status, "status")
            .column_as(ChannelColumn::ConnectionPolicy, "connection_policy")
            .column_as(ChannelColumn::DriverConfig, "driver_config")
            .into_model::<ChannelInfo>()
            .one(&db)
            .await?)
    }

    /// Find all channels
    pub async fn find_all() -> StorageResult<Vec<ChannelInfo>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .left_join(Driver)
            .select_only()
            .column_as(ChannelColumn::Id, "id")
            .column_as(ChannelColumn::Name, "name")
            .column_as(ChannelColumn::DriverId, "driver_id")
            .expr_as(
                Expr::cust("driver.driver_type || ':' || driver.version"),
                "driver_type",
            )
            .column_as(ChannelColumn::CollectionType, "collection_type")
            .column_as(ChannelColumn::Period, "period")
            .column_as(ChannelColumn::ReportType, "report_type")
            .column_as(ChannelColumn::Status, "status")
            .column_as(ChannelColumn::ConnectionPolicy, "connection_policy")
            .column_as(ChannelColumn::DriverConfig, "driver_config")
            .into_model::<ChannelInfo>()
            .all(&db)
            .await?)
    }

    /// Find channels by driver ID
    pub async fn find_by_driver_id(driver_id: i32) -> StorageResult<Vec<ChannelInfo>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::DriverId.eq(driver_id))
            .order_by_asc(ChannelColumn::Id)
            .left_join(Driver)
            .select_only()
            .column_as(ChannelColumn::Id, "id")
            .column_as(ChannelColumn::Name, "name")
            .column_as(ChannelColumn::DriverId, "driver_id")
            .expr_as(
                Expr::cust("driver.driver_type || ':' || driver.version"),
                "driver_type",
            )
            .column_as(ChannelColumn::CollectionType, "collection_type")
            .column_as(ChannelColumn::Period, "period")
            .column_as(ChannelColumn::ReportType, "report_type")
            .column_as(ChannelColumn::Status, "status")
            .column_as(ChannelColumn::ConnectionPolicy, "connection_policy")
            .column_as(ChannelColumn::DriverConfig, "driver_config")
            .into_model::<ChannelInfo>()
            .all(&db)
            .await?)
    }

    pub async fn find_with_devices<C>(
        db: Option<&C>,
    ) -> StorageResult<Vec<(ChannelModel, Vec<DeviceModel>)>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Channel::find().find_with_related(Device).all(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Channel::find().find_with_related(Device).all(&conn).await?)
            }
        }
    }

    /// Find enabled channels
    pub async fn find_by_status(status: Status) -> StorageResult<Vec<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::Status.eq(status))
            .all(&db)
            .await?)
    }

    /// Find channels by collection type
    pub async fn find_by_collection_type(
        collection_type: CollectionType,
    ) -> StorageResult<Vec<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::CollectionType.eq(collection_type))
            .order_by_asc(ChannelColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find channels by report type
    pub async fn find_by_report_type(report_type: ReportType) -> StorageResult<Vec<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::ReportType.eq(report_type))
            .order_by_asc(ChannelColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find channel by driver ID and name
    pub async fn find_by_driver_and_name(
        driver_id: i32,
        name: &str,
    ) -> StorageResult<Option<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::DriverId.eq(driver_id))
            .filter(ChannelColumn::Name.eq(name))
            .one(&db)
            .await?)
    }

    /// Delete all channels by driver ID
    pub async fn delete_by_driver_id(driver_id: i32) -> StorageResult<Vec<ChannelModel>> {
        let db = get_db_connection().await?;
        Ok(Channel::delete_many()
            .filter(ChannelColumn::DriverId.eq(driver_id))
            .exec_with_returning(&db)
            .await?)
    }

    /// Delete all channels and their nested resources by driver ID within a single transaction
    pub async fn delete_deep_by_driver_id(driver_id: i32) -> StorageResult<()> {
        let conn = get_db_connection().await?;
        conn.transaction::<_, _, StorageError>(|txn| {
            Box::pin(async move {
                // Collect channel ids first to ensure deterministic deletion order and to avoid streaming locks
                let channels: Vec<i32> = Channel::find()
                    .filter(ChannelColumn::DriverId.eq(driver_id))
                    .order_by_asc(ChannelColumn::Id)
                    .all(txn)
                    .await?
                    .into_iter()
                    .map(|c| c.id)
                    .collect();

                for id in channels.into_iter() {
                    // 1) delete points by channel id (via device subquery)
                    PointRepository::delete_by_channel_id(id, Some(txn)).await?;
                    // 2) delete actions by channel id (via device subquery)
                    ActionRepository::delete_by_channel_id(id, Some(txn)).await?;
                    // 3) delete devices by channel id
                    DeviceRepository::delete_by_channel_id(id, Some(txn)).await?;
                    // 4) delete channel itself
                    Channel::delete_by_id(id).exec(txn).await?;
                }

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

    /// Count channels by driver ID
    pub async fn count_by_driver_id(driver_id: i32) -> StorageResult<u64> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::DriverId.eq(driver_id))
            .count(&db)
            .await?)
    }

    /// Check if channel exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Channel::find_by_id(id).count(&db).await? > 0)
    }

    /// Check if channel exists by driver ID
    pub async fn exists_by_driver_id(driver_id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Channel::find()
            .filter(ChannelColumn::DriverId.eq(driver_id))
            .count(&db)
            .await?
            > 0)
    }
}
