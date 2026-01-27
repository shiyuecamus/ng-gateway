use crate::{
    constants::{CERT_DIR, DATA_DIR, ENV_PREFIX, ENV_SEPARATOR},
    domain::prelude::{
        LoggingOutputFormat as ApiLoggingOutputFormat,
        LoggingRotationMode as ApiLoggingRotationMode,
        LoggingTimeRotation as ApiLoggingTimeRotation, PatchCollectorSettingsRequest,
        PatchLoggingCleanupSettingsRequest, PatchLoggingOutputSettingsRequest,
        PatchNorthwardSettingsRequest, PatchRetryPolicyRequest, PatchSouthwardSettingsRequest,
        RuntimeSettingKey,
    },
};
use arc_swap::ArcSwap;
use config::{Config, File};
use ng_gateway_error::NGResult;
use ng_gateway_sdk::RetryPolicy;
use serde::{self, Deserialize, Serialize};
use std::{
    ops::Deref,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
};
use sysinfo::System;

#[derive(Debug, Clone)]
pub struct Settings {
    inner: Arc<Inner>,
    /// The config path used to initialize this settings instance (typically `gateway.toml`).
    ///
    /// This is required for runtime apply + persistence (atomic rewrite).
    config_path: Arc<PathBuf>,
}

impl Deref for Settings {
    type Target = Inner;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl Settings {
    pub fn new(config_path: String) -> NGResult<Self> {
        let config_path_buf = PathBuf::from(config_path.clone());
        let builder = Config::builder()
            .add_source(File::with_name(config_path.as_str()).required(false)) // 加载文件配置
            .add_source(
                config::Environment::with_prefix(ENV_PREFIX)
                    .separator(ENV_SEPARATOR)
                    .try_parsing(true)
                    .list_separator(",") // list separator
                    .with_list_parse_key("db.cache.redis.cluster.nodes")
                    .with_list_parse_key("web.cors.whitelist.origins")
                    .with_list_parse_key("web.cors.whitelist.methods")
                    .with_list_parse_key("web.cors.whitelist.headers")
                    .with_list_parse_key("web.cors.whitelist.expose_headers"),
            );
        let inner: Inner = builder.build()?.try_deserialize()?;
        Ok(Self {
            inner: Arc::new(inner),
            config_path: Arc::new(config_path_buf),
        })
    }

    /// Returns the config path used to load settings (typically `gateway.toml`).
    #[inline]
    pub fn config_path(&self) -> &PathBuf {
        &self.config_path
    }
}

/// Atomic runtime setting wrapper for `u64` values.
///
/// # Design
/// - Clone is cheap and shares the same atomic value (`Arc<AtomicU64>`).
/// - Deserializes from a plain integer in config files and env vars.
#[derive(Debug, Clone)]
pub struct AtomicU64Setting(Arc<AtomicU64>);

impl AtomicU64Setting {
    #[inline]
    pub fn new(value: u64) -> Self {
        Self(Arc::new(AtomicU64::new(value)))
    }

    #[inline]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    #[inline]
    fn set(&self, value: u64) {
        self.0.store(value, Ordering::Relaxed);
    }
}

impl<'de> Deserialize<'de> for AtomicU64Setting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = u64::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

/// Atomic runtime setting wrapper for `usize` values.
#[derive(Debug, Clone)]
pub struct AtomicUsizeSetting(Arc<AtomicUsize>);

impl AtomicUsizeSetting {
    #[inline]
    pub fn new(value: usize) -> Self {
        Self(Arc::new(AtomicUsize::new(value)))
    }

    #[inline]
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }

    #[inline]
    fn set(&self, value: usize) {
        self.0.store(value, Ordering::Relaxed);
    }
}

impl<'de> Deserialize<'de> for AtomicUsizeSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = usize::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

/// Lock-free runtime setting wrapper for structured `RetryPolicy`.
///
/// # Design
/// - Clone is cheap (internally swaps an `Arc<RetryPolicy>`).
/// - Deserializes from the `RetryPolicy` TOML/env structure directly.
#[derive(Debug)]
pub struct RetryPolicySetting(ArcSwap<RetryPolicy>);

impl RetryPolicySetting {
    #[inline]
    pub fn new(value: RetryPolicy) -> Self {
        Self(ArcSwap::from_pointee(value))
    }

    #[inline]
    pub fn get(&self) -> RetryPolicy {
        *self.0.load().as_ref()
    }

    #[inline]
    fn set(&self, value: RetryPolicy) {
        self.0.store(Arc::new(value));
    }
}

impl Clone for RetryPolicySetting {
    #[inline]
    fn clone(&self) -> Self {
        Self(ArcSwap::new(self.0.load_full()))
    }
}

impl<'de> Deserialize<'de> for RetryPolicySetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = RetryPolicy::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Inner {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub logging: Logging,
    #[serde(default)]
    pub web: Web,
    #[serde(default)]
    pub db: Db,
    #[serde(default)]
    pub cache: Cache,
}

