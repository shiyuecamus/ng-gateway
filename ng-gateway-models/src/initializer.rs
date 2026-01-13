use crate::{
    constants::{BUILTIN_DIR, DRIVER_DIR, PLUGIN_DIR},
    domain::prelude::{NewDriver, NewPlugin},
    entities::prelude::{
        Driver, DriverActiveModel, DriverColumn, DriverModel, Plugin, PluginActiveModel,
        PluginColumn, PluginModel,
    },
    enums::common::SourceType,
    idens,
};
use async_trait::async_trait;
use ng_gateway_error::{init::InitContextError, NGError, NGResult};
use ng_gateway_sdk::{probe_driver_library, probe_north_library};
use sea_orm::{
    sea_query::{IndexCreateStatement, TableCreateStatement, TableDropStatement},
    ActiveModelTrait, ColumnTrait, DatabaseBackend, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use std::{any::Any, collections::HashMap, path::Path};
use tokio::fs::{metadata, read_dir, try_exists};

#[async_trait]
pub trait NGInitializer: Send + Sync {
    fn order(&self) -> i32;

    fn name(&self) -> &str;

    fn has_update_col(&self) -> bool;

    fn to_create_table_stmt(&self, backend: DatabaseBackend) -> TableCreateStatement;

    fn to_drop_table_stmt(&self, backend: DatabaseBackend) -> TableDropStatement;

    fn to_create_indexes_stmt(&self, backend: DatabaseBackend)
        -> Option<Vec<IndexCreateStatement>>;

    async fn seeding_data(
        &self,
        transaction: &DatabaseTransaction,
        ctx: &mut InitContext,
    ) -> Result<(), DbErr>;
}

/// Trait for types that can be seeded into the database
pub trait SeedableTrait: Send + Sync + 'static {
    /// The active model type for database insertion
    type ActiveModel: ActiveModelTrait<Entity = Self::Entity>;
    /// The entity type for database operations
    type Entity: EntityTrait;

    /// Convert self into an active model
    fn get_active_model(&self) -> Self::ActiveModel;
}

/// Trait for database initializers that can seed data
#[async_trait]
pub trait DataSeederTrait<T: SeedableTrait + Clone> {
    /// Get the initial seed data
    async fn get_seed_data(&self, ctx: &mut InitContext) -> Result<Option<Vec<T>>, DbErr>;
}

/// Helper trait that combines NGInitializer and DataSeeder
#[async_trait]
pub trait SeedableInitializerTrait<T: SeedableTrait + Clone>:
    NGInitializer + DataSeederTrait<T>
{
    /// Default implementation for seeding data
    async fn seed_data(
        &self,
        transaction: &DatabaseTransaction,
        ctx: &mut InitContext,
    ) -> Result<(), DbErr> {
        if let Some(seed_data) = self.get_seed_data(ctx).await? {
            // Skip when there is no data to seed to avoid empty INSERTs
            if seed_data.is_empty() {
                return Ok(());
            }

            let active_models: Vec<T::ActiveModel> = seed_data
                .clone()
                .into_iter()
                .map(|d| d.get_active_model())
                .collect();

            T::Entity::insert_many(active_models)
                .exec(transaction)
                .await?;

            ctx.set(self.name(), seed_data);
        }
        Ok(())
    }
}

pub fn initializers() -> Vec<Box<dyn NGInitializer>> {
    let mut initializers: Vec<Box<dyn NGInitializer>> = vec![
        Box::new(idens::branding::Branding::Table),
        Box::new(idens::driver::Driver::Table),
        Box::new(idens::role::Role::Table),
        Box::new(idens::user::User::Table),
        Box::new(idens::menu::Menu::Table),
        Box::new(idens::relation::Relation::Table),
        Box::new(idens::casbin::Casbin::Table),
        Box::new(idens::credentials::Credentials::Table),
        Box::new(idens::channel::Channel::Table),
        Box::new(idens::device::Device::Table),
        Box::new(idens::point::Point::Table),
        Box::new(idens::action::Action::Table),
        Box::new(idens::plugin::Plugin::Table),
        Box::new(idens::app::App::Table),
        Box::new(idens::app_sub::AppSub::Table),
        Box::new(idens::app_ext::AppExt::Table),
    ];

    initializers.sort_by_key(|init| init.order());
    initializers
}

pub struct BuiltinSynchronizer;

impl BuiltinSynchronizer {
    pub async fn sync(db: &DatabaseConnection) -> NGResult<()> {
        tracing::info!("Starting builtin artifacts synchronization...");

        Self::sync_drivers(db).await?;
        Self::sync_plugins(db).await?;

        tracing::info!("Builtin artifacts synchronization completed.");
        Ok(())
    }

