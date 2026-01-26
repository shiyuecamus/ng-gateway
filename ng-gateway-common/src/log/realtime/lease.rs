//! TTL-based log level override manager (lease model).
//!
//! This module provides "temporary log level escalation" with automatic rollback:
//! - explicit release
//! - TTL expiry (cleanup loop owned by the manager, started once)
//!
//! # Important note
//! Tracing filtering is inherently global; perfect per-channel "zero-cost debug" is not possible.
//! The approach here is best-effort and intended for operational debugging.

use super::hub::LogLevel;
use dashmap::DashMap;
use ng_gateway_models::settings::RealtimeLogs as RealtimeLogsSettings;
use once_cell::sync::OnceCell;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;

/// A best-effort sink for observing effective level changes.
///
/// # Notes
/// This is the missing "semantic closure" for driver log control:
/// when leases expire (cleanup loop) or are created/released/renewed, the effective level can
/// change without a direct web request handler being involved. By notifying a host-installed
/// sink, the host can propagate the effective level to `cdylib` drivers via
/// `ng_driver_set_max_level`.
pub trait LogOverrideChangeSink: Send + Sync + 'static {
    /// Called after the effective log level for a scope has changed.
    fn on_effective_level_change(&self, scope: LogOverrideScope, level: LogLevel);
}

static CHANGE_SINK: OnceCell<Arc<dyn LogOverrideChangeSink>> = OnceCell::new();

/// Install a global change sink (best-effort, single assignment).
///
/// Returns `true` if installed successfully, `false` if a sink was already installed.
pub fn set_change_sink(sink: Arc<dyn LogOverrideChangeSink>) -> bool {
    CHANGE_SINK.set(sink).is_ok()
}

#[inline]
fn notify_change(scope: LogOverrideScope, level_u8: u8) {
    if let Some(sink) = CHANGE_SINK.get() {
        sink.on_effective_level_change(scope, u8_to_level(level_u8));
    }
}

/// Override scope for a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogOverrideScope {
    Global,
    Channel(i32),
}

/// A single log override lease.
#[derive(Debug, Clone)]
pub struct LogOverrideLease {
    pub id: Uuid,
    pub scope: LogOverrideScope,
    pub level: LogLevel,
    pub expires_at_ms: i64,
}

/// Runtime log override manager.
///
/// # Responsibilities
/// - Create / renew / release leases
/// - Compute effective max level for global and per-channel scopes
/// - Cleanup expired leases in a background loop (started once)
pub struct LogOverrideManager {
    base_level: AtomicU8,
    global_level: AtomicU8,
    channel_levels: DashMap<i32, u8>,
    leases: DashMap<Uuid, LogOverrideLease>,

    lease_default_ttl_ms: AtomicU64,
    lease_max_ttl_ms: AtomicU64,
    cleanup_interval_ms: AtomicU64,
    cleanup_started: AtomicBool,
}

impl LogOverrideManager {
    /// Create a new override manager from settings.
    pub fn new(cfg: &RealtimeLogsSettings) -> Self {
        let base = LogLevel::Info as u8;
        Self {
            base_level: AtomicU8::new(base),
            global_level: AtomicU8::new(base),
            channel_levels: DashMap::new(),
            leases: DashMap::new(),
            lease_default_ttl_ms: AtomicU64::new(cfg.lease_default_ttl_ms),
            lease_max_ttl_ms: AtomicU64::new(cfg.lease_max_ttl_ms),
            cleanup_interval_ms: AtomicU64::new(cfg.lease_cleanup_interval_ms),
            cleanup_started: AtomicBool::new(false),
        }
    }

    /// Update settings (hot).
    pub fn update_settings(&self, cfg: &RealtimeLogsSettings) {
        self.lease_default_ttl_ms
            .store(cfg.lease_default_ttl_ms, Ordering::Relaxed);
        self.lease_max_ttl_ms
            .store(cfg.lease_max_ttl_ms, Ordering::Relaxed);
        self.cleanup_interval_ms
            .store(cfg.lease_cleanup_interval_ms, Ordering::Relaxed);
    }

    /// Clear all active leases (best-effort).
    pub fn clear_all(&self) {
        self.leases.clear();
        self.channel_levels.clear();
        self.recompute_global();
    }

    /// Set the baseline level (non-lease).
    pub fn set_base_level(&self, level: LogLevel) {
        self.base_level.store(level as u8, Ordering::Relaxed);
        self.recompute_global();
        self.recompute_all_channels();
    }

    /// Compute the effective level for global scope.
    #[inline]
    pub fn effective_global_level(&self) -> LogLevel {
        u8_to_level(self.global_level.load(Ordering::Relaxed))
    }