#[derive(Debug, Clone, Deserialize)]
pub struct General {
    /// Runtime root directory for all relative paths.
    ///
    /// # What this controls
    /// The gateway uses many relative paths by design (e.g. `./data`, `./drivers`, `./plugins`,
    /// `./certs`, `./pki`). This field defines the directory that those relative paths are
    /// resolved from by changing the process working directory at startup.
    ///
    /// # Best practice
    /// - Linux packages (systemd): keep `runtime_dir="."` and rely on `WorkingDirectory=...`.
    /// - Containers/K8s: set an absolute path (e.g. `/var/lib/ng-gateway`) via config or env.
    ///
    /// # Environment override
    /// - `NG__GENERAL__RUNTIME_DIR=/var/lib/ng-gateway`
    #[serde(default = "General::runtime_dir_default")]
    pub runtime_dir: String,
    #[serde(default = "General::ca_cert_path_default")]
    pub ca_cert_path: String,
    #[serde(default = "General::ca_key_path_default")]
    pub ca_key_path: String,
    /// Collection engine configuration
    #[serde(default)]
    pub collector: Collector,
    /// Northward manager configuration
    #[serde(default)]
    pub northward: Northward,
    /// Southward communication configuration
    #[serde(default)]
    pub southward: Southward,
}

impl Default for General {
    fn default() -> Self {
        General {
            runtime_dir: General::runtime_dir_default(),
            ca_cert_path: General::ca_cert_path_default(),
            ca_key_path: General::ca_key_path_default(),
            collector: Collector::default(),
            northward: Northward::default(),
            southward: Southward::default(),
        }
    }
}

/// Top-level logging configuration.
///
/// This matches the design doc structure:
/// - `[logging.control]`: log-level override runtime knobs
/// - `[logging.output]`: output pipeline configuration (format/file/rotation/retention)
/// - `[logging.cleanup]`: auto-clean task controls
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Logging {
    #[serde(default)]
    pub control: LoggingControl,
    #[serde(default)]
    pub output: LoggingOutputSetting,
    #[serde(default)]
    pub cleanup: LoggingCleanupSetting,
}

/// Log control runtime configuration (global + per-channel overrides).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingControl {
    #[serde(default = "LoggingControl::channel_override_default_ttl_ms_default")]
    pub channel_override_default_ttl_ms: u64,
    #[serde(default = "LoggingControl::channel_override_min_ttl_ms_default")]
    pub channel_override_min_ttl_ms: u64,
    #[serde(default = "LoggingControl::channel_override_max_ttl_ms_default")]
    pub channel_override_max_ttl_ms: u64,
    #[serde(default = "LoggingControl::override_cleanup_interval_ms_default")]
    pub override_cleanup_interval_ms: u64,
    #[serde(default = "LoggingControl::driver_ingest_queue_capacity_default")]
    pub driver_ingest_queue_capacity: usize,
}

impl Default for LoggingControl {
    fn default() -> Self {
        Self {
            channel_override_default_ttl_ms: Self::channel_override_default_ttl_ms_default(),
            channel_override_min_ttl_ms: Self::channel_override_min_ttl_ms_default(),
            channel_override_max_ttl_ms: Self::channel_override_max_ttl_ms_default(),
            override_cleanup_interval_ms: Self::override_cleanup_interval_ms_default(),
            driver_ingest_queue_capacity: Self::driver_ingest_queue_capacity_default(),
        }
    }
}

