use thiserror::Error;

/// Network subsystem errors.
///
/// Covers failures in interface enumeration, D-Bus communication,
/// Wi-Fi operations, AP management, and platform-specific fallbacks.
#[derive(Error, Debug)]
pub enum NetworkError {
    #[error("Network operation not supported on this platform: {0}")]
    PlatformNotSupported(String),

    #[error("NetworkManager is not available: {0}")]
    NetworkManagerUnavailable(String),

    #[error("D-Bus communication error: {0}")]
    DBusError(String),

    #[error("Network interface not found: {0}")]
    InterfaceNotFound(String),

    #[error("Wi-Fi operation failed: {0}")]
    WifiError(String),

    #[error("Wi-Fi connection timed out after {timeout_secs}s for SSID '{ssid}'")]
    WifiConnectionTimeout { ssid: String, timeout_secs: u64 },

    #[error("Wi-Fi scan failed: {0}")]
    WifiScanFailed(String),

    #[error("AP hotspot error: {0}")]
    ApError(String),

    #[error("AP configuration rollback triggered: {reason}")]
    ApConfigRollback { reason: String },

    #[error("DNS configuration error: {0}")]
    DnsError(String),

    #[error("Interface configuration error: {0}")]
    ConfigError(String),

    #[error("Capability detection failed: {0}")]
    CapabilityDetectionFailed(String),

    #[error("Command execution failed: {command} — {reason}")]
    CommandFailed { command: String, reason: String },

    #[error("Invalid network configuration: {0}")]
    ValidationError(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Network operation timed out: {0}")]
    Timeout(String),

    #[error("Wi-Fi connection profile not found: {0}")]
    WifiConnectionNotFound(String),

    #[error("Cannot forget Wi-Fi connection '{ssid}': deactivation failed — {reason}")]
    WifiForgetFailed { ssid: String, reason: String },
}

impl From<NetworkError> for crate::NGError {
    #[inline]
    fn from(e: NetworkError) -> Self {
        crate::NGError::Error(e.to_string())
    }
}
