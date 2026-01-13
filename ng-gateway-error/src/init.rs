use thiserror::Error;

/// Error type for InitContext operations
#[derive(Error, Debug)]
pub enum InitContextError {
    /// Returned when a key is not found in the context
    #[error("key not found: {0}")]
    KeyNotFound(String),
    /// Returned when type conversion fails
    #[error("type mismatch: {0}")]
    TypeMismatch(String),
    /// Returned when a primitive error occurs
    #[error("primitive error: {0}")]
    Primitive(String),
}
