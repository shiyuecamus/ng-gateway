pub mod init;
pub mod rbac;
pub mod storage;
pub mod tls;
pub mod web;

use anyhow::Error as AnyhowError;
use config::ConfigError;
use init::InitContextError;
use sea_orm::{DbErr, TransactionError};
use serde_json::Error as SerdeJsonError;
use std::{error::Error as StdError, io::Error as IoError, num::TryFromIntError};
use storage::StorageError;
use thiserror::Error;
use tls::TLSError;
use tokio::{task::JoinError, time::Duration};
use web::WebError;

pub type NGResult<T, E = NGError> = anyhow::Result<T, E>;
pub type WebResult<T, E = WebError> = anyhow::Result<T, E>;
pub type StorageResult<T, E = StorageError> = Result<T, E>;

#[derive(Error, Debug, Default)]
pub enum NGError {
    #[error("service unavailable")]
    #[default]
    ServiceUnavailable,
    #[error("read/write timeout")]
    Timeout(Duration),
    #[error("{0}")]
    JoinError(#[from] JoinError),
    #[error("{0}")]
    StdError(#[from] Box<dyn StdError + Send + Sync>),
    #[error("{0}")]
    Error(String),
    #[error("{0}")]
    IoError(#[from] IoError),
    #[error("{0}")]
    Msg(String),
    #[error("{0}")]
    Anyhow(#[from] AnyhowError),
    #[error("{0}")]
    Json(#[from] SerdeJsonError),
    #[error("{0}")]
    ConfigError(#[from] ConfigError),
    #[error("{0}")]
    TryFromIntError(#[from] TryFromIntError),
    #[error("{0}")]
    StorageError(#[from] StorageError),
    #[error("{0}")]
    TLSError(#[from] TLSError),
    #[error("{0}")]
    InitContextError(#[from] InitContextError),
    #[error("{0}")]
    WebError(#[from] WebError),
    #[error("Driver error: {0}")]
    DriverError(String),
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
    #[error("Initialization error: {0}")]
    InitializationError(String),
    #[error("Shutdown error: {0}")]
    ShutdownError(String),
    #[error("Serialization error: {0}")]
    SerializationError(String),
    #[error("Deserialization error: {0}")]
    DeserializationError(String),
    #[error("Invalid state error: {0}")]
    InvalidStateError(String),
    #[error("Unknown error")]
    None,
}

impl From<()> for NGError {
    #[inline]
    fn from(_: ()) -> Self {
        Self::default()
    }
}

impl From<String> for NGError {
    #[inline]
    fn from(e: String) -> Self {
        NGError::Msg(e)
    }
}

impl From<&str> for NGError {
    #[inline]
    fn from(e: &str) -> Self {
        NGError::Msg(e.to_string())
    }
}

impl From<&NGError> for NGError {
    #[inline]
    fn from(e: &NGError) -> Self {
        NGError::Msg(e.to_string())
    }
}

impl From<DbErr> for NGError {
    #[inline]
    fn from(e: DbErr) -> Self {
        NGError::StorageError(StorageError::DBError(e))
    }
}

impl From<TransactionError<NGError>> for NGError {
    #[inline]
    fn from(e: TransactionError<NGError>) -> Self {
        NGError::Msg(e.to_string())
    }
}

impl From<Box<dyn StdError>> for NGError {
    #[inline]
    fn from(e: Box<dyn StdError>) -> Self {
        NGError::Error(e.to_string())
    }
}

impl From<NGError> for () {
    fn from(_: NGError) {}
}
