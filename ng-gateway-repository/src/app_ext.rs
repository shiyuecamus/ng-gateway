use crate::get_db_connection;
use ng_gateway_error::StorageResult;
use ng_gateway_models::entities::prelude::*;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter,
    QuerySelect, Set,
};
use std::collections::HashMap;

/// Repository for northward_app_ext table operations
pub struct AppExtRepository;

impl AppExtRepository {
    /// Get extension value by app_id and key
    pub async fn get_by_key<C>(
        app_id: i32,
        key: &str,
        db: Option<&C>,
    ) -> StorageResult<Option<AppExtModel>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(AppExt::find()
                .filter(AppExtColumn::AppId.eq(app_id))
                .filter(AppExtColumn::Key.eq(key))
                .one(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.eq(key))
                    .one(&conn)
                    .await?)
            }
        }
    }

    /// Upsert (insert or update) extension value
    pub async fn upsert<C>(
        app_id: i32,
        key: &str,
        value: serde_json::Value,
        db: Option<&C>,
    ) -> StorageResult<()>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => {
                // Check if exists
                let existing = Self::get_by_key(app_id, key, Some(conn)).await?;

                if let Some(model) = existing {
                    // Update existing
                    let mut active_model: AppExtActiveModel = model.into();
                    active_model.value = Set(value);
                    active_model.update(conn).await?;
                } else {
                    // Insert new
                    let active_model = AppExtActiveModel {
                        app_id: Set(app_id),
                        key: Set(key.to_string()),
                        value: Set(value),
                        ..Default::default()
                    };
                    active_model.insert(conn).await?;
                }
                Ok(())
            }
            None => {
                let conn = get_db_connection().await?;
                // Check if exists
                let existing = Self::get_by_key(app_id, key, Some(&conn)).await?;

                if let Some(model) = existing {
                    // Update existing
                    let mut active_model: AppExtActiveModel = model.into();
                    active_model.value = Set(value);
                    active_model.update(&conn).await?;
                } else {
                    // Insert new
                    let active_model = AppExtActiveModel {
                        app_id: Set(app_id),
                        key: Set(key.to_string()),
                        value: Set(value),
                        ..Default::default()
                    };
                    active_model.insert(&conn).await?;
                }
                Ok(())
            }
        }
    }

    /// Delete extension by app_id and key
    pub async fn delete<C>(app_id: i32, key: &str, db: Option<&C>) -> StorageResult<bool>
    where
        C: ConnectionTrait,
    {
        let result = match db {
            Some(conn) => {
                AppExt::delete_many()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.eq(key))
                    .exec(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::delete_many()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.eq(key))
                    .exec(&conn)
                    .await?
            }
        };

        Ok(result.rows_affected > 0)
    }

    /// Check if extension exists
    pub async fn exists<C>(app_id: i32, key: &str, db: Option<&C>) -> StorageResult<bool>
    where
        C: ConnectionTrait,
    {
        let count = match db {
            Some(conn) => {
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.eq(key))
                    .count(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.eq(key))
                    .count(&conn)
                    .await?
            }
        };

        Ok(count > 0)
    }

    /// Get all extension keys for an app
    pub async fn get_keys<C>(app_id: i32, db: Option<&C>) -> StorageResult<Vec<String>>
    where
        C: ConnectionTrait,
    {
        match db {
            Some(conn) => Ok(AppExt::find()
                .filter(AppExtColumn::AppId.eq(app_id))
                .select_only()
                .column(AppExtColumn::Key)
                .into_tuple::<String>()
                .all(conn)
                .await?),
            None => {
                let conn = get_db_connection().await?;
                Ok(AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .select_only()
                    .column(AppExtColumn::Key)
                    .into_tuple::<String>()
                    .all(&conn)
                    .await?)
            }
        }
    }

    /// Clear all extensions for an app
    pub async fn clear<C>(app_id: i32, db: Option<&C>) -> StorageResult<u64>
    where
        C: ConnectionTrait,
    {
        let result = match db {
            Some(conn) => {
                AppExt::delete_many()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .exec(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::delete_many()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .exec(&conn)
                    .await?
            }
        };

        Ok(result.rows_affected)
    }

    /// Get count of extensions for an app
    pub async fn count<C>(app_id: i32, db: Option<&C>) -> StorageResult<usize>
    where
        C: ConnectionTrait,
    {
        let count = match db {
            Some(conn) => {
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .count(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .count(&conn)
                    .await?
            }
        };

        Ok(count as usize)
    }

    /// Get multiple extensions by keys
    pub async fn get_many<C>(
        app_id: i32,
        keys: &[&str],
        db: Option<&C>,
    ) -> StorageResult<HashMap<String, serde_json::Value>>
    where
        C: ConnectionTrait,
    {
        let models = match db {
            Some(conn) => {
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.is_in(keys.iter().copied()))
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .filter(AppExtColumn::Key.is_in(keys.iter().copied()))
                    .all(&conn)
                    .await?
            }
        };

        let mut result = HashMap::with_capacity(models.len());
        for model in models {
            result.insert(model.key, model.value);
        }

        Ok(result)
    }

    /// Get all extensions for an app
    pub async fn get_all<C>(
        app_id: i32,
        db: Option<&C>,
    ) -> StorageResult<HashMap<String, serde_json::Value>>
    where
        C: ConnectionTrait,
    {
        let models = match db {
            Some(conn) => {
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .all(conn)
                    .await?
            }
            None => {
                let conn = get_db_connection().await?;
                AppExt::find()
                    .filter(AppExtColumn::AppId.eq(app_id))
                    .all(&conn)
                    .await?
            }
        };

        let mut result = HashMap::with_capacity(models.len());
        for model in models {
            result.insert(model.key, model.value);
        }

        Ok(result)
    }
}