impl LoggingControl {
    fn channel_override_default_ttl_ms_default() -> u64 {
        5 * 60 * 1000
    }
    fn channel_override_min_ttl_ms_default() -> u64 {
        10 * 1000
    }
    fn channel_override_max_ttl_ms_default() -> u64 {
        30 * 60 * 1000
    }
    fn override_cleanup_interval_ms_default() -> u64 {
        5_000
    }
    fn driver_ingest_queue_capacity_default() -> usize {
        10_000
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoggingFormat {
    Text,
    Json,
}

impl LoggingFormat {
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

impl From<ApiLoggingOutputFormat> for LoggingFormat {
    fn from(v: ApiLoggingOutputFormat) -> Self {
        match v {
            ApiLoggingOutputFormat::Text => Self::Text,
            ApiLoggingOutputFormat::Json => Self::Json,
        }
    }
}

impl From<LoggingFormat> for ApiLoggingOutputFormat {
    fn from(v: LoggingFormat) -> Self {
        match v {
            LoggingFormat::Text => Self::Text,
            LoggingFormat::Json => Self::Json,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LoggingOutput {
    #[serde(default = "LoggingOutput::format_default")]
    pub format: LoggingFormat,
    #[serde(default = "LoggingOutput::include_span_fields_default")]
    pub include_span_fields: bool,
    #[serde(default)]
    pub file: LoggingFileOutput,
}

impl Default for LoggingOutput {
    fn default() -> Self {
        Self {
            format: Self::format_default(),
            include_span_fields: Self::include_span_fields_default(),
            file: LoggingFileOutput::default(),
        }
    }
}

impl LoggingOutput {
    fn format_default() -> LoggingFormat {
        LoggingFormat::Text
    }
    fn include_span_fields_default() -> bool {
        true
    }
}

/// Hot-swappable logging output setting (used by runtime tuning).
#[derive(Debug)]
pub struct LoggingOutputSetting(ArcSwap<LoggingOutput>);

impl LoggingOutputSetting {
    #[inline]
    pub fn new(v: LoggingOutput) -> Self {
        Self(ArcSwap::from_pointee(v))
    }

    #[inline]
    pub fn get(&self) -> LoggingOutput {
        self.0.load_full().as_ref().clone()
    }

    #[inline]
    pub fn set(&self, v: LoggingOutput) {
        self.0.store(Arc::new(v));
    }
}

impl Default for LoggingOutputSetting {
    fn default() -> Self {
        Self::new(LoggingOutput::default())
    }
}

impl Clone for LoggingOutputSetting {
    #[inline]
    fn clone(&self) -> Self {
        Self(ArcSwap::new(self.0.load_full()))
    }
}

impl<'de> Deserialize<'de> for LoggingOutputSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = LoggingOutput::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct LoggingFileOutput {
    #[serde(default = "LoggingFileOutput::enabled_default")]
    pub enabled: bool,
    #[serde(default = "LoggingFileOutput::dir_default")]
    pub dir: String,
    #[serde(default)]
    pub rotation: LoggingFileRotation,
    #[serde(default)]
    pub retention: LoggingFileRetention,
}

impl Default for LoggingFileOutput {
    fn default() -> Self {
        Self {
            enabled: Self::enabled_default(),
            dir: Self::dir_default(),
            rotation: LoggingFileRotation::default(),
            retention: LoggingFileRetention::default(),
        }
    }
}

impl LoggingFileOutput {
    fn enabled_default() -> bool {
        true
    }
    fn dir_default() -> String {
        "./logs".into()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RotationMode {
    Time,
    Size,
    Both,
}

impl RotationMode {
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Size => "size",
            Self::Both => "both",
        }
    }
}

impl From<ApiLoggingRotationMode> for RotationMode {
    fn from(v: ApiLoggingRotationMode) -> Self {
        match v {
            ApiLoggingRotationMode::Time => Self::Time,
            ApiLoggingRotationMode::Size => Self::Size,
            ApiLoggingRotationMode::Both => Self::Both,
        }
    }
}

impl From<RotationMode> for ApiLoggingRotationMode {
    fn from(v: RotationMode) -> Self {
        match v {
            RotationMode::Time => Self::Time,
            RotationMode::Size => Self::Size,
            RotationMode::Both => Self::Both,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimeRotation {
    Hourly,
    Daily,
}

impl TimeRotation {
    #[inline]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
        }
    }
}

impl From<ApiLoggingTimeRotation> for TimeRotation {
    fn from(v: ApiLoggingTimeRotation) -> Self {
        match v {
            ApiLoggingTimeRotation::Hourly => Self::Hourly,
            ApiLoggingTimeRotation::Daily => Self::Daily,
        }
    }
}

impl From<TimeRotation> for ApiLoggingTimeRotation {
    fn from(v: TimeRotation) -> Self {
        match v {
            TimeRotation::Hourly => Self::Hourly,
            TimeRotation::Daily => Self::Daily,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingFileRotation {
    #[serde(default = "LoggingFileRotation::mode_default")]
    pub mode: RotationMode,
    #[serde(default = "LoggingFileRotation::time_default")]
    pub time: TimeRotation,
    #[serde(default = "LoggingFileRotation::size_mb_default")]
    pub size_mb: u64,
    #[serde(default = "LoggingFileRotation::max_files_default")]
    pub max_files: usize,
}

impl Default for LoggingFileRotation {
    fn default() -> Self {
        Self {
            mode: Self::mode_default(),
            time: Self::time_default(),
            size_mb: Self::size_mb_default(),
            max_files: Self::max_files_default(),
        }
    }
}

impl LoggingFileRotation {
    fn mode_default() -> RotationMode {
        RotationMode::Both
    }
    fn time_default() -> TimeRotation {
        TimeRotation::Daily
    }
    fn size_mb_default() -> u64 {
        100
    }
    fn max_files_default() -> usize {
        200
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingFileRetention {
    #[serde(default = "LoggingFileRetention::max_days_default")]
    pub max_days: u32,
    #[serde(default = "LoggingFileRetention::max_total_size_mb_default")]
    pub max_total_size_mb: u64,
}

impl Default for LoggingFileRetention {
    fn default() -> Self {
        Self {
            max_days: Self::max_days_default(),
            max_total_size_mb: Self::max_total_size_mb_default(),
        }
    }
}

impl LoggingFileRetention {
    fn max_days_default() -> u32 {
        7
    }
    fn max_total_size_mb_default() -> u64 {
        2048
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoggingCleanup {
    #[serde(default = "LoggingCleanup::enabled_default")]
    pub enabled: bool,
    #[serde(default = "LoggingCleanup::interval_ms_default")]
    pub interval_ms: u64,
}

impl Default for LoggingCleanup {
    fn default() -> Self {
        Self {
            enabled: Self::enabled_default(),
            interval_ms: Self::interval_ms_default(),
        }
    }
}

impl LoggingCleanup {
    fn enabled_default() -> bool {
        true
    }
    fn interval_ms_default() -> u64 {
        60_000
    }
}

/// Hot-swappable logging cleanup setting (used by runtime tuning).
#[derive(Debug)]
pub struct LoggingCleanupSetting(ArcSwap<LoggingCleanup>);

impl LoggingCleanupSetting {
    #[inline]
    pub fn new(v: LoggingCleanup) -> Self {
        Self(ArcSwap::from_pointee(v))
    }

    #[inline]
    pub fn get(&self) -> LoggingCleanup {
        self.0.load_full().as_ref().clone()
    }

    #[inline]
    pub fn set(&self, v: LoggingCleanup) {
        self.0.store(Arc::new(v));
    }
}

impl Default for LoggingCleanupSetting {
    fn default() -> Self {
        Self::new(LoggingCleanup::default())
    }
}

impl Clone for LoggingCleanupSetting {
    #[inline]
    fn clone(&self) -> Self {
        Self(ArcSwap::new(self.0.load_full()))
    }
}

impl<'de> Deserialize<'de> for LoggingCleanupSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = LoggingCleanup::deserialize(deserializer)?;
        Ok(Self::new(v))
    }
}

impl Logging {
    /// Apply logging output patch at runtime (hot swap).
    pub fn apply_output_runtime_patch(
        &self,
        req: &PatchLoggingOutputSettingsRequest,
    ) -> Vec<RuntimeSettingKey> {
        let cur = self.output.get();
        let mut next = cur.clone();
        let mut changed: Vec<RuntimeSettingKey> = Vec::new();

        if let Some(v) = req.format {
            let mapped = LoggingFormat::from(v);
            if next.format != mapped {
                next.format = mapped;
                changed.push(RuntimeSettingKey::LoggingOutputFormat);
            }
        }
        if let Some(v) = req.include_span_fields {
            if next.include_span_fields != v {
                next.include_span_fields = v;
                changed.push(RuntimeSettingKey::LoggingOutputIncludeSpanFields);
            }
        }
        if let Some(file) = &req.file {
            if let Some(v) = file.enabled {
                if next.file.enabled != v {
                    next.file.enabled = v;
                    changed.push(RuntimeSettingKey::LoggingOutputFileEnabled);
                }
            }
            if let Some(v) = &file.dir {
                if next.file.dir != *v {
                    next.file.dir = v.clone();
                    changed.push(RuntimeSettingKey::LoggingOutputFileDir);
                }
            }
            if let Some(rotation) = &file.rotation {
                if let Some(v) = rotation.mode {
                    let mapped = RotationMode::from(v);
                    if next.file.rotation.mode != mapped {
                        next.file.rotation.mode = mapped;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRotationMode);
                    }
                }
                if let Some(v) = rotation.time {
                    let mapped = TimeRotation::from(v);
                    if next.file.rotation.time != mapped {
                        next.file.rotation.time = mapped;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRotationTime);
                    }
                }
                if let Some(v) = rotation.size_mb {
                    if next.file.rotation.size_mb != v {
                        next.file.rotation.size_mb = v;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRotationSizeMb);
                    }
                }
                if let Some(v) = rotation.max_files {
                    let v = v.min(usize::MAX as u64) as usize;
                    if next.file.rotation.max_files != v {
                        next.file.rotation.max_files = v;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRotationMaxFiles);
                    }
                }
            }
            if let Some(retention) = &file.retention {
                if let Some(v) = retention.max_days {
                    let v = v.min(u32::MAX as u64) as u32;
                    if next.file.retention.max_days != v {
                        next.file.retention.max_days = v;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRetentionMaxDays);
                    }
                }
                if let Some(v) = retention.max_total_size_mb {
                    if next.file.retention.max_total_size_mb != v {
                        next.file.retention.max_total_size_mb = v;
                        changed.push(RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb);
                    }
                }
            }
        }

        if !changed.is_empty() {
            self.output.set(next);
        }

        changed
    }

    /// Apply logging cleanup patch at runtime (hot swap).
    pub fn apply_cleanup_runtime_patch(
        &self,
        req: &PatchLoggingCleanupSettingsRequest,
    ) -> Vec<RuntimeSettingKey> {
        let cur = self.cleanup.get();
        let mut next = cur.clone();
        let mut changed: Vec<RuntimeSettingKey> = Vec::new();

        if let Some(v) = req.enabled {
            if next.enabled != v {
                next.enabled = v;
                changed.push(RuntimeSettingKey::LoggingCleanupEnabled);
            }
        }
        if let Some(v) = req.interval_ms {
            if next.interval_ms != v {
                next.interval_ms = v;
                changed.push(RuntimeSettingKey::LoggingCleanupIntervalMs);
            }
        }

        if !changed.is_empty() {
            self.cleanup.set(next);
        }
        changed
    }
}

impl General {
    fn runtime_dir_default() -> String {
        ".".into()
    }

    fn ca_cert_path_default() -> String {
        "ca.crt".into()
    }

    fn ca_key_path_default() -> String {
        "ca.key".into()
    }

    /// Resolve CA certificate path under the runtime root.
    ///
    /// # Rules
    /// - If the configured value looks like a path (contains `/` or starts with `.`),
    ///   it is treated as an explicit path and returned as-is.
    /// - Otherwise it is treated as a file name under `CERT_DIR` (default: `./certs`).
    pub fn ca_cert_path_resolved(&self) -> String {
        resolve_cert_path(&self.ca_cert_path)
    }

    /// Resolve CA private key path under the runtime root.
    ///
    /// See `ca_cert_path_resolved()` for the resolution rules.
    pub fn ca_key_path_resolved(&self) -> String {
        resolve_cert_path(&self.ca_key_path)
    }
}

/// Resolve a certificate-related path under `CERT_DIR` when a bare file name is provided.
fn resolve_cert_path(value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return v.to_string();
    }
    if v.starts_with('.') || v.contains('/') {
        return v.to_string();
    }
    format!("{}/{}", CERT_DIR, v)
}

#[derive(Debug, Clone, Deserialize)]
pub struct Collector {
    /// Collection timeout for each device (in milliseconds)
    #[serde(default = "Collector::collection_timeout_ms_default")]
    collection_timeout_ms: AtomicU64Setting,
    /// Maximum concurrent collections per channel
    #[serde(default = "Collector::max_concurrent_collections_default")]
    max_concurrent_collections: AtomicUsizeSetting,
    /// Retry policy for failed collections (unified with SDK retry semantics).
    #[serde(default = "Collector::retry_policy_default")]
    retry_policy: RetryPolicySetting,
    /// Outbound queue capacity from collector to gateway (bounded channel)
    #[serde(default = "Collector::outbound_queue_capacity_default")]
    outbound_queue_capacity: AtomicUsizeSetting,
}

impl Default for Collector {
    fn default() -> Self {
        Collector {
            collection_timeout_ms: Collector::collection_timeout_ms_default(),
            max_concurrent_collections: Collector::max_concurrent_collections_default(),
            retry_policy: Collector::retry_policy_default(),
            outbound_queue_capacity: Collector::outbound_queue_capacity_default(),
        }
    }
}

impl Collector {
    #[inline]
    pub fn collection_timeout_ms(&self) -> u64 {
        self.collection_timeout_ms.get()
    }

    #[inline]
    pub fn max_concurrent_collections(&self) -> usize {
        self.max_concurrent_collections.get()
    }

    #[inline]
    pub fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy.get()
    }

    #[inline]
    pub fn outbound_queue_capacity(&self) -> usize {
        self.outbound_queue_capacity.get()
    }

    /// Apply a runtime patch for supported collector fields.
    ///
    /// # Returns
    /// The list of keys that were actually changed (value differs).
    pub fn apply_runtime_patch(
        &self,
        patch: &PatchCollectorSettingsRequest,
    ) -> Vec<RuntimeSettingKey> {
        let mut changed = Vec::new();

        if let Some(v) = patch.collection_timeout_ms {
            if self.collection_timeout_ms() != v {
                self.collection_timeout_ms.set(v);
                changed.push(RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs);
            }
        }
        if let Some(v) = patch.max_concurrent_collections {
            let v = v as usize;
            if self.max_concurrent_collections() != v {
                self.max_concurrent_collections.set(v);
                changed.push(RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections);
            }
        }
        if let Some(v) = &patch.retry_policy {
            let cur = self.retry_policy();
            let next = apply_retry_policy_patch(cur, v);
            if cur != next {
                self.retry_policy.set(next);
                changed.extend(changed_retry_policy_keys(cur, next));
            }
        }
        if let Some(v) = patch.outbound_queue_capacity {
            let v = v as usize;
            if self.outbound_queue_capacity() != v {
                self.outbound_queue_capacity.set(v);
                changed.push(RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity);
            }
        }

        changed
    }

    fn collection_timeout_ms_default() -> AtomicU64Setting {
        AtomicU64Setting::new(30000)
    }

    fn max_concurrent_collections_default() -> AtomicUsizeSetting {
        AtomicUsizeSetting::new(200)
    }

    fn retry_policy_default() -> RetryPolicySetting {
        RetryPolicySetting::new(RetryPolicy::default())
    }

    fn outbound_queue_capacity_default() -> AtomicUsizeSetting {
        AtomicUsizeSetting::new(10000)
    }
}

#[inline]
fn apply_retry_policy_patch(mut cur: RetryPolicy, patch: &PatchRetryPolicyRequest) -> RetryPolicy {
    if let Some(v) = patch.max_attempts {
        cur.max_attempts = v;
    }
    if let Some(v) = patch.initial_interval_ms {
        cur.initial_interval_ms = v;
    }
    if let Some(v) = patch.max_interval_ms {
        cur.max_interval_ms = v;
    }
    if let Some(v) = patch.randomization_factor {
        cur.randomization_factor = v;
    }
    if let Some(v) = patch.multiplier {
        cur.multiplier = v;
    }
    if let Some(v) = patch.max_elapsed_time_ms {
        cur.max_elapsed_time_ms = v;
    }
    cur
}

#[inline]
fn changed_retry_policy_keys(old: RetryPolicy, new: RetryPolicy) -> Vec<RuntimeSettingKey> {
    let mut changed = Vec::new();
    if old.max_attempts != new.max_attempts {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts);
    }
    if old.initial_interval_ms != new.initial_interval_ms {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs);
    }
    if old.max_interval_ms != new.max_interval_ms {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs);
    }
    if old.randomization_factor != new.randomization_factor {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor);
    }
    if old.multiplier != new.multiplier {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier);
    }
    if old.max_elapsed_time_ms != new.max_elapsed_time_ms {
        changed.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs);
    }
    changed
}

