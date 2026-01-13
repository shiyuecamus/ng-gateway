use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::{
    domain::prelude::{PageResult, PluginInfo, PluginPageParams},
    entities::prelude::{Plugin, PluginActiveModel, PluginColumn, PluginModel},
    enums::common::{OsArch, OsType},
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, Order, PaginatorTrait,
    QueryFilter, QueryOrder, QueryTrait,
};

/// Repository for northward plugin operations
pub struct PluginRepository;

impl PluginRepository {
    /// Create new northward plugin
    pub async fn create<C>(plugin: PluginActiveModel, db: Option<&C>) -> StorageResult<PluginModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(plugin.insert(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(plugin.insert(&conn).await?)
            }
        }
    }

    /// Update existing northward plugin
    pub async fn update<C>(plugin: PluginActiveModel, db: Option<&C>) -> StorageResult<PluginModel>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(plugin.update(conn).await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(plugin.update(&conn).await?)
            }
        }
    }

    /// Delete northward plugin by ID
    pub async fn delete<C>(id: i32, db: Option<&C>) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                Plugin::delete_by_id(id).exec(conn).await?;
            }
            None => {
                let conn = get_db_connection().await?;
                Plugin::delete_by_id(id).exec(&conn).await?;
            }
        }
        Ok(())
    }

    /// Paginate northward plugins with optional filters
    pub async fn page(params: PluginPageParams) -> StorageResult<PageResult<PluginInfo>> {
        let db = get_db_connection().await?;

        let query = Plugin::find()
            .apply_if(params.name.as_ref(), |q, name| {
                q.filter(PluginColumn::Name.like(format!("%{name}%")))
            })
            .apply_if(params.plugin_type.as_ref(), |q, plugin_type| {
                q.filter(PluginColumn::PluginType.like(format!("%{plugin_type}%")))
            })
            .apply_if(params.version.as_ref(), |q, version| {
                q.filter(PluginColumn::Version.eq(version.clone()))
            })
            .apply_if(params.sdk_version.as_ref(), |q, sdk_version| {
                q.filter(PluginColumn::SdkVersion.eq(sdk_version.clone()))
            })
            .apply_if(params.os_type, |q, os_type| {
                q.filter(PluginColumn::OsType.eq(os_type))
            })
            .apply_if(params.os_arch, |q, os_arch| {
                q.filter(PluginColumn::OsArch.eq(os_arch))
            })
            .apply_if(params.source, |q, source| {
                q.filter(PluginColumn::Source.eq(source))
            })
            .order_by(PluginColumn::Id, Order::Asc);

        let (page, page_size) = (params.page.page.unwrap(), params.page.page_size.unwrap());
        let total = query.clone().count(&db).await?;
        let records = query
            .into_partial_model::<PluginInfo>()
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

    /// Find northward plugin by ID
    pub async fn find_by_id(id: i32) -> StorageResult<Option<PluginModel>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find_by_id(id).one(&db).await?)
    }

    /// Find northward plugin info by ID (partial projection)
    pub async fn find_info_by_id(id: i32) -> StorageResult<Option<PluginInfo>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find_by_id(id)
            .into_partial_model::<PluginInfo>()
            .one(&db)
            .await?)
    }

    /// Find all northward plugins (as info projection)
    pub async fn find_all() -> StorageResult<Vec<PluginInfo>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find()
            .order_by_asc(PluginColumn::Id)
            .into_partial_model::<PluginInfo>()
            .all(&db)
            .await?)
    }

    /// Find northward plugins by plugin type
    pub async fn find_by_plugin_type(plugin_type: &str) -> StorageResult<Vec<PluginModel>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find()
            .filter(PluginColumn::PluginType.eq(plugin_type))
            .order_by_asc(PluginColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find northward plugins by (os_type, os_arch)
    pub async fn find_by_platform<C>(
        os_type: OsType,
        os_arch: OsArch,
        db: Option<&C>,
    ) -> StorageResult<Vec<PluginModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(Plugin::find()
                .filter(PluginColumn::OsType.eq(os_type))
                .filter(PluginColumn::OsArch.eq(os_arch))
                .order_by_asc(PluginColumn::Id)
                .all(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(Plugin::find()
                    .filter(PluginColumn::OsType.eq(os_type))
                    .filter(PluginColumn::OsArch.eq(os_arch))
                    .order_by_asc(PluginColumn::Id)
                    .all(&conn)
                    .await?)
            }
        }
    }

    /// Find by path (absolute path match)
    pub async fn find_by_path(path: &str) -> StorageResult<Vec<PluginModel>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find()
            .filter(PluginColumn::Path.eq(path))
            .order_by_asc(PluginColumn::Id)
            .all(&db)
            .await?)
    }

    /// Find by checksum (exact match)
    pub async fn find_by_checksum(checksum: &str) -> StorageResult<Vec<PluginModel>> {
        let db = get_db_connection().await?;
        Ok(Plugin::find()
            .filter(PluginColumn::Checksum.eq(checksum))
            .order_by_asc(PluginColumn::Id)
            .all(&db)
            .await?)
    }

    /// Check if a northward plugin with the same (plugin_type, version) exists
    pub async fn exists_by_type_and_version(
        plugin_type: &str,
        version: &str,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        let count = Plugin::find()
            .filter(PluginColumn::PluginType.eq(plugin_type))
            .filter(PluginColumn::Version.eq(version))
            .count(&db)
            .await?;
        Ok(count > 0)
    }

    /// Check if a northward plugin with the same (plugin_type, version) exists, excluding the given ID
    pub async fn exists_by_type_and_version_exclude_id(
        id: i32,
        plugin_type: &str,
        version: &str,
    ) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        let count = Plugin::find()
            .filter(PluginColumn::Id.ne(id))
            .filter(PluginColumn::PluginType.eq(plugin_type))
            .filter(PluginColumn::Version.eq(version))
            .count(&db)
            .await?;
        Ok(count > 0)
    }

    /// Check if northward plugin exists by ID
    pub async fn exists_by_id(id: i32) -> StorageResult<bool> {
        let db = get_db_connection().await?;
        Ok(Plugin::find_by_id(id).count(&db).await? > 0)
    }
}
