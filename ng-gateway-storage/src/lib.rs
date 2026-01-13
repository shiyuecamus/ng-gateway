mod cache;
mod migration;
mod sql;

use crate::cache::moka::MokaCache;
use async_trait::async_trait;
use migration::{Migrator, MigratorTrait};
use ng_gateway_error::{
    init::InitContextError,
    storage::{CacheError, StorageError},
    NGResult,
};
use ng_gateway_models::{
    cache::{NGBaseCache, UserRoleCache, JWT_BLACKLIST_CACHE_NAME, USER_ROLE_CACHE_NAME},
    settings::{CacheType, Settings},
    CacheProvider, DbManager,
};
use sea_orm::DatabaseConnection;
use sql::sqlite;
use std::{any::Any, collections::HashMap, sync::Arc, time::Duration};
use tracing::{info, instrument};

/// Global database manager struct
pub struct NGDbManager {
    db_conn: Option<DatabaseConnection>,
}

#[async_trait]
impl DbManager for NGDbManager {
    #[inline]
    #[instrument(name = "init-db-manager", skip_all)]
    async fn init(settings: &Settings) -> NGResult<Arc<Self>, InitContextError> {
        let db_conn = {
            let db = sqlite::init_db(&settings.db.sqlite).await.map_err(|e| {
                InitContextError::Primitive(format!("Failed to init SQLite database: {e}"))
            })?;

            // Run database migrations
            Migrator::up(&db, None).await.map_err(|e| {
                InitContextError::Primitive(format!("Failed to migrate SQLite database: {e}"))
            })?;

            db
        };

        let db_manager = Arc::new(NGDbManager {
            db_conn: Some(db_conn),
        });

        info!("Database manager initialized successfully");
        Ok(db_manager)
    }

    #[inline]
    fn get_connection(&self) -> NGResult<DatabaseConnection, StorageError> {
        self.db_conn
            .as_ref()
            .ok_or(StorageError::StorageUnavailable)
            .cloned()
    }

    #[inline]
    #[instrument(name = "db_close", skip_all)]
    async fn close(&self) -> NGResult<()> {
        info!("🛑 Closing database connections...");
        if let Some(db) = &self.db_conn {
            db.clone().close().await?;
        }
        info!("✅ Database connections closed successfully");
        Ok(())
    }
}

#[derive(Debug)]
pub struct NGCacheProvider {
    prefix: String,
    delimiter: String,
    cache_type: CacheType,
    caches: HashMap<String, Arc<dyn Any + Send + Sync>>,
}

impl NGCacheProvider {
    /// Create new cache provider with pre-initialized caches
    pub fn new(prefix: &str, delimiter: &str, cache_type: CacheType) -> Self {
        Self {
            prefix: prefix.into(),
            delimiter: delimiter.into(),
            cache_type,
            caches: HashMap::new(),
        }
    }

    #[inline]
    #[instrument(name = "init-caches", skip_all)]
    fn init_caches(&mut self, settings: &Settings) {
        self.create_cache::<UserRoleCache>(
            USER_ROLE_CACHE_NAME,
            Some(10_000),
            Some(Duration::from_secs(settings.web.jwt.expire as u64)),
        )
        .expect("Failed to initialize user role cache");
        self.create_cache::<()>(
            JWT_BLACKLIST_CACHE_NAME,
            Some(10_000),
            Some(Duration::from_secs(settings.web.jwt.expire as u64)),
        )
        .expect("Failed to initialize jwt blacklist cache");
    }
}

#[async_trait]
impl CacheProvider for NGCacheProvider {
    #[inline]
    #[instrument(name = "init-cache-provider", skip(settings))]
    async fn init(settings: &Settings) -> NGResult<Arc<Self>, InitContextError> {
        let mut provider = Self::new(
            &settings.cache.prefix,
            &settings.cache.delimiter,
            settings.cache.r#type,
        );
        provider.init_caches(settings);
        Ok(Arc::new(provider))
    }

    #[inline]
    #[instrument(name = "create-cache", skip(self))]
    fn create_cache<V: Clone + Send + Sync + 'static>(
        &mut self,
        cache_name: &str,
        max_capacity: Option<u64>,
        ttl: Option<Duration>,
    ) -> NGResult<(), CacheError> {
        if self.caches.contains_key(cache_name) {
            return Err(CacheError::AlreadyExists(format!(
                "Cache already exists: {}",
                cache_name
            )));
        }

        let full_prefix = format!("{}{}{}", self.prefix, self.delimiter, cache_name);
        let cache = match self.cache_type {
            CacheType::Moka => {
                MokaCache::<V>::new(cache_name.to_string(), full_prefix, max_capacity, ttl)
            }
        };
        let cache: Arc<dyn NGBaseCache<Value = V> + Send + Sync> = Arc::new(cache);
        self.caches.insert(cache_name.to_string(), Arc::new(cache));
        info!("Cache created successfully: {}", cache_name);
        Ok(())
    }

    #[inline]
    fn get_cache<V>(
        &self,
        cache_name: &str,
    ) -> NGResult<Arc<dyn NGBaseCache<Value = V> + Send + Sync>, CacheError>
    where
        V: Clone + Send + Sync + 'static,
    {
        self.caches
            .get(cache_name)
            .and_then(|cache| {
                cache
                    .downcast_ref::<Arc<dyn NGBaseCache<Value = V> + Send + Sync>>()
                    .map(Arc::clone)
            })
            .ok_or(CacheError::NotFound(cache_name.to_string()))
    }
}
