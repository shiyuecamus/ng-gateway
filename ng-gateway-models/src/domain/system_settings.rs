use serde::{Deserialize, Serialize};
use tracing::Level as TracingLevel;
use validator::{Validate, ValidationError};

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
    /// TTL in ms used when setting this override. Enables accurate countdown progress in UI.
    pub ttl_ms: u64,
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

/// Log file information for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileInfo {
    pub name: String,
    pub size: u64,
    pub modified_at: i64,
}

/// Response for listing log files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFilesListResponse {
    pub files: Vec<LogFileInfo>,
}

/// Request for downloading log files.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadLogFilesRequest {
    pub files: Vec<String>,
}

/// Request for cleaning up log files by policy.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupLogFilesRequest {
    /// If true, only returns the deletion plan without deleting anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// Response for log cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupLogFilesResponse {
    pub deleted: Vec<LogFileInfo>,
    pub freed_bytes: u64,
    /// Files that were kept because they look active / unsafe to delete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protected_active: Vec<String>,
}

/// System settings domain for runtime tuning.
///
/// # Notes
/// - Serialized as `snake_case` to match REST paths like `/system/settings/{domain}`.
/// - This is **API-facing** and should remain stable across internal refactors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemSettingsDomain {
    Collector,
    Northward,
    Southward,
    LoggingOutput,
    LoggingCleanup,
    Metrics,
}

/// Where a setting's effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingValueSource {
    Default,
    File,
    Env,
}

/// A single setting field view (value + provenance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingField<T> {
    pub value: T,
    pub source: SettingValueSource,
    /// Whether this field is controlled by an env var override.
    ///
    /// If true, UI should treat the field as read-only and surface the derived env key.
    pub env_overridden: bool,
    /// Derived env key for this field (e.g. `NG__GENERAL__COLLECTOR__COLLECTION_TIMEOUT_MS`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
}

/// Strongly-typed key for runtime-editable settings (no magic strings).
///
/// # Scope (phase 1)
/// Only includes collector keys required by the initial rollout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeSettingKey {
    GeneralCollectorCollectionTimeoutMs,
    GeneralCollectorMaxConcurrentCollections,
    GeneralCollectorRetryPolicyMaxAttempts,
    GeneralCollectorRetryPolicyInitialIntervalMs,
    GeneralCollectorRetryPolicyMaxIntervalMs,
    GeneralCollectorRetryPolicyRandomizationFactor,
    GeneralCollectorRetryPolicyMultiplier,
    GeneralCollectorRetryPolicyMaxElapsedTimeMs,
    GeneralCollectorOutboundQueueCapacity,
    GeneralNorthwardQueueCapacity,
    LoggingOutputFormat,
    LoggingOutputIncludeSpanFields,
    LoggingOutputFileEnabled,
    LoggingOutputFileDir,
    LoggingOutputFileRotationMode,
    LoggingOutputFileRotationTime,
    LoggingOutputFileRotationSizeMb,
    LoggingOutputFileRotationMaxFiles,
    LoggingOutputFileRetentionMaxDays,
    LoggingOutputFileRetentionMaxTotalSizeMb,
    LoggingCleanupEnabled,
    LoggingCleanupIntervalMs,
    GeneralSouthwardStartTimeoutMs,
    GeneralNorthwardStartTimeoutMs,
    GeneralSouthwardDeviceChangeCacheTtlMs,
    GeneralSouthwardSnapshotGcIntervalMs,
    GeneralSouthwardSnapshotGcWorkers,
    GeneralSouthwardMaxDevicesPerSnapshotTick,
}

/// Apply impact semantics returned to callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SystemSettingsImpact {
    HotApply,
    RestartComponent { components: Vec<String> },
    RestartProcess,
}

/// Result of a single domain `PATCH` call (apply + persist + impact).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplySystemSettingsResult {
    pub applied: bool,
    pub persisted: bool,
    pub domain: SystemSettingsDomain,
    pub changed_keys: Vec<RuntimeSettingKey>,
    pub blocked_by_env: Vec<RuntimeSettingKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence_warning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_warning: Option<String>,
    pub impact: SystemSettingsImpact,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_targets: Vec<String>,
}

