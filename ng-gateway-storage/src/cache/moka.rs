use async_trait::async_trait;
use chrono::{DateTime, Utc};
use moka::{
    future::{Cache as MokaInner, CacheBuilder},
    Expiry,
};
use ng_gateway_error::{storage::CacheError, StorageResult};
use ng_gateway_models::cache::NGBaseCache;
use std::{any::Any, time::Duration};

// The cached item with per-entry expiration metadata.
#[derive(Clone)]
pub struct CacheItem<V>
where
    V: Clone + Send + Sync + 'static,
{
    pub value: V,
    pub ttl: Option<Duration>, // Fixed TTL
}

/// Moka-based in-memory cache implementation.
///
/// Generic over `V` which must implement `Clone` to support get/set ergonomics.
pub struct MokaCache<V>
where
    V: Clone + Send + Sync + 'static,
{
    /// Logical cache name for metrics and debugging
    name: String,
    /// Logical prefix (unused for key composition; retained for compatibility/metrics)
    prefix: String,
    /// The underlying moka cache
    inner: MokaInner<String, CacheItem<V>>,
}

// Custom Expiry that maps item metadata to a remaining duration per entry.
pub struct MokaExpiry {
    ttl: Option<Duration>,
}

impl<K, V> Expiry<K, CacheItem<V>> for MokaExpiry
where
    V: Clone + Send + Sync + 'static,
{
    fn expire_after_create(
        &self,
        _key: &K,
        item: &CacheItem<V>,
        _: std::time::Instant,
    ) -> Option<Duration> {
        match item.ttl {
            Some(ttl) => Some(ttl),
            None => self.ttl,
        }
    }

    fn expire_after_update(
        &self,
        _key: &K,
        item: &CacheItem<V>,
        _: std::time::Instant,
        _current: Option<Duration>,
    ) -> Option<Duration> {
        match item.ttl {
            Some(ttl) => Some(ttl),
            None => self.ttl,
        }
    }
}

// ----- Numeric helpers -----
// These helper functions provide runtime-checked conversions between `V` and i128
// to implement arithmetic for primitive integer cache values without adding
// extra trait bounds to `V`. Non-integer types will be rejected.

#[inline]
fn try_value_to_i128<V: 'static>(v: &V) -> Option<i128> {
    let any = v as &dyn Any;
    // Signed integers
    if let Some(x) = any.downcast_ref::<i8>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<i16>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<i32>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<i64>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<isize>() {
        return Some(*x as i128);
    }
    // Unsigned integers
    if let Some(x) = any.downcast_ref::<u8>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<u16>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<u32>() {
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<u64>() {
        // May overflow i128 for very large u64, but i128 can represent all u64
        return Some(*x as i128);
    }
    if let Some(x) = any.downcast_ref::<usize>() {
        return Some(*x as i128);
    }
    None
}

