use crate::log::control::{self as log_control, LogControlSettings};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::constants::{DEFAULT_CONFIG_FILE_NAME, ENV_PREFIX, ENV_SEPARATOR};
use ng_gateway_models::domain::prelude::{
    ApplySystemSettingsResult, CollectorSettingsView, LoggingCleanupSettingsView,
    LoggingControlSettingsView, LoggingFileOutputSettingsView, LoggingFileRetentionSettingsView,
    LoggingFileRotationSettingsView, LoggingOutputFormat, LoggingOutputSettingsView,
    LoggingRotationMode, LoggingTimeRotation, NorthwardSettingsView, PatchCollectorSettingsRequest,
    PatchLoggingCleanupSettingsRequest, PatchLoggingControlSettingsRequest,
    PatchLoggingOutputSettingsRequest, PatchNorthwardSettingsRequest, PatchRetryPolicyRequest,
    PatchSouthwardSettingsRequest, RetryPolicySettingsView, RuntimeSettingKey, SettingField,
    SettingValueSource, SouthwardSettingsView, SystemSettingsDomain, SystemSettingsImpact,
};
use ng_gateway_models::settings::Settings;
use ng_gateway_sdk::RetryPolicy;
use std::{fs, path::Path};
use toml_edit::{value as toml_value, DocumentMut, Item};

#[inline]
fn key_path_segments(key: RuntimeSettingKey) -> &'static [&'static str] {
    match key {
        RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs => {
            &["general", "collector", "collection_timeout_ms"]
        }
        RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections => {
            &["general", "collector", "max_concurrent_collections"]
        }
        RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts => {
            &["general", "collector", "retry_policy", "max_attempts"]
        }
        RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs => &[
            "general",
            "collector",
            "retry_policy",
            "initial_interval_ms",
        ],
        RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs => {
            &["general", "collector", "retry_policy", "max_interval_ms"]
        }
        RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor => &[
            "general",
            "collector",
            "retry_policy",
            "randomization_factor",
        ],
        RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier => {
            &["general", "collector", "retry_policy", "multiplier"]
        }
        RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs => &[
            "general",
            "collector",
            "retry_policy",
            "max_elapsed_time_ms",
        ],
        RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity => {
            &["general", "collector", "outbound_queue_capacity"]
        }
        RuntimeSettingKey::GeneralNorthwardQueueCapacity => {
            &["general", "northward", "queue_capacity"]
        }
        RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs => {
            &["logging", "control", "channel_override_default_ttl_ms"]
        }
        RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs => {
            &["logging", "control", "channel_override_min_ttl_ms"]
        }
        RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs => {
            &["logging", "control", "channel_override_max_ttl_ms"]
        }
        RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs => {
            &["logging", "control", "override_cleanup_interval_ms"]
        }
        RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity => {
            &["logging", "control", "driver_ingest_queue_capacity"]
        }
        RuntimeSettingKey::LoggingOutputFormat => &["logging", "output", "format"],
        RuntimeSettingKey::LoggingOutputIncludeSpanFields => {
            &["logging", "output", "include_span_fields"]
        }
        RuntimeSettingKey::LoggingOutputFileEnabled => &["logging", "output", "file", "enabled"],
        RuntimeSettingKey::LoggingOutputFileDir => &["logging", "output", "file", "dir"],
        RuntimeSettingKey::LoggingOutputFileRotationMode => {
            &["logging", "output", "file", "rotation", "mode"]
        }
        RuntimeSettingKey::LoggingOutputFileRotationTime => {
            &["logging", "output", "file", "rotation", "time"]
        }
        RuntimeSettingKey::LoggingOutputFileRotationSizeMb => {
            &["logging", "output", "file", "rotation", "size_mb"]
        }
        RuntimeSettingKey::LoggingOutputFileRotationMaxFiles => {
            &["logging", "output", "file", "rotation", "max_files"]
        }
        RuntimeSettingKey::LoggingOutputFileRetentionMaxDays => {
            &["logging", "output", "file", "retention", "max_days"]
        }
        RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb => &[
            "logging",
            "output",
            "file",
            "retention",
            "max_total_size_mb",
        ],
        RuntimeSettingKey::LoggingCleanupEnabled => &["logging", "cleanup", "enabled"],
        RuntimeSettingKey::LoggingCleanupIntervalMs => &["logging", "cleanup", "interval_ms"],
        RuntimeSettingKey::GeneralSouthwardStartTimeoutMs => {
            &["general", "southward", "start_timeout_ms"]
        }
        RuntimeSettingKey::GeneralNorthwardStartTimeoutMs => {
            &["general", "northward", "start_timeout_ms"]
        }
        RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs => {
            &["general", "southward", "device_change_cache_ttl_ms"]
        }
        RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs => {
            &["general", "southward", "snapshot_gc_interval_ms"]
        }
        RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers => {
            &["general", "southward", "snapshot_gc_workers"]
        }
        RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick => {
            &["general", "southward", "max_devices_per_snapshot_tick"]
        }
    }
}