/// Aggregated settings view for the System preferences page.
///
/// # Rationale
/// Domain endpoints (`/system/settings/{domain}`) remain the source of truth for
/// independent apply/persist/impact semantics. This aggregated view is only a
/// *read-optimized* endpoint to reduce initial page-load roundtrips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemSettingsOverviewView {
    pub collector: CollectorSettingsView,
    pub northward: NorthwardSettingsView,
    pub southward: SouthwardSettingsView,
    pub logging_runtime: GlobalLogLevelView,
    pub logging_output: LoggingOutputSettingsView,
    pub logging_cleanup: LoggingCleanupSettingsView,
}

/// Collector settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectorSettingsView {
    pub collection_timeout_ms: SettingField<u64>,
    pub max_concurrent_collections: SettingField<u64>,
    pub retry_policy: RetryPolicySettingsView,
    pub outbound_queue_capacity: SettingField<u64>,
}

/// Retry policy settings view (collector).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPolicySettingsView {
    pub max_attempts: SettingField<Option<u32>>,
    pub initial_interval_ms: SettingField<u64>,
    pub max_interval_ms: SettingField<u64>,
    pub randomization_factor: SettingField<f64>,
    pub multiplier: SettingField<f64>,
    pub max_elapsed_time_ms: SettingField<Option<u64>>,
}

/// Collector settings patch (partial update).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchCollectorSettingsRequest {
    #[serde(default)]
    pub collection_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_concurrent_collections: Option<u64>,
    #[serde(default)]
    pub retry_policy: Option<PatchRetryPolicyRequest>,
    #[serde(default)]
    pub outbound_queue_capacity: Option<u64>,
}

/// Northward settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NorthwardSettingsView {
    pub queue_capacity: SettingField<u64>,
    pub start_timeout_ms: SettingField<u64>,
}

/// Northward settings patch (partial update).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchNorthwardSettingsRequest {
    #[serde(default)]
    pub queue_capacity: Option<u64>,
    #[serde(default)]
    pub start_timeout_ms: Option<u64>,
}

/// Southward settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SouthwardSettingsView {
    pub start_timeout_ms: SettingField<u64>,
    pub device_change_cache_ttl_ms: SettingField<u64>,
    pub snapshot_gc_interval_ms: SettingField<u64>,
    pub snapshot_gc_workers: SettingField<u64>,
    pub max_devices_per_snapshot_tick: SettingField<u64>,
}

/// Southward settings patch (partial update).
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchSouthwardSettingsRequest {
    #[serde(default)]
    #[validate(range(min = 1, max = 300_000))]
    pub start_timeout_ms: Option<u64>,
    /// `0` disables eviction; otherwise recommended range: [10_000, 86_400_000]
    #[serde(default)]
    #[validate(custom(function = "validate_ttl_ms_or_zero"))]
    pub device_change_cache_ttl_ms: Option<u64>,
    /// Recommended range: [200, 300_000]
    #[serde(default)]
    #[validate(range(min = 200, max = 300_000))]
    pub snapshot_gc_interval_ms: Option<u64>,
    /// Recommended range: [1, 16]
    #[serde(default)]
    #[validate(range(min = 1, max = 16))]
    pub snapshot_gc_workers: Option<u64>,
    /// Recommended range: [1, 10_000]
    #[serde(default)]
    #[validate(range(min = 1, max = 10_000))]
    pub max_devices_per_snapshot_tick: Option<u64>,
}

