use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

/// Driver specific errors
#[derive(Error, Debug, Default)]
pub enum DriverError {
    #[error("Service unavailable")]
    #[default]
    ServiceUnavailable,
    #[error("Validation error: {0}")]
    ValidationError(String),
    #[error("Execution error: {0}")]
    ExecutionError(String),
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
    #[error("Initialization error: {0}")]
    InitializationError(String),
    #[error("Invalid state error: {0}")]
    InvalidStateError(String),
    #[error("Codec error: {0}")]
    CodecError(String),
    #[error("Read/Write timeout")]
    Timeout(Duration),
    #[error("Planner error: {0}")]
    PlannerError(String),
    #[error("Session error: {0}")]
    SessionError(String),
    #[error("Subscription error: {0}")]
    SubscriptionError(String),
    #[error("Northward error: {0}")]
    NorthwardError(String),
    #[error("LoadError error: {0}")]
    LoadError(String),
    #[error("Invalid entity: {0}")]
    InvalidEntity(String),
}

/// Northward communication specific errors
#[derive(Error, Debug)]
pub enum NorthwardError {
    /// Authentication failed
    #[error("Authentication failed for platform '{platform}': {reason}")]
    AuthenticationFailed { platform: String, reason: String },

    /// Message publishing failed
    #[error("Failed to publish message to platform '{platform}': {reason}")]
    PublishFailed { platform: String, reason: String },

    /// Message subscription failed
    #[error("Failed to subscribe to topic '{topic}' on platform '{platform}': {reason}")]
    SubscriptionFailed {
        platform: String,
        topic: String,
        reason: String,
    },

    /// Serialization error
    #[error("Serialization failed: {reason}")]
    SerializationError { reason: String },

    /// Deserialization error
    #[error("Deserialization failed: {reason}")]
    DeserializationError { reason: String },

    /// Configuration error
    #[error("Configuration error: {message}")]
    ConfigurationError { message: String },

    /// Data send error
    #[error("Data send error: {message}")]
    DataSendError { message: String },

    /// Timeout error
    #[error("Operation timed out after {timeout_ms}ms: {operation}")]
    Timeout { timeout_ms: u64, operation: String },

    /// Circuit breaker open
    #[error("Circuit breaker is open for platform '{platform}'")]
    CircuitBreakerOpen { platform: String },

    /// Platform not found
    #[error("Platform '{platform}' not found or not configured")]
    PlatformNotFound { platform: String },

    /// Invalid message format
    #[error("Invalid message format: {reason}")]
    InvalidMessageFormat { reason: String },

    /// TLS/SSL error
    #[error("TLS error for platform '{platform}': {reason}")]
    TlsError { platform: String, reason: String },

    /// MQTT protocol error
    #[error("MQTT protocol error: {reason}")]
    MqttError { reason: String },

    /// Resource exhausted (e.g., connection pool full)
    #[error("Resource exhausted: {resource}")]
    ResourceExhausted { resource: String },

    /// Operation not supported
    #[error("Operation not supported: {operation} for platform '{platform}'")]
    OperationNotSupported { operation: String, platform: String },

    /// Device provision failed
    #[error("Device provision failed for platform '{platform}': {reason}")]
    ProvisionFailed { platform: String, reason: String },

    /// Device credentials not found
    #[error("Device credentials not found for platform '{platform}'")]
    CredentialsNotFound { platform: String },

    /// Device provision timeout
    #[error("Device provision timeout after {timeout_ms}ms for platform '{platform}'")]
    ProvisionTimeout { platform: String, timeout_ms: u64 },

    /// Not connected to platform
    #[error("Not connected to platform - operation requires active connection")]
    NotConnected,

    /// Invalid provision configuration
    #[error("Invalid provision configuration for platform '{platform}': {reason}")]
    InvalidProvisionConfig { platform: String, reason: String },

    /// Internal queue is full; backpressure signaled to upstream
    #[error("Northward manager queue is full (capacity reached)")]
    QueueFull,

    /// Dynamic loading error
    #[error("Load error: {0}")]
    LoadError(String),

    /// Entity not found in gateway runtime (non-hot-path).
    #[error("Not found: {entity}")]
    NotFound { entity: String },

    /// Gateway-side execution error (non-hot-path), used by write-back protocols.
    #[error("Gateway error: {reason}")]
    GatewayError { reason: String },

    /// Parameter/value validation failed (non-hot-path), used by write-back protocols.
    #[error("Validation failed: {reason}")]
    ValidationFailed { reason: String },

    /// Runtime execution error within the plugin environment (e.g. task cancellation)
    #[error("Runtime error: {reason}")]
    RuntimeError { reason: String },
}

impl From<serde_json::Error> for NorthwardError {
    fn from(err: serde_json::Error) -> Self {
        NorthwardError::SerializationError {
            reason: err.to_string(),
        }
    }
}

impl<T> From<mpsc::error::SendError<T>> for NorthwardError {
    fn from(err: mpsc::error::SendError<T>) -> Self {
        NorthwardError::DataSendError {
            message: format!("Channel send failed: {err}"),
        }
    }
}