#[inline]
fn derive_env_key(key: RuntimeSettingKey) -> String {
    let segs = key_path_segments(key);
    let mut s = String::with_capacity(64);
    s.push_str(ENV_PREFIX);
    for seg in segs {
        s.push_str(ENV_SEPARATOR);
        s.push_str(&seg.to_ascii_uppercase());
    }
    s
}

#[inline]
fn env_overridden(key: RuntimeSettingKey) -> bool {
    std::env::var_os(derive_env_key(key)).is_some()
}

#[inline]
fn read_gateway_toml_doc(path: &Path) -> NGResult<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(s) => s
            .parse::<DocumentMut>()
            .map_err(|e| NGError::from(format!("Failed to parse {DEFAULT_CONFIG_FILE_NAME}: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(NGError::from(format!(
            "Failed to read {DEFAULT_CONFIG_FILE_NAME} {}: {e}",
            path.display()
        ))),
    }
}

/// Returns true if the TOML document contains a value at the given segmented key path.
#[inline]
fn toml_contains_path(doc: &DocumentMut, path: &[&str]) -> bool {
    if path.is_empty() {
        return false;
    }
    let mut cur: &Item = doc.as_item();
    for (i, seg) in path.iter().enumerate() {
        match cur.get(seg) {
            Some(next) => {
                if i == path.len() - 1 {
                    return !next.is_none();
                }
                cur = next;
            }
            None => return false,
        }
    }
    false
}

/// Set an integer value at the given segmented key path, creating intermediate tables as needed.
#[inline]
fn toml_set_int(doc: &mut DocumentMut, path: &[&str], v: i64) {
    if path.is_empty() {
        return;
    }
    let mut cur = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        if i == path.len() - 1 {
            cur[*seg] = toml_value(v);
            return;
        }
        if !cur.contains_key(seg) {
            cur[*seg] = toml_edit::table();
        }
        // Ensure intermediate segments are tables.
        if cur[*seg].as_table().is_none() {
            cur[*seg] = toml_edit::table();
        }
        // After forcing a table, `as_table_mut` should succeed. Keep this branch safe anyway.
        let next = cur[*seg].as_table_mut();
        if let Some(t) = next {
            cur = t;
        } else {
            return;
        }
    }
}

/// Set a float value at the given segmented key path, creating intermediate tables as needed.
#[inline]
fn toml_set_float(doc: &mut DocumentMut, path: &[&str], v: f64) {
    if path.is_empty() {
        return;
    }
    let mut cur = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        if i == path.len() - 1 {
            cur[*seg] = toml_value(v);
            return;
        }
        if !cur.contains_key(seg) {
            cur[*seg] = toml_edit::table();
        }
        if cur[*seg].as_table().is_none() {
            cur[*seg] = toml_edit::table();
        }
        let next = cur[*seg].as_table_mut();
        if let Some(t) = next {
            cur = t;
        } else {
            return;
        }
    }
}

/// Set a bool value at the given segmented key path, creating intermediate tables as needed.
#[inline]
fn toml_set_bool(doc: &mut DocumentMut, path: &[&str], v: bool) {
    if path.is_empty() {
        return;
    }
    let mut cur = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        if i == path.len() - 1 {
            cur[*seg] = toml_value(v);
            return;
        }
        if !cur.contains_key(seg) {
            cur[*seg] = toml_edit::table();
        }
        if cur[*seg].as_table().is_none() {
            cur[*seg] = toml_edit::table();
        }
        let next = cur[*seg].as_table_mut();
        if let Some(t) = next {
            cur = t;
        } else {
            return;
        }
    }
}

/// Set a string value at the given segmented key path, creating intermediate tables as needed.
#[inline]
fn toml_set_string(doc: &mut DocumentMut, path: &[&str], v: &str) {
    if path.is_empty() {
        return;
    }
    let mut cur = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        if i == path.len() - 1 {
            cur[*seg] = toml_value(v);
            return;
        }
        if !cur.contains_key(seg) {
            cur[*seg] = toml_edit::table();
        }
        if cur[*seg].as_table().is_none() {
            cur[*seg] = toml_edit::table();
        }
        let next = cur[*seg].as_table_mut();
        if let Some(t) = next {
            cur = t;
        } else {
            return;
        }
    }
}

/// Remove a key at the given segmented path (best-effort).
#[inline]
fn toml_remove(doc: &mut DocumentMut, path: &[&str]) {
    if path.is_empty() {
        return;
    }
    let mut cur = doc.as_table_mut();
    for (i, seg) in path.iter().enumerate() {
        if i == path.len() - 1 {
            cur.remove(seg);
            return;
        }
        if cur[*seg].as_table().is_none() {
            return;
        }
        let next = cur[*seg].as_table_mut();
        if let Some(t) = next {
            cur = t;
        } else {
            return;
        }
    }
}

