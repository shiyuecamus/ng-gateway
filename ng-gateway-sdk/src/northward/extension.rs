use crate::{NorthwardError, NorthwardResult};
use async_trait::async_trait;
use downcast_rs::{impl_downcast, DowncastSync};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;

/// Core extension manager trait for plugin-specific persistent data (object-safe)
///
/// This trait provides a key-value store interface using raw JSON values.
/// Use the `ExtensionManagerExt` trait for ergonomic typed access.
///
/// # Design Philosophy
/// - **Object-Safe**: Can be used as `Arc<dyn ExtensionManager>`
/// - **Flexible**: Raw JSON methods for maximum flexibility
/// - **Efficient**: Database-backed with indexing and caching
/// - **Isolated**: Each app's extensions are completely isolated
#[async_trait]
pub trait ExtensionManager: DowncastSync + Send + Sync {
    /// Delete an extension by key
    ///
    /// # Arguments
    /// * `key` - Extension key to delete
    ///
    /// # Returns
    /// * `Ok(true)` - Key existed and was deleted
    /// * `Ok(false)` - Key did not exist
    /// * `Err(...)` - Database error
    async fn delete(&self, key: &str) -> NorthwardResult<bool>;

    /// Check if a key exists
    ///
    /// # Arguments
    /// * `key` - Extension key to check
    ///
    /// # Returns
    /// * `Ok(true)` - Key exists
    /// * `Ok(false)` - Key does not exist
    /// * `Err(...)` - Database error
    async fn exists(&self, key: &str) -> NorthwardResult<bool>;

    /// Get all extension keys for this app
    ///
    /// # Returns
    /// * `Ok(Vec<String>)` - List of all keys
    /// * `Err(...)` - Database error
    async fn keys(&self) -> NorthwardResult<Vec<String>>;

    /// Clear all extensions for this app
    ///
    /// # Returns
    /// * `Ok(count)` - Number of extensions deleted
    /// * `Err(...)` - Database error
    async fn clear(&self) -> NorthwardResult<u64>;

    /// Get the number of extensions for this app
    ///
    /// # Returns
    /// * `Ok(count)` - Number of extensions
    /// * `Err(...)` - Database error
    async fn len(&self) -> NorthwardResult<usize>;

    /// Check if there are no extensions for this app
    ///
    /// # Returns
    /// * `Ok(true)` - No extensions exist
    /// * `Ok(false)` - At least one extension exists
    /// * `Err(...)` - Database error
    async fn is_empty(&self) -> NorthwardResult<bool> {
        Ok(self.len().await? == 0)
    }

    /// Get a raw JSON value by key
    ///
    /// # Arguments
    /// * `key` - Extension key
    ///
    /// # Returns
    /// * `Ok(Some(Value))` - Raw JSON value
    /// * `Ok(None)` - Key not found
    /// * `Err(...)` - Database error
    async fn get_raw(&self, key: &str) -> NorthwardResult<Option<serde_json::Value>>;

    /// Set a raw JSON value by key
    ///
    /// Performs an upsert operation (insert or update).
    ///
    /// # Arguments
    /// * `key` - Extension key
    /// * `value` - Raw JSON value
    ///
    /// # Returns
    /// * `Ok(())` - Value stored successfully
    /// * `Err(...)` - Database error
    async fn set_raw(&self, key: &str, value: serde_json::Value) -> NorthwardResult<()>;

    /// Get multiple raw JSON values by keys
    ///
    /// # Arguments
    /// * `keys` - List of keys to retrieve
    ///
    /// # Returns
    /// * `Ok(HashMap)` - Map of key -> raw JSON value (only existing keys)
    /// * `Err(...)` - Database error
    async fn get_many_raw(
        &self,
        keys: &[&str],
    ) -> NorthwardResult<HashMap<String, serde_json::Value>>;
}

impl_downcast!(sync ExtensionManager);

/// Extension trait providing typed access to ExtensionManager
///
/// This trait provides ergonomic typed methods with automatic
/// serialization/deserialization on top of the raw JSON methods.
///
/// # Examples
///
/// ```ignore
/// use ng_gateway_sdk::northward::extension::ExtensionManagerExt;
///
/// // Persist provision credentials
/// #[derive(Serialize, Deserialize)]
/// struct ProvisionData {
///     access_token: String,
///     device_id: String,
/// }
///
/// let data = ProvisionData { ... };
/// ext_mgr.set("provision", &data).await?;
///
/// // Retrieve credentials
/// let data: ProvisionData = ext_mgr.get("provision").await?.unwrap();
/// ```
#[async_trait]
pub trait ExtensionManagerExt: ExtensionManager {
    /// Get a value by key with automatic deserialization
    ///
    /// # Type Parameters
    /// * `T` - Target type implementing `DeserializeOwned`
    ///
    /// # Arguments
    /// * `key` - Extension key
    ///
    /// # Returns
    /// * `Ok(Some(T))` - Value found and deserialized successfully
    /// * `Ok(None)` - Key not found
    /// * `Err(...)` - Database error or deserialization error
    async fn get<T: DeserializeOwned>(&self, key: &str) -> NorthwardResult<Option<T>> {
        match self.get_raw(key).await? {
            Some(value) => {
                let deserialized = serde_json::from_value(value).map_err(|e| {
                    NorthwardError::DeserializationError {
                        reason: e.to_string(),
                    }
                })?;
                Ok(Some(deserialized))
            }
            None => Ok(None),
        }
    }

    /// Set a value by key with automatic serialization
    ///
    /// Performs an upsert operation (insert or update).
    ///
    /// # Type Parameters
    /// * `T` - Value type implementing `Serialize`
    ///
    /// # Arguments
    /// * `key` - Extension key
    /// * `value` - Value to store
    ///
    /// # Returns
    /// * `Ok(())` - Value stored successfully
    /// * `Err(...)` - Database error or serialization error
    async fn set<T: Serialize + Send + Sync>(&self, key: &str, value: &T) -> NorthwardResult<()> {
        let json_value =
            serde_json::to_value(value).map_err(|e| NorthwardError::SerializationError {
                reason: e.to_string(),
            })?;
        self.set_raw(key, json_value).await
    }

    /// Get multiple values by keys with automatic deserialization
    ///
    /// # Type Parameters
    /// * `T` - Target type implementing `DeserializeOwned`
    ///
    /// # Arguments
    /// * `keys` - List of keys to retrieve
    ///
    /// # Returns
    /// * `Ok(HashMap)` - Map of key -> deserialized value (only existing keys)
    /// * `Err(...)` - Database error or deserialization error
    async fn get_many<T: DeserializeOwned>(
        &self,
        keys: &[&str],
    ) -> NorthwardResult<HashMap<String, T>> {
        let raw_map = self.get_many_raw(keys).await?;
        let mut result = HashMap::with_capacity(raw_map.len());

        for (key, value) in raw_map {
            let deserialized = serde_json::from_value(value).map_err(|e| {
                NorthwardError::DeserializationError {
                    reason: format!("Failed to deserialize key '{}': {}", key, e),
                }
            })?;
            result.insert(key, deserialized);
        }

        Ok(result)
    }
}

// Blanket implementation: any ExtensionManager gets ExtensionManagerExt for free
impl<T: ExtensionManager + ?Sized> ExtensionManagerExt for T {}
