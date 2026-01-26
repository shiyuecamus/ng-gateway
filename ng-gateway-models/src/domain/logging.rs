use serde::{Deserialize, Serialize};
use tracing::Level as TracingLevel;

/// Log level DTO used by gateway APIs.
///
/// # Notes
/// - Serialized as upper-case strings (`"INFO"`, `"DEBUG"`...).
/// - This type is intentionally **API-facing** (DTO). Runtime filtering/FFI mapping lives in
///   `ng-gateway-common::log`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Stable driver/FFI mapping:
/// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
impl From<LogLevel> for u8 {
    #[inline]
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => 0,
            LogLevel::Warn => 1,
            LogLevel::Info => 2,
            LogLevel::Debug => 3,
            LogLevel::Trace => 4,
        }
    }
}

/// Infallible mapping from u8 to LogLevel.
///
/// Unknown values fall back to TRACE (most permissive).
impl From<u8> for LogLevel {
    #[inline]
    fn from(v: u8) -> Self {
        match v {
            0 => LogLevel::Error,
            1 => LogLevel::Warn,
            2 => LogLevel::Info,
            3 => LogLevel::Debug,
            _ => LogLevel::Trace,
        }
    }
}

impl From<TracingLevel> for LogLevel {
    #[inline]
    fn from(level: TracingLevel) -> Self {
        Self::from(&level)
    }
}

impl From<&TracingLevel> for LogLevel {
    #[inline]
    fn from(level: &TracingLevel) -> Self {
        match *level {
            TracingLevel::ERROR => LogLevel::Error,
            TracingLevel::WARN => LogLevel::Warn,
            TracingLevel::INFO => LogLevel::Info,
            TracingLevel::DEBUG => LogLevel::Debug,
            TracingLevel::TRACE => LogLevel::Trace,
        }
    }
}

impl From<LogLevel> for TracingLevel {
    #[inline]
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => TracingLevel::ERROR,
            LogLevel::Warn => TracingLevel::WARN,
            LogLevel::Info => TracingLevel::INFO,
            LogLevel::Debug => TracingLevel::DEBUG,
            LogLevel::Trace => TracingLevel::TRACE,
        }
    }
}

/// TTL range description for UI validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtlRange {
    pub min_ms: u64,
    pub max_ms: u64,
    pub default_ms: u64,
}

/// Global log level view (baseline + effective).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalLogLevelView {
    /// Host baseline level (what the system is set to).
    pub baseline: LogLevel,
    /// Effective global level (baseline + any global overrides).
    pub effective: LogLevel,
    /// TTL policy for channel overrides.
    pub channel_override_ttl: TtlRange,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetGlobalLogLevelRequest {
    pub level: LogLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelLogOverrideView {
    pub level: LogLevel,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelLogLevelView {
    pub channel_id: i32,
    pub effective: LogLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#override: Option<ChannelLogOverrideView>,
    pub ttl: TtlRange,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetChannelLogLevelRequest {
    pub level: LogLevel,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
}