#[inline]
fn setting_field_u64(doc: &DocumentMut, key: RuntimeSettingKey, value: u64) -> SettingField<u64> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_f64(doc: &DocumentMut, key: RuntimeSettingKey, value: f64) -> SettingField<f64> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_opt_u32(
    doc: &DocumentMut,
    key: RuntimeSettingKey,
    value: Option<u32>,
) -> SettingField<Option<u32>> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_opt_u64(
    doc: &DocumentMut,
    key: RuntimeSettingKey,
    value: Option<u64>,
) -> SettingField<Option<u64>> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_bool(
    doc: &DocumentMut,
    key: RuntimeSettingKey,
    value: bool,
) -> SettingField<bool> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_string(
    doc: &DocumentMut,
    key: RuntimeSettingKey,
    value: String,
) -> SettingField<String> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn setting_field_copy<T: Copy>(
    doc: &DocumentMut,
    key: RuntimeSettingKey,
    value: T,
) -> SettingField<T> {
    let overridden = env_overridden(key);
    let source = if overridden {
        SettingValueSource::Env
    } else if toml_contains_path(doc, key_path_segments(key)) {
        SettingValueSource::File
    } else {
        SettingValueSource::Default
    };
    SettingField {
        value,
        source,
        env_overridden: overridden,
        env_key: overridden.then(|| derive_env_key(key)),
    }
}

#[inline]
fn apply_retry_policy_patch(cur: RetryPolicy, patch: &PatchRetryPolicyRequest) -> RetryPolicy {
    let mut next = cur;
    if let Some(v) = patch.max_attempts {
        next.max_attempts = v;
    }
    if let Some(v) = patch.initial_interval_ms {
        next.initial_interval_ms = v;
    }
    if let Some(v) = patch.max_interval_ms {
        next.max_interval_ms = v;
    }
    if let Some(v) = patch.randomization_factor {
        next.randomization_factor = v;
    }
    if let Some(v) = patch.multiplier {
        next.multiplier = v;
    }
    if let Some(v) = patch.max_elapsed_time_ms {
        next.max_elapsed_time_ms = v;
    }
    next
}

#[inline]
fn validate_retry_policy_patch(_cur: RetryPolicy, next: RetryPolicy) -> NGResult<()> {
    if next.initial_interval_ms == 0 {
        return Err(NGError::from(
            "retry_policy.initial_interval_ms must be > 0",
        ));
    }
    if next.max_interval_ms == 0 {
        return Err(NGError::from("retry_policy.max_interval_ms must be > 0"));
    }
    if next.max_interval_ms < next.initial_interval_ms {
        return Err(NGError::from(
            "retry_policy.max_interval_ms must be >= retry_policy.initial_interval_ms",
        ));
    }
    if !(0.0..=1.0).contains(&next.randomization_factor) {
        return Err(NGError::from(
            "retry_policy.randomization_factor must be within [0.0, 1.0]",
        ));
    }
    if next.multiplier < 1.0 {
        return Err(NGError::from("retry_policy.multiplier must be >= 1.0"));
    }
    if let Some(ms) = next.max_elapsed_time_ms {
        if ms == 0 {
            return Err(NGError::from(
                "retry_policy.max_elapsed_time_ms must be > 0",
            ));
        }
    }
    Ok(())
}

fn atomic_rewrite(path: &Path, content: &str) -> NGResult<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(DEFAULT_CONFIG_FILE_NAME);

    let tmp_path = dir.join(format!("{file_name}.tmp"));
    let bak_path = dir.join(format!("{file_name}.bak"));

    // Best-effort backup (optional).
    if path.exists() {
        if let Ok(existing) = fs::read(path) {
            let _ = fs::write(&bak_path, existing);
        }
    }

    fs::write(&tmp_path, content).map_err(|e| {
        NGError::from(format!(
            "Failed to write tmp config {}: {e}",
            tmp_path.display()
        ))
    })?;

    // Atomic replace on same filesystem.
    fs::rename(&tmp_path, path).map_err(|e| {
        NGError::from(format!(
            "Failed to atomically rename {} -> {}: {e}",
            tmp_path.display(),
            path.display()
        ))
    })?;

    Ok(())
}

/// Build collector settings view with value + source + env override.
pub fn build_collector_view(settings: &Settings) -> NGResult<CollectorSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    let retry_policy = settings.general.collector.retry_policy();

    Ok(CollectorSettingsView {
        collection_timeout_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs,
            settings.general.collector.collection_timeout_ms(),
        ),
        max_concurrent_collections: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections,
            settings.general.collector.max_concurrent_collections() as u64,
        ),
        retry_policy: RetryPolicySettingsView {
            max_attempts: setting_field_opt_u32(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts,
                retry_policy.max_attempts,
            ),
            initial_interval_ms: setting_field_u64(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs,
                retry_policy.initial_interval_ms,
            ),
            max_interval_ms: setting_field_u64(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs,
                retry_policy.max_interval_ms,
            ),
            randomization_factor: setting_field_f64(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor,
                retry_policy.randomization_factor,
            ),
            multiplier: setting_field_f64(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier,
                retry_policy.multiplier,
            ),
            max_elapsed_time_ms: setting_field_opt_u64(
                &doc,
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs,
                retry_policy.max_elapsed_time_ms,
            ),
        },
        outbound_queue_capacity: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity,
            settings.general.collector.outbound_queue_capacity() as u64,
        ),
    })
}

