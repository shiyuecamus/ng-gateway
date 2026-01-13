use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{DriverInfo, DriverPageParams, PageResult},
    entities::driver::{
        ActiveModel as DriverActiveModel, Column as DriverColumn, Entity as Driver,
        Model as DriverModel,
    },
    enums::common::{OsArch, OsType},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for driver operations
pub struct DriverRepository;

impl DriverRepository {
    /// Create new driver
    pub async fn create<C>(driver: DriverActiveModel, db: Option<&C>) -> StorageResult<DriverModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(driver.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(driver.insert(&conn).await?)
            }
        }
    }

    /// Update existing driver
    pub async fn update<C>(driver: DriverActiveModel, db: Option<&C>) -> StorageResult<DriverModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(driver.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(driver.update(&conn).await?)
            }
        }
    }

    /// Delete driver by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Driver::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Driver::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Paginate drivers with optional filters
    pub async fn page(params: DriverPageParams) -> StorageResult<PageResult<DriverInfo>> {
        let db = get_db_connection().await?;

        let query = Driver::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(DriverColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.driver_type.as_ref(), |q, driver_type| {
                q.filter(DriverColumn::DriverType.like(format!("%{driver_type}%")))
            })
            .apply_if(params.version.as_ref(), |q, version| {
                q.filter(DriverColumn::Version.eq(version.clone()))
            })
            .apply_if(params.sdk_version.as_ref(), |q, sdk_version| {
                q.filter(DriverColumn::SdkVersion.eq(sdk_version.clone()))
            })
            .apply_if(params.os_type, |q, os_type| {
                q.filter(DriverColumn::OsType.eq(os_type))
            })
            .apply_if(params.os_arch, |q, os_arch| {
                q.filter(DriverColumn::OsArch.eq(os_arch))
            })
            .apply_if(params.source, |q, source| {
                q.filter(DriverColumn::Source.eq(source))
            })
            .order_by(DriverColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<DriverInfo>()
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

    /// Find driver by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<DriverModel>> {
        let db = get_db_connection().await?;
        Ok(Driver::find_by_id(id).one(&db).await?)
    }

    /// Find driver info by ID (partial projection)
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<DriverInfo>> {
        let db = get_db_connection().await?;
        Ok(Driver::find_by_id(id)
            .into_partial_model::<DriverInfo>()
            .one(&db)
            .await?)
    }

    /// Find all drivers (as info projection)
    pub async fn find_all() -> StorageResult<Vec<DriverInfo>> {
        let db = get_db_connection().await?;
        Ok(Driver::find()
            .order_by_asc(DriverColumn::Id)
            .into_partial_model::<DriverInfo>()
            .all(&db)
            .await?)
    }

    /// Find drivers by driver type
    pub async fn find_by_driver_type(driver_type: &str) -> StorageResult<Vec<DriverModel>> {
        let db = get_db_connection().await?;
        Ok(Driver::find()
            .filter(DriverColumn::DriverType.eq(driver_type))
            .order_by_asc(DriverColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find drivers by (os_type, os_arch)
    pub async fn find_by_platform<C>(
        os_type: OsType,
        os_arch: OsArch,
        db: Option<&C>,
    ) -> StorageResult<Vec<DriverModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Driver::find()
                .filter(DriverColumn::OsType.eq(os_type))
                .filter(DriverColumn::OsArch.eq(os_arch))
                .order_by_asc(DriverColumn::Id)
                .all(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Driver::find()
                    .filter(DriverColumn::OsType.eq(os_type))
                    .filter(DriverColumn::OsArch.eq(os_arch))
                    .order_by_asc(DriverColumn::Id)
                    .all(&conn)
                    .await?)
            }
        }
    }

    /// Find by path (absolute path match)
    pub async fn find_by_path(path: &str) -> StorageResult<Vec<DriverModel>> {
        let db = get_db_connection().await?;
        Ok(Driver::find()
            .filter(DriverColumn::Path.eq(path))
            .order_by_asc(DriverColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find by checksum (exact match)
    pub async fn find_by_checksum(checksum: &str) -> StorageResult<Vec<DriverModel>> {
        let db = get_db_connection().await?;
        Ok(Driver::find()
            .filter(DriverColumn::Checksum.eq(checksum))
            .order_by_asc(DriverColumn::Id)
            .all(&db)
            .await?)
    }

    /// Check if a driver with the same (driver_type, version) exists
    pub async fn exists_by_type_and_version(
        driver_type: &str,
        version: &str,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        let count = Driver::find()
            .filter(DriverColumn::DriverType.eq(driver_type))
            .filter(DriverColumn::Version.eq(version))
            .count(&db)
            .await?;
        Ok(count > 0)
    }

    /// Check if a driver with the same (driver_type, version) exists, excluding the given ID
    pub async fn exists_by_type_and_version_exclude_id(
        id: i32,
        driver_type: &str,
        version: &str,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        let count = Driver::find()
            .filter(DriverColumn::Id.ne(id))
            .filter(DriverColumn::DriverType.eq(driver_type))
            .filter(DriverColumn::Version.eq(version))
            .count(&db)
            .await?;
        Ok(count > 0)
    }

    /// Check if driver exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Driver::find_by_id(id).count(&db).await? > 0)
    }
}
