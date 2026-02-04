//! Runtime log level control (global + per-channel) with TTL-based overrides.
//!
//! ## Goals
//! - **Hot path safe**: lock-free fast reads for effective levels.
//! - **Operational safety**: TTL required/bounded; automatic rollback on expiry.
//! - **Driver propagation**: best-effort sink to push effective levels to `cdylib` drivers.

use dashmap::DashMap;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{domain::prelude::LogLevel, settings::LoggingControl};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use tracing::Instrument;
use uuid::Uuid;

/// Override scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOverrideScope {
    Global,
    Channel(i32),
    App(i32),
}

/// A single override lease.
#[derive(Debug, Clone)]
pub struct LogOverrideLease {
    pub id: Uuid,
    pub scope: LogOverrideScope,
    pub level: LogLevel,
    /// TTL in ms used when creating this lease. Exposed to API for countdown progress.
    pub ttl_ms: u64,
    pub expires_at_ms: i64,
}

/// Best-effort sink for observing effective level changes.
///
/// This lets the host propagate the effective level to `cdylib` drivers via
/// `ng_driver_set_max_level` (including when TTL expiry changes levels).
pub trait LogOverrideChangeSink: Send + Sync + 'static {
    fn on_effective_level_change(&self, scope: LogOverrideScope, level: LogLevel);
}

static CHANGE_SINK: OnceCell<Arc<dyn LogOverrideChangeSink>> = OnceCell::new();

/// Install a global change sink (best-effort, single assignment).
///
/// Returns `true` if installed successfully, `false` if already installed.
pub fn set_change_sink(sink: Arc<dyn LogOverrideChangeSink>) -> bool {
    CHANGE_SINK.set(sink).is_ok()
}

#[inline]
fn notify_change(scope: LogOverrideScope, level_u8: u8) {
    if let Some(sink) = CHANGE_SINK.get() {
        sink.on_effective_level_change(scope, LogLevel::from(level_u8));
    }
}

/// Log runtime settings for TTL control and driver ingest capacity.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LogControlSettings {
    /// Default TTL for all override scopes in milliseconds.
    pub override_default_ttl_ms: u64,
    /// Minimum allowed TTL (ms) to avoid abusive rapid toggling.
    pub override_min_ttl_ms: u64,
    /// Maximum allowed TTL (ms) to avoid "permanent debug".
    pub override_max_ttl_ms: u64,
    /// Cleanup tick interval (ms) for expiring leases.
    pub override_cleanup_interval_ms: u64,
    /// Unified `cdylib -> host` ingest queue capacity (driver + plugin).
    ///
    /// This is a bounded queue at the FFI boundary; when it fills up, the oldest items are dropped
    /// to ensure new logs can still enter.
    pub ingest_queue_capacity: usize,
}

impl Default for LogControlSettings {
    fn default() -> Self {
        Self {
            override_default_ttl_ms: 5 * 60 * 1000,
            override_min_ttl_ms: 10 * 1000,
            override_max_ttl_ms: 30 * 60 * 1000,
            override_cleanup_interval_ms: 5_000,
            ingest_queue_capacity: 10_000,
        }
    }
}

impl From<LoggingControl> for LogControlSettings {
    fn from(v: LoggingControl) -> Self {
        Self {
            override_default_ttl_ms: v.override_default_ttl_ms,
            override_min_ttl_ms: v.override_min_ttl_ms,
            override_max_ttl_ms: v.override_max_ttl_ms,
            override_cleanup_interval_ms: v.override_cleanup_interval_ms,
            ingest_queue_capacity: v.ingest_queue_capacity,
        }
    }
}

impl LogControlSettings {
    #[inline]
    pub fn clamp_override_ttl_ms(&self, ttl_ms: u64) -> u64 {
        let min = self.override_min_ttl_ms.max(1);
        let max = self.override_max_ttl_ms.max(min);
        ttl_ms.clamp(min, max)
    }

    #[inline]
    pub fn validate_override_ttl_ms(&self, ttl_ms: u64) -> NGResult<()> {
        let min = self.override_min_ttl_ms.max(1);
        let max = self.override_max_ttl_ms.max(min);
        if ttl_ms < min || ttl_ms > max {
            return Err(NGError::from(format!(
                "Invalid ttlMs: must be within [{min}, {max}] ms"
            )));
        }
        Ok(())
    }
}

/// Runtime log override manager.
///
/// - `base_level`: baseline max level (non-lease), typically controlled by host logger.
/// - `leases`: temporary overrides with TTL.
/// - `global_level` / `channel_levels`: cached effective levels for fast reads.
pub struct LogOverrideManager {
    base_level: AtomicU8,
    global_level: AtomicU8,
    channel_levels: DashMap<i32, u8>,
    app_levels: DashMap<i32, u8>,
    leases: DashMap<Uuid, LogOverrideLease>,

    cleanup_interval_ms: AtomicU64,
    cleanup_started: AtomicBool,
}