/// Build northward settings view with value + source + env override.
pub fn build_northward_view(settings: &Settings) -> NGResult<NorthwardSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    Ok(NorthwardSettingsView {
        queue_capacity: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralNorthwardQueueCapacity,
            settings.general.northward.queue_capacity() as u64,
        ),
        start_timeout_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralNorthwardStartTimeoutMs,
            settings.general.northward.start_timeout_ms(),
        ),
    })
}

/// Build southward settings view (value + source + env override).
pub fn build_southward_view(settings: &Settings) -> NGResult<SouthwardSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    Ok(SouthwardSettingsView {
        start_timeout_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralSouthwardStartTimeoutMs,
            settings.general.southward.start_timeout_ms(),
        ),
        device_change_cache_ttl_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs,
            settings.general.southward.device_change_cache_ttl_ms(),
        ),
        snapshot_gc_interval_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs,
            settings.general.southward.snapshot_gc_interval_ms(),
        ),
        snapshot_gc_workers: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers,
            settings.general.southward.snapshot_gc_workers() as u64,
        ),
        max_devices_per_snapshot_tick: setting_field_u64(
            &doc,
            RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick,
            settings.general.southward.max_devices_per_snapshot_tick() as u64,
        ),
    })
}

/// Build logging output settings view (value + source + env override).
pub fn build_logging_output_view(settings: &Settings) -> NGResult<LoggingOutputSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    let output = settings.logging.output.get();

    let format: LoggingOutputFormat = output.format.into();
    let rotation_mode: LoggingRotationMode = output.file.rotation.mode.into();
    let rotation_time: LoggingTimeRotation = output.file.rotation.time.into();

    Ok(LoggingOutputSettingsView {
        format: setting_field_copy(&doc, RuntimeSettingKey::LoggingOutputFormat, format),
        include_span_fields: setting_field_bool(
            &doc,
            RuntimeSettingKey::LoggingOutputIncludeSpanFields,
            output.include_span_fields,
        ),
        file: LoggingFileOutputSettingsView {
            enabled: setting_field_bool(
                &doc,
                RuntimeSettingKey::LoggingOutputFileEnabled,
                output.file.enabled,
            ),
            dir: setting_field_string(
                &doc,
                RuntimeSettingKey::LoggingOutputFileDir,
                output.file.dir.clone(),
            ),
            rotation: LoggingFileRotationSettingsView {
                mode: setting_field_copy(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRotationMode,
                    rotation_mode,
                ),
                time: setting_field_copy(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRotationTime,
                    rotation_time,
                ),
                size_mb: setting_field_u64(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRotationSizeMb,
                    output.file.rotation.size_mb,
                ),
                max_files: setting_field_u64(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRotationMaxFiles,
                    output.file.rotation.max_files as u64,
                ),
            },
            retention: LoggingFileRetentionSettingsView {
                max_days: setting_field_u64(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRetentionMaxDays,
                    output.file.retention.max_days as u64,
                ),
                max_total_size_mb: setting_field_u64(
                    &doc,
                    RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb,
                    output.file.retention.max_total_size_mb,
                ),
            },
        },
    })
}

/// Build logging cleanup policy view (value + source + env override).
pub fn build_logging_cleanup_view(settings: &Settings) -> NGResult<LoggingCleanupSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    let cleanup = settings.logging.cleanup.get();
    Ok(LoggingCleanupSettingsView {
        enabled: setting_field_bool(
            &doc,
            RuntimeSettingKey::LoggingCleanupEnabled,
            cleanup.enabled,
        ),
        interval_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingCleanupIntervalMs,
            cleanup.interval_ms,
        ),
    })
}

/// Build logging control settings view (override TTL policy).
///
/// # Data source
/// - Effective values come from the log-control runtime (hot-applied).
/// - `source/envOverridden/envKey` are derived from env vars + gateway.toml.
pub fn build_logging_control_view(
    settings: &Settings,
    current: LogControlSettings,
) -> NGResult<LoggingControlSettingsView> {
    let doc = read_gateway_toml_doc(settings.config_path())?;
    Ok(LoggingControlSettingsView {
        channel_override_default_ttl_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs,
            current.channel_override_default_ttl_ms,
        ),
        channel_override_min_ttl_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs,
            current.channel_override_min_ttl_ms,
        ),
        channel_override_max_ttl_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs,
            current.channel_override_max_ttl_ms,
        ),
        override_cleanup_interval_ms: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs,
            current.override_cleanup_interval_ms,
        ),
        driver_ingest_queue_capacity: setting_field_u64(
            &doc,
            RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity,
            current.driver_ingest_queue_capacity as u64,
        ),
    })
}