#[derive(Debug, Clone, Deserialize)]
pub struct Web {
    #[serde(default = "Web::router_prefix_default")]
    pub router_prefix: String,
    #[serde(default = "Web::host_default")]
    pub host: String,
    #[serde(default = "Web::port_default")]
    pub port: u16,
    #[serde(default = "Web::workers_default")]
    pub workers: i32,
    /// Static UI (admin console) serving configuration.
    ///
    /// This enables a "single process / single container" distribution model:
    /// the gateway serves both API and the web UI on the same HTTP port.
    #[serde(default)]
    pub ui: WebUi,
    #[serde(default)]
    pub ssl: SSLWithCert,
    #[serde(default)]
    pub cors: Cors,
    #[serde(default)]
    pub jwt: Jwt,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            router_prefix: Web::router_prefix_default(),
            host: Web::host_default(),
            port: Web::port_default(),
            ui: Default::default(),
            ssl: Default::default(),
            cors: Default::default(),
            workers: Web::workers_default(),
            jwt: Default::default(),
        }
    }
}

impl Web {
    fn router_prefix_default() -> String {
        "/api".into()
    }

    fn port_default() -> u16 {
        5678
    }

    fn host_default() -> String {
        "0.0.0.0".into()
    }

    fn workers_default() -> i32 {
        0 // 默认使用 CPU 数量
    }