    async fn sync_drivers(db: &DatabaseConnection) -> NGResult<()> {
        // 1. Sync Drivers
        let disk_drivers = Self::scan_builtin_drivers().await.map_err(|e| {
            NGError::InitContextError(InitContextError::Primitive(format!(
                "Scan builtin drivers failed: {}",
                e
            )))
        })?;

        if !disk_drivers.is_empty() {
            // Load all existing BuiltIn drivers
            let existing_drivers = Driver::find()
                .filter(DriverColumn::Source.eq(SourceType::BuiltIn))
                .all(db)
                .await
                .map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Load existing drivers failed: {}",
                        e
                    )))
                })?;

            // Create a map for quick lookup: Name -> Model
            let mut existing_map: HashMap<String, DriverModel> = existing_drivers
                .into_iter()
                .map(|d| (d.name.clone(), d))
                .collect();

            let mut to_insert = Vec::new();

            for new_driver in disk_drivers {
                if let Some(existing) = existing_map.remove(&new_driver.name) {
                    let mut active: DriverActiveModel = existing.into();

                    // Update fields
                    active.version = Set(new_driver.version);
                    active.api_version = Set(new_driver.api_version);
                    active.sdk_version = Set(new_driver.sdk_version);
                    active.os_type = Set(new_driver.os_type);
                    active.os_arch = Set(new_driver.os_arch);
                    active.size = Set(new_driver.size);
                    active.path = Set(new_driver.path);
                    active.checksum = Set(new_driver.checksum);
                    active.metadata = Set(new_driver.metadata);
                    active.updated_at = Set(Some(chrono::Utc::now()));

                    active.update(db).await.map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Update driver failed: {}",
                            e
                        )))
                    })?;
                } else {
                    // Insert
                    to_insert.push(new_driver.into_active_model());
                }
            }

            if !to_insert.is_empty() {
                Driver::insert_many(to_insert).exec(db).await.map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Insert drivers failed: {}",
                        e
                    )))
                })?;
            }

            // Remove obsolete drivers (exist in DB but not on disk)
            if !existing_map.is_empty() {
                let ids_to_delete: Vec<i32> = existing_map.values().map(|d| d.id).collect();
                tracing::info!(
                    "Removing obsolete builtin drivers: {:?}",
                    existing_map.keys().collect::<Vec<_>>()
                );
                Driver::delete_many()
                    .filter(DriverColumn::Id.is_in(ids_to_delete))
                    .exec(db)
                    .await
                    .map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Delete obsolete drivers failed: {}",
                            e
                        )))
                    })?;
            }
        } else {
            // Disk is empty, but we might have obsolete drivers in DB
            let existing_drivers = Driver::find()
                .filter(DriverColumn::Source.eq(SourceType::BuiltIn))
                .all(db)
                .await
                .map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Load existing drivers failed: {}",
                        e
                    )))
                })?;

            if !existing_drivers.is_empty() {
                let ids_to_delete: Vec<i32> = existing_drivers.into_iter().map(|d| d.id).collect();
                tracing::info!(
                    "Removing all builtin drivers as disk directory is empty/scanned empty"
                );
                Driver::delete_many()
                    .filter(DriverColumn::Id.is_in(ids_to_delete))
                    .exec(db)
                    .await
                    .map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Delete all builtin drivers failed: {}",
                            e
                        )))
                    })?;
            }
        }
        Ok(())
    }

    async fn sync_plugins(db: &DatabaseConnection) -> NGResult<()> {
        // 2. Sync Plugins
        let disk_plugins = Self::scan_builtin_plugins().await.map_err(|e| {
            NGError::InitContextError(InitContextError::Primitive(format!(
                "Scan builtin plugins failed: {}",
                e
            )))
        })?;

        if !disk_plugins.is_empty() {
            // Load all existing BuiltIn plugins
            let existing_plugins = Plugin::find()
                .filter(PluginColumn::Source.eq(SourceType::BuiltIn))
                .all(db)
                .await
                .map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Load existing plugins failed: {}",
                        e
                    )))
                })?;

            // Create a map for quick lookup: Name -> Model
            let mut existing_map: HashMap<String, PluginModel> = existing_plugins
                .into_iter()
                .map(|d| (d.name.clone(), d))
                .collect();

            let mut to_insert = Vec::new();

            for new_plugin in disk_plugins {
                if let Some(existing) = existing_map.remove(&new_plugin.name) {
                    let mut active: PluginActiveModel = existing.into();

                    // Update fields
                    active.version = Set(new_plugin.version);
                    active.api_version = Set(new_plugin.api_version);
                    active.sdk_version = Set(new_plugin.sdk_version);
                    active.os_type = Set(new_plugin.os_type);
                    active.os_arch = Set(new_plugin.os_arch);
                    active.size = Set(new_plugin.size);
                    active.path = Set(new_plugin.path);
                    active.checksum = Set(new_plugin.checksum);
                    active.metadata = Set(new_plugin.metadata);
                    active.updated_at = Set(Some(chrono::Utc::now()));

                    active.update(db).await.map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Update plugin failed: {}",
                            e
                        )))
                    })?;
                } else {
                    // Insert
                    to_insert.push(new_plugin.into_active_model());
                }
            }

            if !to_insert.is_empty() {
                Plugin::insert_many(to_insert).exec(db).await.map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Insert plugins failed: {}",
                        e
                    )))
                })?;
            }

            // Remove obsolete plugins (exist in DB but not on disk)
            if !existing_map.is_empty() {
                let ids_to_delete: Vec<i32> = existing_map.values().map(|p| p.id).collect();
                tracing::info!(
                    "Removing obsolete builtin plugins: {:?}",
                    existing_map.keys().collect::<Vec<_>>()
                );
                Plugin::delete_many()
                    .filter(PluginColumn::Id.is_in(ids_to_delete))
                    .exec(db)
                    .await
                    .map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Delete obsolete plugins failed: {}",
                            e
                        )))
                    })?;
            }
        } else {
            // Disk is empty, but we might have obsolete plugins in DB
            let existing_plugins = Plugin::find()
                .filter(PluginColumn::Source.eq(SourceType::BuiltIn))
                .all(db)
                .await
                .map_err(|e| {
                    NGError::InitContextError(InitContextError::Primitive(format!(
                        "Load existing plugins failed: {}",
                        e
                    )))
                })?;

            if !existing_plugins.is_empty() {
                let ids_to_delete: Vec<i32> = existing_plugins.into_iter().map(|p| p.id).collect();
                tracing::info!(
                    "Removing all builtin plugins as disk directory is empty/scanned empty"
                );
                Plugin::delete_many()
                    .filter(PluginColumn::Id.is_in(ids_to_delete))
                    .exec(db)
                    .await
                    .map_err(|e| {
                        NGError::InitContextError(InitContextError::Primitive(format!(
                            "Delete all builtin plugins failed: {}",
                            e
                        )))
                    })?;
            }
        }
        Ok(())
    }

    async fn scan_builtin_drivers() -> Result<Vec<NewDriver>, DbErr> {
        let dir = Path::new(DRIVER_DIR).join(BUILTIN_DIR);
        if !(try_exists(&dir)
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?)
        {
            return Ok(Vec::new());
        }

        let mut dir_entries = read_dir(&dir)
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?;

        let mut set = tokio::task::JoinSet::new();

        while let Some(entry) = dir_entries
            .next_entry()
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?
        {
            let path = entry.path();
            set.spawn(async move {
                let meta = match metadata(&path).await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            "[southward-driver] metadata error path={:?} err={}",
                            path,
                            e
                        );
                        return None;
                    }
                };
                if !meta.is_file() {
                    tracing::info!("[southward-driver] skip non-file path={:?}", path);
                    return None;
                }

                let path_clone = path.clone();
                let probe =
                    match tokio::task::spawn_blocking(move || probe_driver_library(&path_clone))
                        .await
                    {
                        Ok(res) => match res {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!(
                                    "[southward-driver] probe error path={:?} err={}",
                                    path,
                                    e
                                );
                                return None;
                            }
                        },
                        Err(e) => {
                            tracing::error!(
                                "[southward-driver] task join error path={:?} err={}",
                                path,
                                e
                            );
                            return None;
                        }
                    };

                let metadata = match serde_json::to_value(&probe.metadata) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "[southward-driver] metadata serialize error path={:?} err={}",
                            path,
                            e
                        );
                        return None;
                    }
                };

                // Store the library path as a runtime-root-relative path in the DB.
                //
                // Why: runtime directories can be relocated (via `general.runtime_dir` + chdir),
                // so DB rows must not hardcode absolute paths.
                let db_path = normalize_runtime_relative_path(&path);

                Some(NewDriver {
                    name: probe.name.clone(),
                    description: probe.description,
                    driver_type: probe.driver_type,
                    source: SourceType::BuiltIn,
                    version: probe.version,
                    api_version: probe.api_version,
                    sdk_version: probe.sdk_version,
                    os_type: probe.os_type.into(),
                    os_arch: probe.os_arch.into(),
                    size: probe.size,
                    path: db_path,
                    checksum: probe.checksum,
                    metadata,
                })
            });
        }

        let mut records = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(driver)) = res {
                records.push(driver);
            }
        }

        Ok(records)
    }

    async fn scan_builtin_plugins() -> Result<Vec<NewPlugin>, DbErr> {
        let dir = Path::new(PLUGIN_DIR).join(BUILTIN_DIR);
        if !(try_exists(&dir)
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?)
        {
            return Ok(Vec::new());
        }

        let mut dir_entries = read_dir(&dir)
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?;

        let mut set = tokio::task::JoinSet::new();

        while let Some(entry) = dir_entries
            .next_entry()
            .await
            .map_err(|e| DbErr::Custom(e.to_string()))?
        {
            let path = entry.path();
            set.spawn(async move {
                let meta = match metadata(&path).await {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            "[northward_plugin] metadata error path={:?} err={}",
                            path,
                            e
                        );
                        return None;
                    }
                };
                if !meta.is_file() {
                    tracing::info!("[northward_plugin] skip non-file path={:?}", path);
                    return None;
                }

                let path_clone = path.clone();
                let probe =
                    match tokio::task::spawn_blocking(move || probe_north_library(&path_clone))
                        .await
                    {
                        Ok(res) => match res {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!(
                                    "[northward_plugin] probe error path={:?} err={}",
                                    path,
                                    e
                                );
                                return None;
                            }
                        },
                        Err(e) => {
                            tracing::error!(
                                "[northward_plugin] task join error path={:?} err={}",
                                path,
                                e
                            );
                            return None;
                        }
                    };

                let metadata = match serde_json::to_value(&probe.metadata) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            "[northward_plugin] metadata serialize error path={:?} err={}",
                            path,
                            e
                        );
                        return None;
                    }
                };

                // Store the library path as a runtime-root-relative path in the DB.
                let db_path = normalize_runtime_relative_path(&path);

                Some(NewPlugin {
                    name: probe.name.clone(),
                    description: probe.description,
                    plugin_type: probe.plugin_type,
                    source: SourceType::BuiltIn,
                    version: probe.version,
                    api_version: probe.api_version,
                    sdk_version: probe.sdk_version,
                    os_type: probe.os_type.into(),
                    os_arch: probe.os_arch.into(),
                    size: probe.size,
                    path: db_path,
                    checksum: probe.checksum,
                    metadata,
                })
            });
        }

        let mut records = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(plugin)) = res {
                records.push(plugin);
            }
        }

        Ok(records)
    }
}