    /// Get effective level for a channel scope (fallback to global).
    #[inline]
    pub fn effective_channel_level(&self, channel_id: i32) -> LogLevel {
        if let Some(v) = self.channel_levels.get(&channel_id) {
            return u8_to_level(*v);
        }
        self.effective_global_level()
    }

    /// Create a lease with bounded TTL.
    pub fn create_lease(&self, scope: LogOverrideScope, level: LogLevel, ttl_ms: u64) -> Uuid {
        let now = chrono::Utc::now().timestamp_millis();
        let max = self.lease_max_ttl_ms.load(Ordering::Relaxed).max(1);
        let ttl_ms = ttl_ms.clamp(1, max);
        let expires_at_ms = now + ttl_ms as i64;

        let id = Uuid::new_v4();
        self.leases.insert(
            id,
            LogOverrideLease {
                id,
                scope,
                level,
                expires_at_ms,
            },
        );
        self.recompute_scope(scope);
        id
    }

    /// Renew an existing lease (best-effort).
    pub fn renew_lease(&self, id: Uuid, ttl_ms: u64) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        let max = self.lease_max_ttl_ms.load(Ordering::Relaxed).max(1);
        let ttl_ms = ttl_ms.clamp(1, max);
        if let Some(mut lease) = self.leases.get_mut(&id) {
            lease.expires_at_ms = now + ttl_ms as i64;
            self.recompute_scope(lease.scope);
            return true;
        }
        false
    }

    /// Release a lease (best-effort).
    pub fn release_lease(&self, id: Uuid) -> bool {
        if let Some((_, lease)) = self.leases.remove(&id) {
            self.recompute_scope(lease.scope);
            return true;
        }
        false
    }

    /// Start the background cleanup loop (idempotent).
    pub fn start_cleanup_loop(self: &Arc<Self>) {
        if self.cleanup_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let ms = this.cleanup_interval_ms.load(Ordering::Relaxed).max(200);
                tokio::time::sleep(Duration::from_millis(ms)).await;
                this.cleanup_expired();
            }
        });
    }

    fn cleanup_expired(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let ids: Vec<Uuid> = self
            .leases
            .iter()
            .filter(|e| e.expires_at_ms <= now)
            .map(|e| *e.key())
            .collect();

        let mut changed_scopes: Vec<LogOverrideScope> = Vec::new();
        for id in ids {
            if let Some((_, lease)) = self.leases.remove(&id) {
                changed_scopes.push(lease.scope);
            }
        }
        for scope in changed_scopes {
            self.recompute_scope(scope);
        }
    }

    fn recompute_scope(&self, scope: LogOverrideScope) {
        match scope {
            LogOverrideScope::Global => self.recompute_global(),
            LogOverrideScope::Channel(id) => self.recompute_channel(id),
        }
    }

    fn recompute_global(&self) {
        let old = self.global_level.load(Ordering::Relaxed);
        let base = self.base_level.load(Ordering::Relaxed);
        let mut max = base;
        for e in self.leases.iter() {
            if matches!(e.scope, LogOverrideScope::Global) {
                max = max.max(e.level as u8);
            }
        }
        self.global_level.store(max, Ordering::Relaxed);
        if max != old {
            notify_change(LogOverrideScope::Global, max);
        }
    }

    fn recompute_channel(&self, channel_id: i32) {
        let old = self.effective_channel_level(channel_id) as u8;
        let base = self.base_level.load(Ordering::Relaxed);
        let mut max = self.global_level.load(Ordering::Relaxed).max(base);
        for e in self.leases.iter() {
            if matches!(e.scope, LogOverrideScope::Channel(id) if id == channel_id) {
                max = max.max(e.level as u8);
            }
        }
        if max == self.effective_global_level() as u8 {
            self.channel_levels.remove(&channel_id);
        } else {
            self.channel_levels.insert(channel_id, max);
        }
        if max != old {
            notify_change(LogOverrideScope::Channel(channel_id), max);
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

    /// Default TTL (ms) for new leases.
    #[inline]
    pub fn default_ttl_ms(&self) -> u64 {
        self.lease_default_ttl_ms.load(Ordering::Relaxed).max(1)
    }
}

#[inline]
fn u8_to_level(v: u8) -> LogLevel {
    match v {
        x if x == LogLevel::Error as u8 => LogLevel::Error,
        x if x == LogLevel::Warn as u8 => LogLevel::Warn,
        x if x == LogLevel::Info as u8 => LogLevel::Info,
        x if x == LogLevel::Debug as u8 => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}