/// Apply + persist logging control settings (override TTL policy).
///
/// # Semantics
/// - Hot-applies to the log-control runtime immediately
/// - Persists changed keys to `gateway.toml`
pub fn apply_logging_control_settings(
    settings: &Settings,
    mut req: PatchLoggingControlSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();
    if req.channel_override_default_ttl_ms.is_some()
        && env_overridden(RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs);
        req.channel_override_default_ttl_ms = None;
    }
    if req.channel_override_min_ttl_ms.is_some()
        && env_overridden(RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs);
        req.channel_override_min_ttl_ms = None;
    }
    if req.channel_override_max_ttl_ms.is_some()
        && env_overridden(RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs);
        req.channel_override_max_ttl_ms = None;
    }
    if req.override_cleanup_interval_ms.is_some()
        && env_overridden(RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs);
        req.override_cleanup_interval_ms = None;
    }
    if req.driver_ingest_queue_capacity.is_some()
        && env_overridden(RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity);
        req.driver_ingest_queue_capacity = None;
    }

    // No-op fast path.
    if req.channel_override_default_ttl_ms.is_none()
        && req.channel_override_min_ttl_ms.is_none()
        && req.channel_override_max_ttl_ms.is_none()
        && req.override_cleanup_interval_ms.is_none()
        && req.driver_ingest_queue_capacity.is_none()
    {
        return Ok(ApplySystemSettingsResult {
            applied: true,
            persisted: true,
            domain: SystemSettingsDomain::LoggingControl,
            changed_keys: Vec::new(),
            blocked_by_env,
            persistence_warning: None,
            runtime_warning: None,
            impact: SystemSettingsImpact::HotApply,
            restart_targets: Vec::new(),
        });
    }

    let rt = log_control::global().ok_or_else(|| {
        NGError::from("Log control runtime is not initialized (cannot apply logging_control)")
    })?;

    let cur = rt.settings();
    let mut next = cur;

    if let Some(v) = req.channel_override_default_ttl_ms {
        next.channel_override_default_ttl_ms = v;
    }
    if let Some(v) = req.channel_override_min_ttl_ms {
        next.channel_override_min_ttl_ms = v;
    }
    if let Some(v) = req.channel_override_max_ttl_ms {
        next.channel_override_max_ttl_ms = v;
    }
    if let Some(v) = req.override_cleanup_interval_ms {
        next.override_cleanup_interval_ms = v;
    }
    if let Some(v) = req.driver_ingest_queue_capacity {
        // Avoid usize overflow on 32-bit targets (defensive).
        next.driver_ingest_queue_capacity = (v.min(usize::MAX as u64)) as usize;
    }

    // Cross-field validation (keep policy coherent).
    if next.channel_override_max_ttl_ms < next.channel_override_min_ttl_ms {
        return Err(NGError::from(
            "channel_override_max_ttl_ms must be >= channel_override_min_ttl_ms",
        ));
    }
    if next.channel_override_default_ttl_ms < next.channel_override_min_ttl_ms
        || next.channel_override_default_ttl_ms > next.channel_override_max_ttl_ms
    {
        return Err(NGError::from(
            "channel_override_default_ttl_ms must be within [min, max]",
        ));
    }

    // Apply to runtime.
    rt.apply_settings(next);

    // Track changed keys.
    let mut changed_keys: Vec<RuntimeSettingKey> = Vec::new();
    if cur.channel_override_default_ttl_ms != next.channel_override_default_ttl_ms {
        changed_keys.push(RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs);
    }
    if cur.channel_override_min_ttl_ms != next.channel_override_min_ttl_ms {
        changed_keys.push(RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs);
    }
    if cur.channel_override_max_ttl_ms != next.channel_override_max_ttl_ms {
        changed_keys.push(RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs);
    }
    if cur.override_cleanup_interval_ms != next.override_cleanup_interval_ms {
        changed_keys.push(RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs);
    }
    if cur.driver_ingest_queue_capacity != next.driver_ingest_queue_capacity {
        changed_keys.push(RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity);
    }

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;
    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::LoggingControlChannelOverrideDefaultTtlMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        next.channel_override_default_ttl_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingControlChannelOverrideMinTtlMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        next.channel_override_min_ttl_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingControlChannelOverrideMaxTtlMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        next.channel_override_max_ttl_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingControlOverrideCleanupIntervalMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        next.override_cleanup_interval_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingControlDriverIngestQueueCapacity => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (next.driver_ingest_queue_capacity as u64).min(i64::MAX as u64) as i64,
                    );
                }
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    Ok(ApplySystemSettingsResult {
        applied: true,
        persisted,
        domain: SystemSettingsDomain::LoggingControl,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact: SystemSettingsImpact::HotApply,
        restart_targets: Vec::new(),
    })
}

