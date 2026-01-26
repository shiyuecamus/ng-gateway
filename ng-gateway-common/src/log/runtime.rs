//! Realtime logs runtime controller (enable/disable + hot settings apply).
//!
//! This module owns the process-wide realtime logs runtime state:
//! - Enabled flag
//! - Current settings snapshot
//! - Current LogHub instance (when enabled)
//! - Override manager (always present; leases are cleared when disabled)
//! - Shutdown broadcast for websocket endpoints (disable => close immediately)

use crate::log::realtime::{hub::LogHub, hub::LogHubConfig, lease::LogOverrideManager};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::settings::RealtimeLogs as RealtimeLogsSettings;
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shutdown notice for `/api/ws/logs`.
#[derive(Debug, Clone)]
pub struct RealtimeShutdownNotice {
    /// Monotonic generation, increments on each forced websocket close (disable/reconfigure).
    pub generation: u64,
    /// UTC millis when disabled.
    pub ts: i64,
    /// Human readable reason.
    pub reason: String,
}

struct Inner {
    settings: RealtimeLogsSettings,
    hub: Option<Arc<LogHub>>,
    overrides: Arc<LogOverrideManager>,
    generation: u64,
}

/// Global realtime logs runtime.
pub struct RealtimeRuntime {
    inner: RwLock<Inner>,
    shutdown_tx: broadcast::Sender<RealtimeShutdownNotice>,
}

static GLOBAL: OnceCell<Arc<RealtimeRuntime>> = OnceCell::new();

/// Initialize the runtime (idempotent).
///
/// # Behavior
/// - First call sets up the runtime.
/// - Subsequent calls apply new settings.
pub fn init(settings: RealtimeLogsSettings) -> NGResult<Arc<RealtimeRuntime>> {
    if let Some(rt) = GLOBAL.get() {
        rt.apply_settings(settings)?;
        return Ok(Arc::clone(rt));
    }

    let overrides = Arc::new(LogOverrideManager::new(&settings));
    overrides.start_cleanup_loop();

    let hub = if settings.enabled {
        Some(Arc::new(LogHub::new(LogHubConfig::from(&settings))?))
    } else {
        None
    };

    let (shutdown_tx, _rx) = broadcast::channel(64);

    let rt = Arc::new(RealtimeRuntime {
        inner: RwLock::new(Inner {
            settings,
            hub,
            overrides,
            generation: 0,
        }),
        shutdown_tx,
    });

    GLOBAL
        .set(Arc::clone(&rt))
        .map_err(|_| NGError::from("RealtimeRuntime already initialized"))?;
    Ok(rt)
}

/// Get global runtime.
#[inline]
pub fn global() -> Option<&'static Arc<RealtimeRuntime>> {
    GLOBAL.get()
}

impl RealtimeRuntime {
    /// Get current settings snapshot.
    #[inline]
    pub fn settings(&self) -> RealtimeLogsSettings {
        self.inner.read().settings
    }

    /// Get override manager.
    #[inline]
    pub fn overrides(&self) -> Arc<LogOverrideManager> {
        Arc::clone(&self.inner.read().overrides)
    }

    /// Get LogHub if enabled.
    #[inline]
    pub fn hub(&self) -> Option<Arc<LogHub>> {
        self.inner.read().hub.clone()
    }

    /// Subscribe to shutdown notices.
    #[inline]
    pub fn subscribe_shutdown(&self) -> broadcast::Receiver<RealtimeShutdownNotice> {
        self.shutdown_tx.subscribe()
    }

