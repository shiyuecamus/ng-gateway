//! Core-provided supervision observers.
//!
//! This module provides a host-owned `ObserverFactory` implementation so the SDK supervision loop
//! can emit lifecycle events without depending on any metrics/logging crates directly.
//!
//! # Design goals
//! - **Low overhead**: only called on control-plane state transitions and failures.
//! - **No blocking**: never await; never perform network or blocking I/O.
//! - **Bounded labels**: strictly low-cardinality (`channel_id/app_id` + `kind`).

use ng_gateway_sdk::supervision::{
    ConnectionState, FailureKind, FailureReport, NoopObserver, NorthwardObserverLabels, Observer,
    ObserverFactory, Phase, RetryBudgetSnapshot, SouthwardObserverLabels,
};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

/// A tiny fan-out observer that calls multiple observers sequentially.
///
/// # Notes
/// - This stays on the control plane (state transitions/backoff/failures).
/// - Individual observers MUST be non-blocking; this wrapper does not isolate slowness.
pub(crate) struct CompositeObserver {
    inner: Vec<Arc<dyn Observer>>,
}

impl CompositeObserver {
    #[inline]
    pub(crate) fn new(inner: Vec<Arc<dyn Observer>>) -> Self {
        Self { inner }
    }
}

impl Observer for CompositeObserver {
    #[inline]
    fn on_state(&self, state: &ConnectionState) {
        for o in self.inner.iter() {
            o.on_state(state);
        }
    }

    #[inline]
    fn on_failure(&self, report: &FailureReport) {
        for o in self.inner.iter() {
            o.on_failure(report);
        }
    }

    #[inline]
    fn on_backoff(&self, delay: Duration, budget: &RetryBudgetSnapshot) {
        for o in self.inner.iter() {
            o.on_backoff(delay, budget);
        }
    }
}

/// Core-owned observer factory.
///
/// Currently, this only enables structured logging. Metrics can be added later without changing
/// the supervision ABI.
#[derive(Debug, Default, Clone)]
pub struct CoreObserverFactory;

impl CoreObserverFactory {
    /// Create a new factory.
    #[inline]
    pub fn new() -> Self {
        Self
    }
}

impl ObserverFactory for CoreObserverFactory {
    fn create_southward(&self, labels: SouthwardObserverLabels) -> Arc<dyn Observer> {
        Arc::new(LoggingObserver::new(LoggingTarget::Southward {
            channel_id: labels.channel_id,
            kind: labels.driver_kind,
        }))
    }

    fn create_northward(&self, labels: NorthwardObserverLabels) -> Arc<dyn Observer> {
        Arc::new(LoggingObserver::new(LoggingTarget::Northward {
            app_id: labels.app_id,
            kind: labels.plugin_kind,
        }))
    }
}

/// Logging target identity.
#[derive(Debug, Clone)]
enum LoggingTarget {
    Southward { channel_id: i32, kind: Arc<str> },
    Northward { app_id: i32, kind: Arc<str> },
}

/// A low-overhead observer that logs phase transitions and failures.
///
/// # Notes
/// - This is intentionally conservative: it avoids allocations and avoids per-state spam.
/// - It does NOT emit business events or touch data-plane queues.
#[derive(Debug)]
struct LoggingObserver {
    target: LoggingTarget,
    last_phase: AtomicU8,
}

impl LoggingObserver {
    #[inline]
    fn new(target: LoggingTarget) -> Self {
        Self {
            target,
            last_phase: AtomicU8::new(u8::from(Phase::Disconnected)),
        }
    }
}

impl Observer for LoggingObserver {
    fn on_state(&self, state: &ConnectionState) {
        let now = u8::from(state.phase);
        let prev = self.last_phase.swap(now, Ordering::AcqRel);
        if prev == now {
            return;
        }

        match &self.target {
            LoggingTarget::Southward { channel_id, kind } => {
                tracing::info!(
                    channel_id = *channel_id,
                    driver_kind = kind.as_ref(),
                    phase = ?state.phase,
                    attempt = state.attempt,
                    "supervisor state changed"
                );
            }
            LoggingTarget::Northward { app_id, kind } => {
                tracing::info!(
                    app_id = *app_id,
                    plugin_kind = kind.as_ref(),
                    phase = ?state.phase,
                    attempt = state.attempt,
                    "supervisor state changed"
                );
            }
        }
    }

    fn on_failure(&self, report: &FailureReport) {
        let level = match report.kind {
            FailureKind::Retryable => "retryable",
            FailureKind::Fatal => "fatal",
            FailureKind::Stop => "stop",
        };

        match &self.target {
            LoggingTarget::Southward { channel_id, kind } => {
                tracing::warn!(
                    channel_id = *channel_id,
                    driver_kind = kind.as_ref(),
                    failure_phase = ?report.phase,
                    failure_kind = level,
                    summary = report.summary.as_ref(),
                    code = report.code.as_deref(),
                    "supervisor failure"
                );
            }
            LoggingTarget::Northward { app_id, kind } => {
                tracing::warn!(
                    app_id = *app_id,
                    plugin_kind = kind.as_ref(),
                    failure_phase = ?report.phase,
                    failure_kind = level,
                    summary = report.summary.as_ref(),
                    code = report.code.as_deref(),
                    "supervisor failure"
                );
            }
        }
    }

    fn on_backoff(&self, delay: Duration, budget: &RetryBudgetSnapshot) {
        match &self.target {
            LoggingTarget::Southward { channel_id, kind } => {
                tracing::info!(
                    channel_id = *channel_id,
                    driver_kind = kind.as_ref(),
                    backoff_ms = delay.as_millis() as u64,
                    exhausted = budget.exhausted,
                    remaining_hint = budget.remaining_hint,
                    "supervisor backoff"
                );
            }
            LoggingTarget::Northward { app_id, kind } => {
                tracing::info!(
                    app_id = *app_id,
                    plugin_kind = kind.as_ref(),
                    backoff_ms = delay.as_millis() as u64,
                    exhausted = budget.exhausted,
                    remaining_hint = budget.remaining_hint,
                    "supervisor backoff"
                );
            }
        }
    }
}

/// Helper to provide a noop factory when the host chooses to disable supervision observers.
#[inline]
pub fn noop_factory() -> Arc<dyn ObserverFactory> {
    Arc::new(NoopFactory)
}

#[derive(Debug, Default, Clone, Copy)]
struct NoopFactory;

impl ObserverFactory for NoopFactory {
    #[inline]
    fn create_southward(&self, _labels: SouthwardObserverLabels) -> Arc<dyn Observer> {
        Arc::new(NoopObserver)
    }

    #[inline]
    fn create_northward(&self, _labels: NorthwardObserverLabels) -> Arc<dyn Observer> {
        Arc::new(NoopObserver)
    }
}