/// Apply + persist logging output settings.
pub fn apply_logging_output_settings(
    settings: &Settings,
    mut req: PatchLoggingOutputSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();
    if req.format.is_some() && env_overridden(RuntimeSettingKey::LoggingOutputFormat) {
        blocked_by_env.push(RuntimeSettingKey::LoggingOutputFormat);
        req.format = None;
    }
    if req.include_span_fields.is_some()
        && env_overridden(RuntimeSettingKey::LoggingOutputIncludeSpanFields)
    {
        blocked_by_env.push(RuntimeSettingKey::LoggingOutputIncludeSpanFields);
        req.include_span_fields = None;
    }
    if let Some(file) = req.file.as_mut() {
        if file.enabled.is_some() && env_overridden(RuntimeSettingKey::LoggingOutputFileEnabled) {
            blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileEnabled);
            file.enabled = None;
        }
        if file.dir.is_some() && env_overridden(RuntimeSettingKey::LoggingOutputFileDir) {
            blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileDir);
            file.dir = None;
        }
        if let Some(rotation) = file.rotation.as_mut() {
            if rotation.mode.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRotationMode)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRotationMode);
                rotation.mode = None;
            }
            if rotation.time.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRotationTime)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRotationTime);
                rotation.time = None;
            }
            if rotation.size_mb.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRotationSizeMb)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRotationSizeMb);
                rotation.size_mb = None;
            }
            if rotation.max_files.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRotationMaxFiles)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRotationMaxFiles);
                rotation.max_files = None;
            }
        }
        if let Some(retention) = file.retention.as_mut() {
            if retention.max_days.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRetentionMaxDays)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRetentionMaxDays);
                retention.max_days = None;
            }
            if retention.max_total_size_mb.is_some()
                && env_overridden(RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb)
            {
                blocked_by_env.push(RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb);
                retention.max_total_size_mb = None;
            }
        }
    }

    // Apply to runtime (hot swap).
    let changed_keys = settings.logging.apply_output_runtime_patch(&req);
    let applied = true;

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;
    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        let output = settings.logging.output.get();
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::LoggingOutputFormat => {
                    toml_set_string(&mut doc, key_path_segments(*key), output.format.as_str());
                }
                RuntimeSettingKey::LoggingOutputIncludeSpanFields => {
                    toml_set_bool(
                        &mut doc,
                        key_path_segments(*key),
                        output.include_span_fields,
                    );
                }
                RuntimeSettingKey::LoggingOutputFileEnabled => {
                    toml_set_bool(&mut doc, key_path_segments(*key), output.file.enabled);
                }
                RuntimeSettingKey::LoggingOutputFileDir => {
                    toml_set_string(&mut doc, key_path_segments(*key), &output.file.dir);
                }
                RuntimeSettingKey::LoggingOutputFileRotationMode => {
                    toml_set_string(
                        &mut doc,
                        key_path_segments(*key),
                        output.file.rotation.mode.as_str(),
                    );
                }
                RuntimeSettingKey::LoggingOutputFileRotationTime => {
                    toml_set_string(
                        &mut doc,
                        key_path_segments(*key),
                        output.file.rotation.time.as_str(),
                    );
                }
                RuntimeSettingKey::LoggingOutputFileRotationSizeMb => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (output.file.rotation.size_mb).min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingOutputFileRotationMaxFiles => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (output.file.rotation.max_files as u64).min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingOutputFileRetentionMaxDays => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (output.file.retention.max_days as u64).min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::LoggingOutputFileRetentionMaxTotalSizeMb => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (output.file.retention.max_total_size_mb).min(i64::MAX as u64) as i64,
                    );
                }
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    let mut impact = SystemSettingsImpact::HotApply;
    let mut restart_targets: Vec<String> = Vec::new();
    if !changed_keys.is_empty() {
        impact = SystemSettingsImpact::RestartComponent {
            components: vec!["logging_pipeline".to_string()],
        };
        restart_targets.push("logging_pipeline".to_string());
    }

    Ok(ApplySystemSettingsResult {
        applied,
        persisted,
        domain: SystemSettingsDomain::LoggingOutput,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact,
        restart_targets,
    })
}

/// Apply + persist logging cleanup policy.
pub fn apply_logging_cleanup_settings(
    settings: &Settings,
    mut req: PatchLoggingCleanupSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();
    if req.enabled.is_some() && env_overridden(RuntimeSettingKey::LoggingCleanupEnabled) {
        blocked_by_env.push(RuntimeSettingKey::LoggingCleanupEnabled);
        req.enabled = None;
    }
    if req.interval_ms.is_some() && env_overridden(RuntimeSettingKey::LoggingCleanupIntervalMs) {
        blocked_by_env.push(RuntimeSettingKey::LoggingCleanupIntervalMs);
        req.interval_ms = None;
    }

    // Apply to runtime (hot swap).
    let changed_keys = settings.logging.apply_cleanup_runtime_patch(&req);
    let applied = true;

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;
    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        let cleanup = settings.logging.cleanup.get();
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::LoggingCleanupEnabled => {
                    toml_set_bool(&mut doc, key_path_segments(*key), cleanup.enabled);
                }
                RuntimeSettingKey::LoggingCleanupIntervalMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (cleanup.interval_ms).min(i64::MAX as u64) as i64,
                    );
                }
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    Ok(ApplySystemSettingsResult {
        applied,
        persisted,
        domain: SystemSettingsDomain::LoggingCleanup,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact: SystemSettingsImpact::HotApply,
        restart_targets: Vec::new(),
    })
}