    /// Get actual number of workers based on configuration
    pub fn get_worker_count(&self) -> usize {
        match self.workers {
            0 => System::new_all().cpus().len(),
            n if n > 0 => n as usize,
            n => std::cmp::max(
                1,
                (System::new_all().cpus().len() as i32 / n.abs()) as usize,
            ),
        }
    }
}

/// Web UI serving configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct WebUi {
    /// Enable serving UI from the gateway process.
    #[serde(default = "WebUi::enabled_default")]
    pub enabled: bool,

    /// Serving mode for UI assets.
    #[serde(default)]
    pub mode: WebUiMode,

    /// Filesystem root directory for UI assets when `mode = "filesystem"`.
    ///
    /// Expected to contain `index.html` plus `css/`, `js/`... (Vite dist output).
    #[serde(default = "WebUi::filesystem_root_default")]
    pub filesystem_root: String,
}

impl Default for WebUi {
    fn default() -> Self {
        Self {
            enabled: WebUi::enabled_default(),
            mode: Default::default(),
            filesystem_root: WebUi::filesystem_root_default(),
        }
    }
}

impl WebUi {
    fn enabled_default() -> bool {
        // Safe default for dev/CI: don't assume UI assets are present.
        true
    }

    fn filesystem_root_default() -> String {
        // Convenient default for developer mode when building UI locally.
        "./ng-gateway-ui/apps/web-antd/dist".into()
    }
}