    /// Get current shutdown generation.
    ///
    /// # Notes
    /// This value increments whenever the server forces `/api/ws/logs` sessions to close
    /// (disable or reconfigure). It can be used by UI to detect re-subscribe events.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.inner.read().generation
    }

    /// Apply new settings (hot).
    ///
    /// If `enabled` changes from true->false, this will:
    /// - drop the hub
    /// - clear all leases
    /// - broadcast shutdown notice
    pub fn apply_settings(&self, new_settings: RealtimeLogsSettings) -> NGResult<()> {
        let old_settings = self.inner.read().settings;
        if old_settings == new_settings {
            return Ok(());
        }

        // Pre-build hub outside the lock so failures are atomic.
        let maybe_hub = if new_settings.enabled {
            Some(Arc::new(LogHub::new(LogHubConfig::from(&new_settings))?))
        } else {
            None
        };

        let was_enabled = old_settings.enabled;

        let mut guard = self.inner.write();
        let now_enabled = new_settings.enabled;

        guard.overrides.update_settings(&new_settings);

        guard.settings = new_settings;

        if now_enabled {
            guard.hub = maybe_hub;
        } else {
            guard.hub = None;
            guard.overrides.clear_all();
        }

        // Desired semantics:
        // - disable => close all sessions immediately
        // - enabled + settings changed (hub recreated) => close all sessions to force re-subscribe
        //   (prevents "connected but stuck on old hub receiver").
        if was_enabled && !now_enabled {
            guard.generation = guard.generation.wrapping_add(1);
            let notice = RealtimeShutdownNotice {
                generation: guard.generation,
                ts: chrono::Utc::now().timestamp_millis(),
                reason: "realtime logs disabled by settings".into(),
            };
            let _ = self.shutdown_tx.send(notice);
        } else if was_enabled && now_enabled {
            guard.generation = guard.generation.wrapping_add(1);
            let notice = RealtimeShutdownNotice {
                generation: guard.generation,
                ts: chrono::Utc::now().timestamp_millis(),
                reason: "realtime logs reconfigured by settings".into(),
            };
            let _ = self.shutdown_tx.send(notice);
        }
        Ok(())
    }

    /// Enable realtime logs.
    pub fn enable(&self) {
        let mut s = self.settings();
        if s.enabled {
            return;
        }
        s.enabled = true;
        let _ = self.apply_settings(s);
    }

    /// Disable realtime logs and notify websocket clients.
    pub fn disable(&self, reason: impl Into<String>) {
        let mut guard = self.inner.write();
        if !guard.settings.enabled {
            return;
        }
        guard.settings.enabled = false;
        guard.hub = None;
        guard.overrides.clear_all();
        guard.generation = guard.generation.wrapping_add(1);
        let notice = RealtimeShutdownNotice {
            generation: guard.generation,
            ts: chrono::Utc::now().timestamp_millis(),
            reason: reason.into(),
        };
        let _ = self.shutdown_tx.send(notice);
    }
}

/// Apply new settings to the global runtime.
pub fn apply_settings(new_settings: RealtimeLogsSettings) -> NGResult<()> {
    let rt = GLOBAL
        .get()
        .ok_or_else(|| NGError::from("RealtimeRuntime is not initialized"))?;
    rt.apply_settings(new_settings)
}

/// Enable realtime logs via global runtime.
pub fn enable() -> NGResult<()> {
    let rt = GLOBAL
        .get()
        .ok_or_else(|| NGError::from("RealtimeRuntime is not initialized"))?;
    rt.enable();
    Ok(())
}

/// Disable realtime logs via global runtime.
pub fn disable(reason: impl Into<String>) -> NGResult<()> {
    let rt = GLOBAL
        .get()
        .ok_or_else(|| NGError::from("RealtimeRuntime is not initialized"))?;
    rt.disable(reason);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_enable_disable_emits_shutdown() {
        let s = RealtimeLogsSettings {
            enabled: true,
            broadcast_capacity: 32,
            ..Default::default()
        };
        let rt = init(s).expect("init runtime");

        // Enabled => hub exists.
        assert!(rt.hub().is_some());

        let mut rx = rt.subscribe_shutdown();
        rt.disable("test-disable");

        // Disabled => hub dropped.
        assert!(rt.hub().is_none());

        let notice = rx.recv().await.expect("shutdown notice");
        assert_eq!(notice.reason, "test-disable");

        // Re-enable => hub recreated.
        rt.enable();
        assert!(rt.hub().is_some());
    }

    #[tokio::test]
    async fn runtime_apply_settings_reconfigured_closes_sessions() {
        let s = RealtimeLogsSettings {
            enabled: true,
            broadcast_capacity: 32,
            ..Default::default()
        };
        let rt = init(s).expect("init runtime");

        let mut rx = rt.subscribe_shutdown();
        let s2 = RealtimeLogsSettings {
            enabled: true,
            broadcast_capacity: 64,
            ..Default::default()
        };
        rt.apply_settings(s2).expect("apply settings");

        let notice = rx.recv().await.expect("shutdown notice");
        assert_eq!(notice.reason, "realtime logs reconfigured by settings");
    }
}