/// Apply + persist northward settings.
///
/// Phase 3 supports `general.northward.queue_capacity` with restart_component semantics.
pub fn apply_northward_settings(
    settings: &Settings,
    mut req: PatchNorthwardSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();
    if req.queue_capacity.is_some()
        && env_overridden(RuntimeSettingKey::GeneralNorthwardQueueCapacity)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralNorthwardQueueCapacity);
        req.queue_capacity = None;
    }
    if req.start_timeout_ms.is_some()
        && env_overridden(RuntimeSettingKey::GeneralNorthwardStartTimeoutMs)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralNorthwardStartTimeoutMs);
        req.start_timeout_ms = None;
    }

    // Apply to runtime (atomic).
    let changed_keys = settings.general.northward.apply_runtime_patch(&req);

    let applied = true;

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;

    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::GeneralNorthwardQueueCapacity => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.northward.queue_capacity() as u64).min(i64::MAX as u64)
                            as i64,
                    );
                }
                RuntimeSettingKey::GeneralNorthwardStartTimeoutMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.northward.start_timeout_ms()).min(i64::MAX as u64) as i64,
                    );
                }
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    let mut impact = SystemSettingsImpact::HotApply;
    let mut restart_targets: Vec<String> = Vec::new();
    if changed_keys.contains(&RuntimeSettingKey::GeneralNorthwardQueueCapacity) {
        impact = SystemSettingsImpact::RestartComponent {
            components: vec!["northward_events_pipeline".to_string()],
        };
        restart_targets.push("northward_events_pipeline".to_string());
    }

    Ok(ApplySystemSettingsResult {
        applied,
        persisted,
        domain: SystemSettingsDomain::Northward,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact,
        restart_targets,
    })
}

/// Apply + persist southward settings.
///
/// # Semantics
/// `start_timeout_ms` is consumed by control-plane start/restart operations. Hot applying it affects
/// subsequent operations immediately (no component restart required).
pub fn apply_southward_settings(
    settings: &Settings,
    mut req: PatchSouthwardSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    if let Some(v) = req.start_timeout_ms {
        if v == 0 {
            return Err(NGError::from("start_timeout_ms must be > 0"));
        }
        if v > 300_000 {
            return Err(NGError::from("start_timeout_ms must be <= 300000"));
        }
    }

    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();
    if req.start_timeout_ms.is_some()
        && env_overridden(RuntimeSettingKey::GeneralSouthwardStartTimeoutMs)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralSouthwardStartTimeoutMs);
        req.start_timeout_ms = None;
    }
    if req.device_change_cache_ttl_ms.is_some()
        && env_overridden(RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs);
        req.device_change_cache_ttl_ms = None;
    }
    if req.snapshot_gc_interval_ms.is_some()
        && env_overridden(RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs);
        req.snapshot_gc_interval_ms = None;
    }
    if req.snapshot_gc_workers.is_some()
        && env_overridden(RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers);
        req.snapshot_gc_workers = None;
    }
    if req.max_devices_per_snapshot_tick.is_some()
        && env_overridden(RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick);
        req.max_devices_per_snapshot_tick = None;
    }

    // Apply to runtime (atomic).
    let changed_keys = settings.general.southward.apply_runtime_patch(&req);
    let applied = true;

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;
    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::GeneralSouthwardStartTimeoutMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.southward.start_timeout_ms()).min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralSouthwardDeviceChangeCacheTtlMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.southward.device_change_cache_ttl_ms())
                            .min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralSouthwardSnapshotGcIntervalMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.southward.snapshot_gc_interval_ms()).min(i64::MAX as u64)
                            as i64,
                    );
                }
                RuntimeSettingKey::GeneralSouthwardSnapshotGcWorkers => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.southward.snapshot_gc_workers() as u64)
                            .min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralSouthwardMaxDevicesPerSnapshotTick => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        (settings.general.southward.max_devices_per_snapshot_tick() as u64)
                            .min(i64::MAX as u64) as i64,
                    );
                }
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    Ok(ApplySystemSettingsResult {
        applied,
        persisted,
        domain: SystemSettingsDomain::Southward,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact: SystemSettingsImpact::HotApply,
        restart_targets: Vec::new(),
    })
}

