use async_trait::async_trait;
use ng_gateway_repository::AppExtRepository;
use ng_gateway_sdk::{ExtensionManager, NorthwardError, NorthwardResult};
use sea_orm::DatabaseConnection;
use std::collections::HashMap;

/// Database-backed extension manager for northward apps
///
/// This implementation provides persistent storage for plugin-specific data
/// using the `northward_app_ext` table. Each app's extensions are completely isolated.
///
/// **Thread-Safety**: Uses `DatabaseConnection` which is `Clone` and internally uses
/// connection pooling, making it safe to clone and use across async tasks.
#[derive(Debug, Clone)]
pub struct AppExtensionManager {
    /// App ID for isolation
    app_id: i32,
    /// Database connection (cloneable, uses connection pool internally)
    db: DatabaseConnection,
}

impl AppExtensionManager {
    /// Create a new extension manager for a specific app
    ///
    /// # Arguments
    /// * `app_id` - The app ID for data isolation
    /// * `db` - Database connection (cloneable, pooled)
    pub fn new(app_id: i32, db: DatabaseConnection) -> Self {
        Self { app_id, db }
    }
}

#[async_trait]
impl ExtensionManager for AppExtensionManager {
    async fn delete(&self, key: &str) -> NorthwardResult<bool> {
        AppExtRepository::delete::<DatabaseConnection>(self.app_id, key, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to delete extension: {}", e),
            })
    }

    async fn exists(&self, key: &str) -> NorthwardResult<bool> {
        AppExtRepository::exists::<DatabaseConnection>(self.app_id, key, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to check extension existence: {}", e),
            })
    }

    async fn keys(&self) -> NorthwardResult<Vec<String>> {
        AppExtRepository::get_keys::<DatabaseConnection>(self.app_id, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to get extension keys: {}", e),
            })
    }

    async fn clear(&self) -> NorthwardResult<u64> {
        AppExtRepository::clear::<DatabaseConnection>(self.app_id, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to clear extensions: {}", e),
            })
    }

    async fn len(&self) -> NorthwardResult<usize> {
        AppExtRepository::count::<DatabaseConnection>(self.app_id, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to count extensions: {}", e),
            })
    }

    async fn get_raw(&self, key: &str) -> NorthwardResult<Option<serde_json::Value>> {
        let model =
            AppExtRepository::get_by_key::<DatabaseConnection>(self.app_id, key, Some(&self.db))
                .await
                .map_err(|e| NorthwardError::ConfigurationError {
                    message: format!("Failed to get extension: {}", e),
                })?;

        Ok(model.map(|m| m.value))
    }

    async fn set_raw(&self, key: &str, value: serde_json::Value) -> NorthwardResult<()> {
        AppExtRepository::upsert::<DatabaseConnection>(self.app_id, key, value, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to set extension: {}", e),
            })
    }

    async fn get_many_raw(
        &self,
        keys: &[&str],
    ) -> NorthwardResult<HashMap<String, serde_json::Value>> {
        AppExtRepository::get_many::<DatabaseConnection>(self.app_id, keys, Some(&self.db))
            .await
            .map_err(|e| NorthwardError::ConfigurationError {
                message: format!("Failed to get multiple extensions: {}", e),
            })
    }
}