/// Patch request for retry policy (partial update).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRetryPolicyRequest {
    /// Set to `null` to mean unlimited retries.
    #[serde(default)]
    pub max_attempts: Option<Option<u32>>,
    #[serde(default)]
    pub initial_interval_ms: Option<u64>,
    #[serde(default)]
    pub max_interval_ms: Option<u64>,
    #[serde(default)]
    pub randomization_factor: Option<f64>,
    #[serde(default)]
    pub multiplier: Option<f64>,
    /// Set to `null` to remove policy elapsed-time limit (collector still enforces its own budget).
    #[serde(default)]
    pub max_elapsed_time_ms: Option<Option<u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggingOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggingRotationMode {
    Time,
    Size,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoggingTimeRotation {
    Hourly,
    Daily,
}

/// Logging output settings view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingOutputSettingsView {
    pub format: SettingField<LoggingOutputFormat>,
    pub include_span_fields: SettingField<bool>,
    pub file: LoggingFileOutputSettingsView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingFileOutputSettingsView {
    pub enabled: SettingField<bool>,
    pub dir: SettingField<String>,
    pub rotation: LoggingFileRotationSettingsView,
    pub retention: LoggingFileRetentionSettingsView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingFileRotationSettingsView {
    pub mode: SettingField<LoggingRotationMode>,
    pub time: SettingField<LoggingTimeRotation>,
    pub size_mb: SettingField<u64>,
    pub max_files: SettingField<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingFileRetentionSettingsView {
    pub max_days: SettingField<u64>,
    pub max_total_size_mb: SettingField<u64>,
}

/// Logging output settings patch (partial update).
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchLoggingOutputSettingsRequest {
    #[serde(default)]
    pub format: Option<LoggingOutputFormat>,
    #[serde(default)]
    pub include_span_fields: Option<bool>,
    #[serde(default)]
    #[validate(nested)]
    pub file: Option<PatchLoggingFileOutputRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchLoggingFileOutputRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    #[validate(custom(function = "validate_non_empty_trimmed"))]
    pub dir: Option<String>,
    #[serde(default)]
    #[validate(nested)]
    pub rotation: Option<PatchLoggingFileRotationRequest>,
    #[serde(default)]
    #[validate(nested)]
    pub retention: Option<PatchLoggingFileRetentionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchLoggingFileRotationRequest {
    #[serde(default)]
    pub mode: Option<LoggingRotationMode>,
    #[serde(default)]
    pub time: Option<LoggingTimeRotation>,
    #[serde(default)]
    #[validate(range(min = 1))]
    pub size_mb: Option<u64>,
    #[serde(default)]
    #[validate(range(min = 1))]
    pub max_files: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchLoggingFileRetentionRequest {
    #[serde(default)]
    #[validate(custom(function = "validate_u64_le_u32_max_or_zero"))]
    pub max_days: Option<u64>,
    #[serde(default)]
    #[validate(custom(function = "validate_u64_le_i64_max_or_zero"))]
    pub max_total_size_mb: Option<u64>,
}

/// Logging cleanup policy view.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggingCleanupSettingsView {
    pub enabled: SettingField<bool>,
    pub interval_ms: SettingField<u64>,
}

/// Logging cleanup policy patch (partial update).
#[derive(Debug, Clone, Serialize, Deserialize, Validate)]
#[serde(rename_all = "camelCase")]
pub struct PatchLoggingCleanupSettingsRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    #[validate(range(min = 1))]
    pub interval_ms: Option<u64>,
}

#[inline]
fn validate_ttl_ms_or_zero(v: u64) -> Result<(), ValidationError> {
    if v == 0 {
        return Ok(());
    }
    if !(10_000..=86_400_000).contains(&v) {
        return Err(ValidationError::new("out_of_range"));
    }
    Ok(())
}

#[inline]
fn validate_non_empty_trimmed(v: &str) -> Result<(), ValidationError> {
    if v.trim().is_empty() {
        return Err(ValidationError::new("empty"));
    }
    Ok(())
}

#[inline]
fn validate_u64_le_u32_max_or_zero(v: u64) -> Result<(), ValidationError> {
    if v == 0 {
        return Ok(());
    }
    if v > (u32::MAX as u64) {
        return Err(ValidationError::new("too_large"));
    }
    Ok(())
}

#[inline]
fn validate_u64_le_i64_max_or_zero(v: u64) -> Result<(), ValidationError> {
    if v == 0 {
        return Ok(());
    }
    if v > (i64::MAX as u64) {
        return Err(ValidationError::new("too_large"));
    }
    Ok(())
}