impl LogOverrideManager {
    pub fn new(settings: &LogControlSettings) -> Self {
        let base: u8 = LogLevel::Info.into();
        Self {
            base_level: AtomicU8::new(base),
            global_level: AtomicU8::new(base),
            channel_levels: DashMap::new(),
            app_levels: DashMap::new(),
            leases: DashMap::new(),
            cleanup_interval_ms: AtomicU64::new(settings.override_cleanup_interval_ms),
            cleanup_started: AtomicBool::new(false),
        }
    }

    pub fn update_settings(&self, settings: &LogControlSettings) {
        self.cleanup_interval_ms
            .store(settings.override_cleanup_interval_ms, Ordering::Relaxed);
    }

    /// Clear all active leases (best-effort).
    pub fn clear_all(&self) {
        self.leases.clear();
        self.channel_levels.clear();
        self.app_levels.clear();
        self.recompute_global();
    }

    /// Set baseline max level (non-lease).
    pub fn set_base_level(&self, level: LogLevel) {
        self.base_level.store(level.into(), Ordering::Relaxed);
        self.recompute_global();
        self.recompute_all_channels();
        self.recompute_all_apps();
    }

    /// Effective global max level.
    #[inline]
    pub fn effective_global_level(&self) -> LogLevel {
        LogLevel::from(self.global_level.load(Ordering::Relaxed))
    }

    /// Effective max level for a channel (fallback to global).
    #[inline]
    pub fn effective_channel_level(&self, channel_id: i32) -> LogLevel {
        if let Some(v) = self.channel_levels.get(&channel_id) {
            return LogLevel::from(*v);
        }
        self.effective_global_level()
    }

    /// Effective max level for an app (fallback to global).
    #[inline]
    pub fn effective_app_level(&self, app_id: i32) -> LogLevel {
        if let Some(v) = self.app_levels.get(&app_id) {
            return LogLevel::from(*v);
        }
        self.effective_global_level()
    }

    /// Set a temporary override for a scope.
    ///
    /// Semantics: **replace** all existing leases for the same scope, then create a new lease.
    pub fn set_temporary_override(
        &self,
        scope: LogOverrideScope,
        level: LogLevel,
        ttl_ms: u64,
    ) -> LogOverrideLease {
        // Remove existing leases for this scope to keep behavior deterministic for UI.
        let to_remove: Vec<Uuid> = self
            .leases
            .iter()
            .filter(|e| e.scope == scope)
            .map(|e| *e.key())
            .collect();
        for id in to_remove {
            let _ = self.leases.remove(&id);
        }

        let now = chrono::Utc::now().timestamp_millis();
        let expires_at_ms = now + ttl_ms as i64;
        let id = Uuid::new_v4();
        let lease = LogOverrideLease {
            id,
            scope,
            level,
            ttl_ms,
            expires_at_ms,
        };
        self.leases.insert(id, lease.clone());
        self.recompute_scope(scope);
        lease
    }

    /// Clear all overrides for a scope.
    pub fn clear_scope(&self, scope: LogOverrideScope) {
        let to_remove: Vec<Uuid> = self
            .leases
            .iter()
            .filter(|e| e.scope == scope)
            .map(|e| *e.key())
            .collect();
        for id in to_remove {
            let _ = self.leases.remove(&id);
        }
        self.recompute_scope(scope);
    }

    /// Get the active lease for a scope (if any). If multiple exist, returns the one with max
    /// level then max expiry.
    pub fn active_scope_lease(&self, scope: LogOverrideScope) -> Option<LogOverrideLease> {
        let mut best: Option<LogOverrideLease> = None;
        for e in self.leases.iter() {
            if e.scope != scope {
                continue;
            }
            let cand = e.value().clone();
            best = match best {
                None => Some(cand),
                Some(prev) => {
                    if cand.level > prev.level
                        || (cand.level == prev.level && cand.expires_at_ms > prev.expires_at_ms)
                    {
                        Some(cand)
                    } else {
                        Some(prev)
                    }
                }
            };
        }
        best
    }

    /// Start background cleanup loop (idempotent).
    ///
    /// Must be called from within a Tokio runtime context.
    pub fn start_cleanup_loop(self: &Arc<Self>) {
        if self.cleanup_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(
            async move {
                loop {
                    let ms = this.cleanup_interval_ms.load(Ordering::Relaxed).max(200);
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    this.cleanup_expired();
                }
            }
            .in_current_span(),
        );
    }

    fn cleanup_expired(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let ids: Vec<Uuid> = self
            .leases
            .iter()
            .filter(|e| e.expires_at_ms <= now)
            .map(|e| *e.key())
            .collect();

        let mut changed: Vec<LogOverrideScope> = Vec::new();
        for id in ids {
            if let Some((_, lease)) = self.leases.remove(&id) {
                changed.push(lease.scope);
            }
        }
        for scope in changed {
            self.recompute_scope(scope);
        }
    }

