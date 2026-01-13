mod prelude;
mod user;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ng_gateway_error::StorageResult;
use std::{future::Future, sync::Arc, time::Duration};

pub use prelude::*;

pub const USER_ROLE_CACHE_NAME: &str = "UserRole";
pub const JWT_BLACKLIST_CACHE_NAME: &str = "JwtBlacklist";

/// Base cache trait that defines common cache operations
#[async_trait]
pub trait NGBaseCache: Send + Sync + 'static {
    /// Associated value type that can be cached
    type Value: Clone + Send + Sync + 'static;

    /// Get cache name
    fn name(&self) -> &str;

    /// Get value by key
    async fn get(&self, key: String) -> StorageResult<Option<Self::Value>>;

    /// Set value with default TTL
    async fn set(&self, key: String, value: Self::Value) -> StorageResult<()>;

    /// Set value if key not exists
    async fn set_nx(&self, key: String, value: Self::Value) -> StorageResult<bool>;

    /// Set value if key exists
    async fn set_xx(&self, key: String, value: Self::Value) -> StorageResult<bool>;

    /// Set value with custom TTL
    async fn set_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<()>;

    /// Set value with custom TTL if key not exists
    async fn set_nx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool>;

    /// Set value with custom TTL if key exists
    async fn set_xx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool>;

    /// Set value with expire time
    async fn set_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<()>;

    /// Set value with expire time if key not exists
    async fn set_nx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool>;

    /// Set value with expire time if key exists
    async fn set_xx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool>;

    /// Delete key
    async fn delete(&self, key: String) -> StorageResult<bool>;

    /// Delete all keys with prefix
    async fn delete_all(&self) -> StorageResult<u64>;

    /// Delete multiple keys
    async fn delete_by_keys(&self, keys: Vec<String>) -> StorageResult<u64>;

    /// Increment counter
    async fn incr(&self, key: String) -> StorageResult<i64>;

    /// Increment counter by value
    async fn incr_by(&self, key: String, value: i64) -> StorageResult<i64>;

    /// Decrement counter
    async fn decr(&self, key: String) -> StorageResult<i64>;

    /// Decrement counter by value
    async fn decr_by(&self, key: String, value: i64) -> StorageResult<i64>;

    /// Check if key exists
    async fn exists(&self, key: String) -> StorageResult<bool>;
}

#[async_trait]
impl<T: NGBaseCache + ?Sized> NGBaseCache for Arc<T> {
    type Value = T::Value;

    fn name(&self) -> &str {
        (**self).name()
    }

    async fn get(&self, key: String) -> StorageResult<Option<Self::Value>> {
        (**self).get(key).await
    }

    async fn set(&self, key: String, value: Self::Value) -> StorageResult<()> {
        (**self).set(key, value).await
    }

    async fn set_nx(&self, key: String, value: Self::Value) -> StorageResult<bool> {
        (**self).set_nx(key, value).await
    }

    async fn set_xx(&self, key: String, value: Self::Value) -> StorageResult<bool> {
        (**self).set_xx(key, value).await
    }

    async fn set_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<()> {
        (**self).set_with_ttl(key, value, ttl).await
    }

    async fn set_nx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool> {
        (**self).set_nx_with_ttl(key, value, ttl).await
    }

    async fn set_xx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool> {
        (**self).set_xx_with_ttl(key, value, ttl).await
    }

    async fn set_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<()> {
        (**self).set_with_expire_at(key, value, expire_at).await
    }

    async fn set_nx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        (**self).set_nx_with_expire_at(key, value, expire_at).await
    }

    async fn set_xx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        (**self).set_xx_with_expire_at(key, value, expire_at).await
    }

    async fn delete(&self, key: String) -> StorageResult<bool> {
        (**self).delete(key).await
    }

    async fn delete_all(&self) -> StorageResult<u64> {
        (**self).delete_all().await
    }

    async fn delete_by_keys(&self, keys: Vec<String>) -> StorageResult<u64> {
        (**self).delete_by_keys(keys).await
    }

    async fn incr(&self, key: String) -> StorageResult<i64> {
        (**self).incr(key).await
    }

    async fn incr_by(&self, key: String, value: i64) -> StorageResult<i64> {
        (**self).incr_by(key, value).await
    }

    async fn decr(&self, key: String) -> StorageResult<i64> {
        (**self).decr(key).await
    }

    async fn decr_by(&self, key: String, value: i64) -> StorageResult<i64> {
        (**self).decr_by(key, value).await
    }

    async fn exists(&self, key: String) -> StorageResult<bool> {
        (**self).exists(key).await
    }
}

#[async_trait]
pub trait NGCacheExt: NGBaseCache {
    async fn get_or_create<F, Fut>(&self, key: String, f: F) -> StorageResult<Self::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<Self::Value>> + Send;

    async fn get_or_create_with_ttl<F, Fut>(
        &self,
        key: String,
        ttl: Duration,
        f: F,
    ) -> StorageResult<Self::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<Self::Value>> + Send;

    async fn get_or_create_with_expire_at<F, Fut>(
        &self,
        key: String,
        expire_at: DateTime<Utc>,
        f: F,
    ) -> StorageResult<Self::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<Self::Value>> + Send;
}

#[async_trait]
impl<T: NGBaseCache> NGCacheExt for T {
    async fn get_or_create<F, Fut>(&self, key: String, f: F) -> StorageResult<T::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<T::Value>> + Send,
    {
        if let Some(value) = self.get(key.clone()).await? {
            return Ok(value);
        }
        let value = f(key.clone()).await?;
        self.set(key, value.clone()).await?;
        Ok(value)
    }

    async fn get_or_create_with_ttl<F, Fut>(
        &self,
        key: String,
        ttl: Duration,
        f: F,
    ) -> StorageResult<T::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<T::Value>> + Send,
    {
        if let Some(value) = self.get(key.clone()).await? {
            return Ok(value);
        }
        let value = f(key.clone()).await?;
        self.set_with_ttl(key, value.clone(), ttl).await?;
        Ok(value)
    }

    async fn get_or_create_with_expire_at<F, Fut>(
        &self,
        key: String,
        expire_at: DateTime<Utc>,
        f: F,
    ) -> StorageResult<T::Value>
    where
        F: FnOnce(String) -> Fut + Send + Sync,
        Fut: Future<Output = StorageResult<T::Value>> + Send,
    {
        if let Some(value) = self.get(key.clone()).await? {
            return Ok(value);
        }
        let value = f(key.clone()).await?;
        self.set_with_expire_at(key, value.clone(), expire_at)
            .await?;
        Ok(value)
    }
}
