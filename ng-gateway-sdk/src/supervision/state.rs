//! Connection lifecycle state model used by supervised components.
//!
//! # Design goals
//! - **Structured**: distinguish connect/init/run failures for diagnostics.
//! - **Cheap clone**: expose state as `Arc<ConnectionState>` for O(1) clones.
//! - **UI/REST friendly**: timestamps use Unix milliseconds for stable display.
//! - **Low overhead**: avoid `String` duplication on fast control-plane paths.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{sync::Arc, time::Duration};

/// Connection phase for a supervised component.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Not connected and not attempting to connect.
    Disconnected,
    /// Establishing transport/protocol connection.
    Connecting,
    /// Post-connect initialization that defines "Ready" (subscribe/GI/handshake-after-connect).
    Initializing,
    /// Ready for data-plane operations; handle is published.
    Connected,
    /// Waiting for next reconnect attempt (backoff window).
    Reconnecting,
    /// Budget exhausted or fatal error; human intervention required.
    Failed,
}

impl From<Phase> for u8 {
    #[inline]
    fn from(value: Phase) -> Self {
        match value {
            Phase::Disconnected => 0,
            Phase::Connecting => 1,
            Phase::Initializing => 2,
            Phase::Connected => 3,
            Phase::Reconnecting => 4,
            Phase::Failed => 5,
        }
    }
}

impl From<Phase> for i64 {
    #[inline]
    fn from(value: Phase) -> Self {
        i64::from(u8::from(value))
    }
}

impl Phase {
    /// Returns true if the component is considered connected/ready.
    #[inline]
    pub fn is_connected(self) -> bool {
        matches!(self, Phase::Connected)
    }

    /// Returns true if the component is actively attempting to connect/initialize.
    #[inline]
    pub fn is_connecting(self) -> bool {
        matches!(self, Phase::Connecting | Phase::Initializing)
    }

    /// Returns true if the component is in a backoff window.
    #[inline]
    pub fn is_reconnecting(self) -> bool {
        matches!(self, Phase::Reconnecting)
    }

    /// Returns true if the component is failed.
    #[inline]
    pub fn is_failed(self) -> bool {
        matches!(self, Phase::Failed)
    }
}

/// Failure classification that drives retry/budget decisions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    /// The operation can be retried under the current retry policy.
    Retryable,
    /// A non-recoverable error (e.g., unsupported config, protocol incompatibility).
    Fatal,
    /// A graceful stop requested by system/operator; no retries should occur.
    Stop,
}

/// Failure phase for precise diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailurePhase {
    Connect,
    Init,
    Run,
}

/// Structured failure report for UI/alerts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailureReport {
    /// Where the failure occurred.
    pub phase: FailurePhase,
    /// How the failure should be handled.
    pub kind: FailureKind,
    /// UI-friendly summary (cheap clone via `Arc`).
    pub summary: Arc<str>,
    /// Optional stable error code for aggregation/alerting.
    pub code: Option<Arc<str>>,
}

/// Snapshot of retry budget for observability.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryBudgetSnapshot {
    pub exhausted: bool,
    pub remaining_hint: Option<u32>,
}

/// Connection state snapshot for a supervised component.
///
/// This is the single source of truth for monitor/UI/metrics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConnectionState {
    pub phase: Phase,
    /// Attempt counter; monotonically increases per connect loop.
    pub attempt: u64,
    /// Unix timestamp in milliseconds when this snapshot was emitted.
    pub emitted_at_unix_ms: u64,
    /// Unix timestamp in milliseconds when the current phase was entered.
    pub phase_entered_at_unix_ms: u64,
    /// Backoff duration for `Reconnecting` (or next retry hint).
    pub backoff: Option<Duration>,
    /// Latest structured failure if any.
    pub last_failure: Option<Arc<FailureReport>>,
    /// Retry budget snapshot for UI diagnostics.
    pub budget: RetryBudgetSnapshot,
}

impl ConnectionState {
    /// Create a minimal state snapshot.
    ///
    /// # Notes
    /// Timestamps must be supplied by the caller to avoid hidden time sources and
    /// to keep tests deterministic.
    #[inline]
    pub fn new(phase: Phase, attempt: u64, now_unix_ms: u64, phase_entered_unix_ms: u64) -> Self {
        Self {
            phase,
            attempt,
            emitted_at_unix_ms: now_unix_ms,
            phase_entered_at_unix_ms: phase_entered_unix_ms,
            backoff: None,
            last_failure: None,
            budget: RetryBudgetSnapshot {
                exhausted: false,
                remaining_hint: None,
            },
        }
    }

    /// Create a state snapshot using the current system time.
    ///
    /// This is intended for production code paths. For deterministic tests, prefer `new(...)`.
    #[inline]
    pub fn now(phase: Phase, attempt: u64) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        Self::new(phase, attempt, now_ms, now_ms)
    }

    /// Create an `Arc`-wrapped snapshot using the current system time.
    ///
    /// # Rationale
    /// Most consumers (`watch<Arc<ConnectionState>>`) expect an `Arc` snapshot. Providing this
    /// helper avoids duplicated boilerplate and keeps timestamp semantics consistent.
    #[inline]
    pub fn arc_now(phase: Phase, attempt: u64) -> Arc<Self> {
        Arc::new(Self::now(phase, attempt))
    }

    /// Create an `Arc`-wrapped snapshot with a provided failure report.
    #[inline]
    pub fn arc_now_with_failure(
        phase: Phase,
        attempt: u64,
        last_failure: Option<Arc<FailureReport>>,
    ) -> Arc<Self> {
        let mut st = Self::now(phase, attempt);
        st.last_failure = last_failure;
        Arc::new(st)
    }

    /// Returns true if this state is connected/ready.
    #[inline]
    pub fn is_connected(&self) -> bool {
        self.phase.is_connected()
    }

    /// Returns true if this state is reconnecting (backoff window).
    #[inline]
    pub fn is_reconnecting(&self) -> bool {
        self.phase.is_reconnecting()
    }

    /// Returns true if this state is failed.
    #[inline]
    pub fn is_failed(&self) -> bool {
        self.phase.is_failed()
    }

    /// Returns true if this state is disconnected.
    #[inline]
    pub fn is_disconnected(&self) -> bool {
        matches!(self.phase, Phase::Disconnected)
    }

    /// Returns a numeric value for gauges (stable ordering for dashboards).
    #[inline]
    pub fn as_value(&self) -> i64 {
        i64::from(self.phase)
    }
}