/// Web UI serving mode.
#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebUiMode {
    /// Serve from embedded zip (`ng-gateway-web/ui-dist.zip`) for single-binary distribution.
    EmbeddedZip,
    /// Serve from filesystem directory (`web.ui.filesystem_root`).
    #[default]
    Filesystem,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SSL {
    #[serde(default = "SSL::enabled_default")]
    pub enabled: bool,
    #[serde(default = "SSL::port_default")]
    pub port: u16,
}

impl Default for SSL {
    fn default() -> Self {
        SSL {
            enabled: SSL::enabled_default(),
            port: SSL::port_default(),
        }
    }
}

impl SSL {
    fn enabled_default() -> bool {
        false
    }
    fn port_default() -> u16 {
        8443
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SSLWithCert {
    #[serde(default = "SSLWithCert::enabled_default")]
    pub enabled: bool,
    #[serde(default = "SSLWithCert::port_default")]
    pub port: u16,
    #[serde(default)]
    pub r#type: SSLType,
    #[serde(default)]
    pub cert: String,
    #[serde(default)]
    pub key: String,
}

impl Default for SSLWithCert {
    fn default() -> Self {
        SSLWithCert {
            enabled: SSLWithCert::enabled_default(),
            port: SSLWithCert::port_default(),
            r#type: Default::default(),
            cert: Default::default(),
            key: Default::default(),
        }
    }
}

impl SSLWithCert {
    fn enabled_default() -> bool {
        false
    }

    fn port_default() -> u16 {
        5679
    }
}

#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SSLType {
    #[default]
    Auto,
    Local,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Cors {
    #[serde(default)]
    pub mode: CorsMode,
    #[serde(default)]
    pub whitelist: Whitelist,
}

#[derive(Default, Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorsMode {
    #[default]
    AllowAll,
    Whitelist,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Whitelist {
    #[serde(default = "Whitelist::origins_default")]
    pub origins: Vec<String>,
    #[serde(default = "Whitelist::methods_default")]
    pub methods: Vec<String>,
    #[serde(default = "Whitelist::headers_default")]
    pub headers: Vec<String>,
    #[serde(default = "Whitelist::expose_headers_default")]
    pub expose_headers: Vec<String>,
    #[serde(default = "Whitelist::credentials_default")]
    pub credentials: bool,
}

impl Default for Whitelist {
    fn default() -> Self {
        Whitelist {
            origins: Whitelist::origins_default(),
            methods: Whitelist::methods_default(),
            headers: Whitelist::headers_default(),
            expose_headers: Whitelist::expose_headers_default(),
            credentials: Whitelist::credentials_default(),
        }
    }
}

impl Whitelist {
    fn origins_default() -> Vec<String> {
        vec!["*".into()]
    }

    fn methods_default() -> Vec<String> {
        vec!["GET".into(), "POST".into(), "PUT".into(), "DELETE".into()]
    }

    fn headers_default() -> Vec<String> {
        vec![
            "Content-Type".into(),
            "AccessToken".into(),
            "X-CSRF-Token".into(),
            "Authorization".into(),
            "Token".into(),
            "X-Token".into(),
            "X-User-Id".into(),
        ]
    }

    fn expose_headers_default() -> Vec<String> {
        vec![
            "Content-Length".into(),
            "Access-Control-Allow-Origin".into(),
            "Access-Control-Allow-Headers".into(),
            "Content-Type".into(),
        ]
    }

    fn credentials_default() -> bool {
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwt {
    #[serde(default = "Jwt::secret_default")]
    pub secret: String,
    #[serde(default = "Jwt::expire_default")]
    pub expire: i64,
    #[serde(default = "Jwt::issuer_default")]
    pub issuer: String,
}

impl Default for Jwt {
    fn default() -> Self {
        Jwt {
            secret: Jwt::secret_default(),
            expire: Jwt::expire_default(),
            issuer: Jwt::issuer_default(),
        }
    }
}

impl Jwt {
    fn secret_default() -> String {
        "ng-gateway".into()
    }

    fn expire_default() -> i64 {
        3_600_000
    }

    fn issuer_default() -> String {
        "ng-gateway".into()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Db {
    #[serde(default)]
    pub sqlite: Sqlite,
}

/// SQLite database type enum
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SqlType {
    #[default]
    Sqlite,
}

/// NGDbConfig is a trait that defines the necessary methods for database configuration.
/// It includes methods to get the database file path and connection URL for SQLite.
pub trait NGDbConfig: Send + Sync {
    /// Returns the type of SQL database.
    fn db_type(&self) -> SqlType;

    /// Returns the database file path.
    fn db_path(&self) -> String;

    /// Generates a URL for the database connection.
    fn to_url(&self) -> String;

    /// Returns the directory containing the database file.
    fn db_dir(&self) -> String;
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sqlite {
    #[serde(default = "Sqlite::path_default")]
    pub path: String,
    #[serde(default = "Sqlite::timeout_default")]
    pub timeout: u64,
    #[serde(default = "Sqlite::idle_timeout_default")]
    pub idle_timeout: u64,
    #[serde(default = "Sqlite::max_lifetime_default")]
    pub max_lifetime: u64,
    #[serde(default = "Sqlite::max_connections_default")]
    pub max_connections: u32,
    #[serde(default = "Sqlite::auto_create_default")]
    pub auto_create: bool,
}

impl Default for Sqlite {
    fn default() -> Self {
        Sqlite {
            path: Sqlite::path_default(),
            timeout: Sqlite::timeout_default(),
            idle_timeout: Sqlite::idle_timeout_default(),
            max_lifetime: Sqlite::max_lifetime_default(),
            max_connections: Sqlite::max_connections_default(),
            auto_create: Sqlite::auto_create_default(),
        }
    }
}

impl NGDbConfig for Sqlite {
    fn db_type(&self) -> SqlType {
        SqlType::Sqlite
    }

    fn db_path(&self) -> String {
        self.path.clone()
    }

    fn to_url(&self) -> String {
        if self.auto_create {
            // Use mode=rwc to automatically create file if it doesn't exist
            // r = read, w = write, c = create
            format!("sqlite:{}/{}?mode=rwc", DATA_DIR, self.path)
        } else {
            format!("sqlite:{}/{}", DATA_DIR, self.path)
        }
    }

    fn db_dir(&self) -> String {
        DATA_DIR.into()
    }
}

impl Sqlite {
    fn path_default() -> String {
        "ng-gateway.db".into()
    }

    fn timeout_default() -> u64 {
        5000
    }

    fn idle_timeout_default() -> u64 {
        5000
    }

    fn max_lifetime_default() -> u64 {
        5000
    }

    fn max_connections_default() -> u32 {
        100
    }

    fn auto_create_default() -> bool {
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cache {
    #[serde(default)]
    pub r#type: CacheType, // only "moka"
    #[serde(default = "Cache::prefix_default")]
    pub prefix: String,
    #[serde(default = "Cache::delimiter_default")]
    pub delimiter: String,
}

impl Default for Cache {
    fn default() -> Self {
        Cache {
            r#type: Default::default(),
            prefix: Cache::prefix_default(),
            delimiter: Cache::delimiter_default(),
        }
    }
}

impl Cache {
    fn prefix_default() -> String {
        "ng".into()
    }

    fn delimiter_default() -> String {
        ":".into()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CacheType {
    #[default]
    Moka,
}

/// Southward communication configuration
#[derive(Debug, Clone, Deserialize)]
pub struct Southward {
    /// API synchronous start wait timeout for driver connection (milliseconds)
    #[serde(default = "Southward::start_timeout_ms_default")]
    start_timeout_ms: AtomicU64Setting,
    /// Point baseline TTL in milliseconds. `0` disables eviction.
    #[serde(default = "Southward::device_change_cache_ttl_ms_default")]
    device_change_cache_ttl_ms: AtomicU64Setting,
    /// Snapshot GC scan interval in milliseconds.
    #[serde(default = "Southward::snapshot_gc_interval_ms_default")]
    snapshot_gc_interval_ms: AtomicU64Setting,
    /// Number of async snapshot GC workers.
    #[serde(default = "Southward::snapshot_gc_workers_default")]
    snapshot_gc_workers: AtomicUsizeSetting,
    /// Max devices to scan per snapshot GC tick.
    #[serde(default = "Southward::max_devices_per_snapshot_tick_default")]
    max_devices_per_snapshot_tick: AtomicUsizeSetting,
}

impl Default for Southward {
    fn default() -> Self {
        Self {
            start_timeout_ms: Southward::start_timeout_ms_default(),
            device_change_cache_ttl_ms: Southward::device_change_cache_ttl_ms_default(),
            snapshot_gc_interval_ms: Southward::snapshot_gc_interval_ms_default(),
            snapshot_gc_workers: Southward::snapshot_gc_workers_default(),
            max_devices_per_snapshot_tick: Southward::max_devices_per_snapshot_tick_default(),
        }
    }
}

impl Southward {
    #[inline]
    pub fn start_timeout_ms(&self) -> u64 {
        self.start_timeout_ms.get()
    }

    #[inline]
    pub fn device_change_cache_ttl_ms(&self) -> u64 {
        self.device_change_cache_ttl_ms.get()
    }

    #[inline]
    pub fn snapshot_gc_interval_ms(&self) -> u64 {
        self.snapshot_gc_interval_ms.get()
    }

    #[inline]
    pub fn snapshot_gc_workers(&self) -> usize {
        self.snapshot_gc_workers.get()
    }

    #[inline]
    pub fn max_devices_per_snapshot_tick(&self) -> usize {
        self.max_devices_per_snapshot_tick.get()
    }

    #[inline]
    pub fn apply_runtime_patch(
        &self,
        patch: &PatchSouthwardSettingsRequest,
    ) -> Vec<RuntimeSettingKey> {
        let mut changed = Vec::new();
        if let Some(v) = patch.start_timeout_ms {
            if self.start_timeout_ms() != v {
                self.start_timeout_ms.set(v);
                changed.push(RuntimeSettingKey::GeneralSouthwardStartTimeoutMs);
            }
        }
        if let Some(v) = patch.device_change_cache_ttl_ms {
            self.device_change_cache_ttl_ms.set(v);
            changed.push(RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs);
        }
        if let Some(v) = patch.snapshot_gc_interval_ms {
            self.snapshot_gc_interval_ms.set(v);
            changed.push(RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs);
        }
        if let Some(v) = patch.snapshot_gc_workers {
            self.snapshot_gc_workers
                .set(v.min(usize::MAX as u64) as usize);
            changed.push(RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers);
        }
        if let Some(v) = patch.max_devices_per_snapshot_tick {
            self.max_devices_per_snapshot_tick
                .set(v.min(usize::MAX as u64) as usize);
            changed.push(RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick);
        }
        changed
    }

    fn start_timeout_ms_default() -> AtomicU64Setting {
        AtomicU64Setting::new(5000)
    }

    fn device_change_cache_ttl_ms_default() -> AtomicU64Setting {
        AtomicU64Setting::new(10 * 60 * 1000)
    }
    fn snapshot_gc_interval_ms_default() -> AtomicU64Setting {
        AtomicU64Setting::new(60 * 1000)
    }
    fn snapshot_gc_workers_default() -> AtomicUsizeSetting {
        AtomicUsizeSetting::new(2)
    }
    fn max_devices_per_snapshot_tick_default() -> AtomicUsizeSetting {
        AtomicUsizeSetting::new(256)
    }
}

/// Northward communication configuration
#[derive(Debug, Clone, Deserialize)]
pub struct Northward {
    /// Internal bounded queue capacity for northward manager
    #[serde(default = "Northward::queue_capacity_default")]
    queue_capacity: AtomicUsizeSetting,
    /// API synchronous start wait timeout for northward app (milliseconds)
    #[serde(default = "Northward::start_timeout_ms_default")]
    start_timeout_ms: AtomicU64Setting,
}

impl Default for Northward {
    fn default() -> Self {
        Self {
            queue_capacity: Northward::queue_capacity_default(),
            start_timeout_ms: Northward::start_timeout_ms_default(),
        }
    }
}

impl Northward {
    #[inline]
    pub fn queue_capacity(&self) -> usize {
        self.queue_capacity.get()
    }

    #[inline]
    pub fn apply_runtime_patch(
        &self,
        patch: &PatchNorthwardSettingsRequest,
    ) -> Vec<RuntimeSettingKey> {
        let mut changed = Vec::new();
        if let Some(v) = patch.queue_capacity {
            let v = v as usize;
            if self.queue_capacity() != v {
                self.queue_capacity.set(v);
                changed.push(RuntimeSettingKey::GeneralNorthwardQueueCapacity);
            }
        }
        if let Some(v) = patch.start_timeout_ms {
            if self.start_timeout_ms() != v {
                self.start_timeout_ms.set(v);
                changed.push(RuntimeSettingKey::GeneralNorthwardStartTimeoutMs);
            }
        }
        changed
    }

    #[inline]
    pub fn start_timeout_ms(&self) -> u64 {
        self.start_timeout_ms.get()
    }

    fn queue_capacity_default() -> AtomicUsizeSetting {
        AtomicUsizeSetting::new(10000)
    }

    fn start_timeout_ms_default() -> AtomicU64Setting {
        AtomicU64Setting::new(5000)
    }
}