/// Normalize an artifact path so it can be stored in the database safely.
///
/// # Design
/// - Always prefer runtime-root-relative paths (e.g. `drivers/builtin/libng_driver_xxx.so`).
/// - If an absolute path is provided, we try to strip the current working directory prefix.
///   This makes the path stable when users relocate `general.runtime_dir`.
fn normalize_runtime_relative_path(path: &Path) -> String {
    if path.is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = path.strip_prefix(&cwd) {
                return rel.to_string_lossy().to_string();
            }
        }
    }
    path.to_string_lossy().to_string()
}

/// A context for storing initialization data between different initializers
///
/// This struct provides a type-safe way to store and retrieve vectors of initialization data
/// that can be shared between different initialization steps
pub struct InitContext {
    /// Internal storage using type-erased vectors of data
    data: HashMap<String, Vec<Box<dyn Any + Send + Sync>>>,
}

impl InitContext {
    /// Creates a new empty initialization context
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Stores a vector of initialization data under the specified key
    ///
    /// # Arguments
    /// * `key` - The key under which to store the data
    /// * `values` - A vector of values to store
    pub fn set<T: 'static + Send + Sync>(&mut self, key: &str, values: Vec<T>) {
        let boxed_values: Vec<Box<dyn Any + Send + Sync>> = values
            .into_iter()
            .map(|v| Box::new(v) as Box<dyn Any + Send + Sync>)
            .collect();
        self.data.insert(key.into(), boxed_values);
    }

    /// Retrieves a vector of previously stored data for the specified key and type
    ///
    /// # Arguments
    /// * `key` - The key to lookup
    ///
    /// # Returns
    /// * `Result<Vec<&T>, InitContextError>` - A vector of references to the stored data if found and all elements match the expected type
    pub fn get<T: 'static>(&self, key: &str) -> NGResult<Vec<&T>> {
        let values =
            self.data
                .get(key)
                .ok_or(NGError::InitContextError(InitContextError::KeyNotFound(
                    key.into(),
                )))?;

        values
            .iter()
            .map(|value| {
                value.downcast_ref::<T>().ok_or(NGError::InitContextError(
                    InitContextError::TypeMismatch(key.into()),
                ))
            })
            .collect()
    }
}

impl Default for InitContext {
    fn default() -> Self {
        Self::new()
    }
}