#[inline]
fn try_i128_to_value<V: 'static>(n: i128) -> Option<V> {
    // Signed integers
    if n >= i8::MIN as i128 && n <= i8::MAX as i128 {
        let x: i8 = n as i8;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= i16::MIN as i128 && n <= i16::MAX as i128 {
        let x: i16 = n as i16;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= i32::MIN as i128 && n <= i32::MAX as i128 {
        let x: i32 = n as i32;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= i64::MIN as i128 && n <= i64::MAX as i128 {
        let x: i64 = n as i64;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= isize::MIN as i128 && n <= isize::MAX as i128 {
        let x: isize = n as isize;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    // Unsigned integers (require n >= 0)
    if n >= 0 && n <= u8::MAX as i128 {
        let x: u8 = n as u8;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= 0 && n <= u16::MAX as i128 {
        let x: u16 = n as u16;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= 0 && n <= u32::MAX as i128 {
        let x: u32 = n as u32;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= 0 && n <= u64::MAX as i128 {
        let x: u64 = n as u64;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    if n >= 0 && n <= usize::MAX as i128 {
        let x: usize = n as usize;
        let b: Box<dyn Any> = Box::new(x);
        if let Ok(v) = b.downcast::<V>() {
            return Some(*v);
        }
    }
    None
}

#[inline]
fn compute_add_result<V: Clone + Send + Sync + 'static>(
    current: Option<&V>,
    delta: i64,
) -> Result<(V, i64), CacheError> {
    // Convert current value to i128 if present; otherwise start from zero
    let base_i128 = match current {
        Some(v) => match try_value_to_i128(v) {
            Some(n) => n,
            None => {
                return Err(CacheError::ValueType(
                    "incr/decr requires integer cache value".into(),
                ))
            }
        },
        None => 0i128,
    };

    let delta_i128 = delta as i128;
    let sum_i128 = base_i128
        .checked_add(delta_i128)
        .ok_or(CacheError::Unexpected("integer addition overflow".into()))?;

    // Ensure the return value fits into i64 as required by the trait signature
    if sum_i128 < i64::MIN as i128 || sum_i128 > i64::MAX as i128 {
        return Err(CacheError::Unexpected(
            "result does not fit into i64".into(),
        ));
    }

    // Attempt to convert back to V preserving the original numeric type
    let new_v: V = try_i128_to_value::<V>(sum_i128).ok_or(CacheError::Unexpected(
        "result does not fit destination type".into(),
    ))?;

    Ok((new_v, sum_i128 as i64))
}

impl<V> MokaCache<V>
where
    V: Clone + Send + Sync + 'static,
{
    /// Create a new Moka cache using fixed parameters.
    pub fn new(
        name: String,
        prefix: String,
        max_capacity: Option<u64>,
        ttl: Option<Duration>,
    ) -> Self {
        // Fixed parameters: conservative but reasonable defaults for gateway
        let mut builder = CacheBuilder::default().expire_after(MokaExpiry { ttl });
        if let Some(max_capacity) = max_capacity {
            builder = builder.max_capacity(max_capacity);
        }
        let inner = builder.build();

        Self {
            name,
            prefix,
            inner,
        }
    }

    /// Build full key with prefix.
    #[inline]
    fn get_full_key(&self, key: String) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}:{key}", self.prefix)
        }
    }
}

#[async_trait]
impl<V> NGBaseCache for MokaCache<V>
where
    V: Clone + Send + Sync + 'static,
{
    type Value = V;

    #[inline]
    fn name(&self) -> &str {
        &self.name
    }

    #[inline]
    async fn get(&self, key: String) -> StorageResult<Option<Self::Value>> {
        let full_key = self.get_full_key(key);
        let value = self.inner.get(&full_key).await;
        Ok(value.map(|item| item.value))
    }

    #[inline]
    async fn set(&self, key: String, value: Self::Value) -> StorageResult<()> {
        let full_key = self.get_full_key(key);
        self.inner
            .insert(full_key, CacheItem { value, ttl: None })
            .await;
        Ok(())
    }

    #[inline]
    async fn set_nx(&self, key: String, value: Self::Value) -> StorageResult<bool> {
        // Non-atomic best-effort NX
        let full_key = self.get_full_key(key);
        if self.inner.contains_key(&full_key) {
            return Ok(false);
        }
        self.inner
            .insert(full_key, CacheItem { value, ttl: None })
            .await;
        Ok(true)
    }

    #[inline]
    async fn set_xx(&self, key: String, value: Self::Value) -> StorageResult<bool> {
        // Only set if exists
        let full_key = self.get_full_key(key);
        let exists = self.inner.contains_key(&full_key);
        if exists {
            self.inner
                .insert(full_key, CacheItem { value, ttl: None })
                .await;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[inline]
    async fn set_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<()> {
        if ttl.as_millis() == 0 {
            return Err(CacheError::TTLExpired("ttl must be > 0".into()).into());
        }
        let full_key = self.get_full_key(key);
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(ttl),
                },
            )
            .await;
        Ok(())
    }

    #[inline]
    async fn set_nx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool> {
        if ttl.as_millis() == 0 {
            return Err(CacheError::TTLExpired("ttl must be > 0".into()).into());
        }
        let full_key = self.get_full_key(key);
        if self.inner.contains_key(&full_key) {
            return Ok(false);
        }
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(ttl),
                },
            )
            .await;
        Ok(true)
    }

    #[inline]
    async fn set_xx_with_ttl(
        &self,
        key: String,
        value: Self::Value,
        ttl: Duration,
    ) -> StorageResult<bool> {
        if ttl.as_millis() == 0 {
            return Err(CacheError::TTLExpired("ttl must be > 0".into()).into());
        }
        let full_key = self.get_full_key(key);
        if !self.inner.contains_key(&full_key) {
            return Ok(false);
        }
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(ttl),
                },
            )
            .await;
        Ok(true)
    }

    #[inline]
    async fn set_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<()> {
        let now = Utc::now();
        let ms = (expire_at - now).num_milliseconds();
        if ms <= 0 {
            return Err(CacheError::TTLExpired("expire_at is in the past".into()).into());
        }
        let full_key = self.get_full_key(key);
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(Duration::from_millis(ms as u64)),
                },
            )
            .await;
        Ok(())
    }

    #[inline]
    async fn set_nx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        let now = Utc::now();
        let ms = (expire_at - now).num_milliseconds();
        if ms <= 0 {
            return Err(CacheError::TTLExpired("expire_at is in the past".into()).into());
        }
        let full_key = self.get_full_key(key);
        if self.inner.contains_key(&full_key) {
            return Ok(false);
        }
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(Duration::from_millis(ms as u64)),
                },
            )
            .await;
        Ok(true)
    }

    #[inline]
    async fn set_xx_with_expire_at(
        &self,
        key: String,
        value: Self::Value,
        expire_at: DateTime<Utc>,
    ) -> StorageResult<bool> {
        let now = Utc::now();
        let ms = (expire_at - now).num_milliseconds();
        if ms <= 0 {
            return Err(CacheError::TTLExpired("expire_at is in the past".into()).into());
        }
        let full_key = self.get_full_key(key);
        if !self.inner.contains_key(&full_key) {
            return Ok(false);
        }
        self.inner
            .insert(
                full_key.clone(),
                CacheItem {
                    value,
                    ttl: Some(Duration::from_millis(ms as u64)),
                },
            )
            .await;
        Ok(true)
    }

    #[inline]
    async fn delete(&self, key: String) -> StorageResult<bool> {
        let full_key = self.get_full_key(key);
        let existed = self.inner.contains_key(&full_key);
        self.inner.invalidate(&full_key).await;
        Ok(existed)
    }

    #[inline]
    async fn delete_all(&self) -> StorageResult<u64> {
        self.inner.invalidate_all();
        Ok(self.inner.entry_count())
    }

    #[inline]
    async fn delete_by_keys(&self, keys: Vec<String>) -> StorageResult<u64> {
        let mut deleted: u64 = 0;
        for key in keys {
            let full_key = self.get_full_key(key);
            if self.inner.contains_key(&full_key) {
                deleted += 1;
            }
            self.inner.invalidate(&full_key).await;
        }
        Ok(deleted)
    }

    #[inline]
    async fn incr(&self, key: String) -> StorageResult<i64> {
        self.incr_by(key, 1).await
    }

    #[inline]
    async fn incr_by(&self, key: String, value: i64) -> StorageResult<i64> {
        let full_key = self.get_full_key(key);
        let current = self.inner.get(&full_key).await;
        let current_ref = current.as_ref().map(|it| &it.value);
        let (new_v, ret) = compute_add_result::<V>(current_ref, value)?;
        // Preserve existing TTL if present
        let ttl = current.and_then(|it| it.ttl);
        self.inner
            .insert(full_key, CacheItem { value: new_v, ttl })
            .await;
        Ok(ret)
    }

    #[inline]
    async fn decr(&self, key: String) -> StorageResult<i64> {
        self.decr_by(key, 1).await
    }

    #[inline]
    async fn decr_by(&self, key: String, value: i64) -> StorageResult<i64> {
        let full_key = self.get_full_key(key);
        let current = self.inner.get(&full_key).await;
        let current_ref = current.as_ref().map(|it| &it.value);
        let (new_v, ret) = compute_add_result::<V>(current_ref, -value)?;
        let ttl = current.and_then(|it| it.ttl);
        self.inner
            .insert(full_key, CacheItem { value: new_v, ttl })
            .await;
        Ok(ret)
    }

    #[inline]
    async fn exists(&self, key: String) -> StorageResult<bool> {
        let full_key = self.get_full_key(key);
        Ok(self.inner.contains_key(&full_key))
    }
}
