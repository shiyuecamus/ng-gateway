use thiserror::Error;

/// Classifies cache-related errors to avoid ad-hoc strings.
#[derive(Error, Debug, Clone)]
pub enum CacheError {
    /// Value type is incompatible with the requested operation (e.g. non-integer for incr)
    #[error("cache value type incompatible: {0}")]
    ValueType(String),
    /// Operation could not initialize or persist a value atomically
    #[error("cache initialization failure: {0}")]
    Initialization(String),
    /// Unexpected internal state was encountered
    #[error("cache unexpected state: {0}")]
    Unexpected(String),
    /// Generic cache error message
    #[error("cache error: {0}")]
    Msg(String),
    /// Key does not exist or value factory returned no value
    #[error("cache key missing: {0}")]
    KeyMiss(String),
    /// TTL or expire_at was already expired or invalid
    #[error("cache ttl expired or invalid: {0}")]
    TTLExpired(String),
    /// Concurrency conflict such as NX/XX semantics violation
    #[error("cache concurrency conflict: {0}")]
    ConcurrencyConflict(String),
    /// Cache already exists
    #[error("cache already exists: {0}")]
    AlreadyExists(String),
    /// Cache not found
    #[error("cache not found: {0}")]
    NotFound(String),
}

#[derive(Error, Debug, Default)]
pub enum StorageError {
    #[error("database unavailable")]
    #[default]
    StorageUnavailable,

    #[error("database error: `{0}`")]
    DBError(#[from] sea_orm::DbErr),

    #[error("entity not found: {0}")]
    EntityNotFound(String),

    /// Structured cache error kind for better classification
    #[error("{0}")]
    CacheKind(#[from] CacheError),
}