    fn recompute_scope(&self, scope: LogOverrideScope) {
        match scope {
            LogOverrideScope::Global => self.recompute_global(),
            LogOverrideScope::Channel(id) => self.recompute_channel(id),
            LogOverrideScope::App(id) => self.recompute_app(id),
        }
    }

    fn recompute_global(&self) {
        let old = self.global_level.load(Ordering::Relaxed);
        let base = self.base_level.load(Ordering::Relaxed);
        let mut max = base;
        for e in self.leases.iter() {
            if matches!(e.scope, LogOverrideScope::Global) {
                max = max.max(u8::from(e.level));
            }
        }
        self.global_level.store(max, Ordering::Relaxed);
        if max != old {
            notify_change(LogOverrideScope::Global, max);
        }
    }

    fn recompute_channel(&self, channel_id: i32) {
        let old: u8 = self.effective_channel_level(channel_id).into();

        // Semantics: channel override **overrides** global level for this channel
        // (it can both raise and lower verbosity). If no lease exists, fall back to global.
        let global: u8 = self.global_level.load(Ordering::Relaxed);
        let mut chosen: Option<u8> = None;
        for e in self.leases.iter() {
            if matches!(e.scope, LogOverrideScope::Channel(id) if id == channel_id) {
                let v = u8::from(e.level);
                chosen = Some(chosen.map_or(v, |prev| prev.max(v)));
            }
        }

        let new = chosen.unwrap_or(global);
        if new == global {
            self.channel_levels.remove(&channel_id);
        } else {
            self.channel_levels.insert(channel_id, new);
        }

        if new != old {
            notify_change(LogOverrideScope::Channel(channel_id), new);
        }
    }

    fn recompute_app(&self, app_id: i32) {
        let old: u8 = self.effective_app_level(app_id).into();

        // Semantics: app override **overrides** global level for this app
        // (it can both raise and lower verbosity). If no lease exists, fall back to global.
        let global: u8 = self.global_level.load(Ordering::Relaxed);
        let mut chosen: Option<u8> = None;
        for e in self.leases.iter() {
            if matches!(e.scope, LogOverrideScope::App(id) if id == app_id) {
                let v = u8::from(e.level);
                chosen = Some(chosen.map_or(v, |prev| prev.max(v)));
            }
        }

        let new = chosen.unwrap_or(global);
        if new == global {
            self.app_levels.remove(&app_id);
        } else {
            self.app_levels.insert(app_id, new);
        }

        if new != old {
            notify_change(LogOverrideScope::App(app_id), new);
        }
    }

    fn recompute_all_channels(&self) {
        let channel_ids: Vec<i32> = self
            .leases
            .iter()
            .filter_map(|e| match e.scope {
                LogOverrideScope::Channel(id) => Some(id),
                _ => None,
            })
            .collect();
        for id in channel_ids {
            self.recompute_channel(id);
        }
    }

    fn recompute_all_apps(&self) {
        let app_ids: Vec<i32> = self
            .leases
            .iter()
            .filter_map(|e| match e.scope {
                LogOverrideScope::App(id) => Some(id),
                _ => None,
            })
            .collect();
        for id in app_ids {
            self.recompute_app(id);
        }
    }
}

/// Global log control runtime (singleton).
pub struct LogControlRuntime {
    settings: parking_lot::RwLock<LogControlSettings>,
    overrides: Arc<LogOverrideManager>,
}

static GLOBAL: OnceCell<Arc<LogControlRuntime>> = OnceCell::new();

/// Initialize the runtime (idempotent).
///
/// First call sets up the runtime; subsequent calls hot-apply settings.
pub fn init(settings: LogControlSettings) -> NGResult<Arc<LogControlRuntime>> {
    if let Some(rt) = GLOBAL.get() {
        rt.apply_settings(settings);
        return Ok(Arc::clone(rt));
    }

    let overrides = Arc::new(LogOverrideManager::new(&settings));
    overrides.start_cleanup_loop();

    let rt = Arc::new(LogControlRuntime {
        settings: parking_lot::RwLock::new(settings),
        overrides,
    });

    GLOBAL
        .set(Arc::clone(&rt))
        .map_err(|_| NGError::from("LogControlRuntime already initialized"))?;
    Ok(rt)
}

/// Get global runtime.
#[inline]
pub fn global() -> Option<&'static Arc<LogControlRuntime>> {
    GLOBAL.get()
}

impl LogControlRuntime {
    #[inline]
    pub fn settings(&self) -> LogControlSettings {
        *self.settings.read()
    }

    #[inline]
    pub fn overrides(&self) -> Arc<LogOverrideManager> {
        Arc::clone(&self.overrides)
    }

    pub fn apply_settings(&self, new_settings: LogControlSettings) {
        *self.settings.write() = new_settings;
        self.overrides.update_settings(&new_settings);
    }
}