/// Apply + persist collector settings.
///
/// Phase 2 supports collector retry semantics, dynamic concurrency, and outbound queue rebuild.
pub fn apply_collector_settings(
    settings: &Settings,
    mut req: PatchCollectorSettingsRequest,
) -> NGResult<ApplySystemSettingsResult> {
    if let Some(rp) = &req.retry_policy {
        let cur = settings.general.collector.retry_policy();
        let next = apply_retry_policy_patch(cur, rp);
        validate_retry_policy_patch(cur, next)?;
    }

    // Filter out env-controlled fields.
    let mut blocked_by_env = Vec::new();

    if req.collection_timeout_ms.is_some()
        && env_overridden(RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs);
        req.collection_timeout_ms = None;
    }
    if let Some(rp) = req.retry_policy.as_mut() {
        if rp.max_attempts.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts);
            rp.max_attempts = None;
        }
        if rp.initial_interval_ms.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs);
            rp.initial_interval_ms = None;
        }
        if rp.max_interval_ms.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs);
            rp.max_interval_ms = None;
        }
        if rp.randomization_factor.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor);
            rp.randomization_factor = None;
        }
        if rp.multiplier.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier);
            rp.multiplier = None;
        }
        if rp.max_elapsed_time_ms.is_some()
            && env_overridden(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs)
        {
            blocked_by_env.push(RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs);
            rp.max_elapsed_time_ms = None;
        }
    }
    if req.max_concurrent_collections.is_some()
        && env_overridden(RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections);
        req.max_concurrent_collections = None;
    }
    if req.outbound_queue_capacity.is_some()
        && env_overridden(RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity)
    {
        blocked_by_env.push(RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity);
        req.outbound_queue_capacity = None;
    }

    // Apply to runtime (atomic).
    let changed_keys = settings.general.collector.apply_runtime_patch(&req);
    let applied = true;

    // Persist changed keys only.
    let mut persisted = true;
    let mut persistence_warning = None;

    if !changed_keys.is_empty() {
        let mut doc = read_gateway_toml_doc(settings.config_path())?;
        let retry_policy = settings.general.collector.retry_policy();
        for key in &changed_keys {
            match key {
                RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        settings
                            .general
                            .collector
                            .collection_timeout_ms()
                            .min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralCollectorMaxConcurrentCollections => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        settings
                            .general
                            .collector
                            .max_concurrent_collections()
                            .min(i64::MAX as usize) as i64,
                    );
                }
                RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        settings
                            .general
                            .collector
                            .outbound_queue_capacity()
                            .min(i64::MAX as usize) as i64,
                    );
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyInitialIntervalMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        retry_policy.initial_interval_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxIntervalMs => {
                    toml_set_int(
                        &mut doc,
                        key_path_segments(*key),
                        retry_policy.max_interval_ms.min(i64::MAX as u64) as i64,
                    );
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyRandomizationFactor => {
                    toml_set_float(
                        &mut doc,
                        key_path_segments(*key),
                        retry_policy.randomization_factor,
                    );
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyMultiplier => {
                    toml_set_float(&mut doc, key_path_segments(*key), retry_policy.multiplier);
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxAttempts => {
                    match retry_policy.max_attempts {
                        Some(v) => {
                            toml_set_int(
                                &mut doc,
                                key_path_segments(*key),
                                (v as u64).min(i64::MAX as u64) as i64,
                            );
                        }
                        None => toml_remove(&mut doc, key_path_segments(*key)),
                    }
                }
                RuntimeSettingKey::GeneralCollectorRetryPolicyMaxElapsedTimeMs => {
                    match retry_policy.max_elapsed_time_ms {
                        Some(v) => {
                            toml_set_int(
                                &mut doc,
                                key_path_segments(*key),
                                v.min(i64::MAX as u64) as i64,
                            );
                        }
                        None => toml_remove(&mut doc, key_path_segments(*key)),
                    }
                }
                // Non-collector keys should never appear in this apply path; ignore defensively.
                _ => {}
            }
        }

        let out = doc.to_string();
        if let Err(e) = atomic_rewrite(settings.config_path(), &out) {
            persisted = false;
            persistence_warning = Some(format!(
                "Runtime applied, but failed to persist to {} (restart will lose changes): {e}",
                settings.config_path().display()
            ));
        }
    }

    let mut impact = SystemSettingsImpact::HotApply;
    let mut restart_targets: Vec<String> = Vec::new();
    if changed_keys.contains(&RuntimeSettingKey::GeneralCollectorOutboundQueueCapacity) {
        impact = SystemSettingsImpact::RestartComponent {
            components: vec!["collector_outbound".to_string()],
        };
        restart_targets.push("collector_outbound".to_string());
    }

    Ok(ApplySystemSettingsResult {
        applied,
        persisted,
        domain: SystemSettingsDomain::Collector,
        changed_keys,
        blocked_by_env,
        persistence_warning,
        runtime_warning: None,
        impact,
        restart_targets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_derivation_is_stable() {
        assert_eq!(
            derive_env_key(RuntimeSettingKey::GeneralCollectorCollectionTimeoutMs),
            "NG__GENERAL__COLLECTOR__COLLECTION_TIMEOUT_MS"
        );
    }

    #[test]
    fn toml_set_int_creates_nested_tables() {
        let mut doc = DocumentMut::new();
        toml_set_int(
            &mut doc,
            &["general", "collector", "collection_timeout_ms"],
            123,
        );
        match doc["general"]["collector"]["collection_timeout_ms"].as_integer() {
            Some(v) => assert_eq!(v, 123),
            None => panic!("collection_timeout_ms is not an integer"),
        }
    }
}
