//! Observability hooks for the supervision loop.
//!
//! # Design goals
//! - **Low overhead**: callbacks are control-plane only (low frequency).
//! - **Decoupled**: the core loop does not depend on tracing/metrics crates directly.
//! - **Safe**: implementations must never block the data-plane hot path.

use super::{ConnectionState, FailureReport, RetryBudgetSnapshot};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

/// Observer for supervision lifecycle events.
///
/// # Notes
/// - All methods must be fast and non-blocking.
/// - Implementations MUST NOT panic.
pub trait Observer: Send + Sync + 'static {
    /// Called whenever the connection state snapshot is updated.
    fn on_state(&self, state: &ConnectionState);

    /// Called when a failure is classified and published.
    fn on_failure(&self, report: &FailureReport);

    /// Called when the supervisor decides to back off before retrying.
    ///
    /// # Notes
    /// - This is control-plane only and should remain low frequency.
    /// - Implementations MUST NOT block.
    fn on_backoff(&self, delay: Duration, budget: &RetryBudgetSnapshot);
}

/// Stable, low-cardinality labels bound to a southward supervised instance.
#[derive(Clone, Debug)]
pub struct SouthwardObserverLabels {
    /// Channel identifier.
    pub channel_id: i32,
    /// Driver kind/type string (low-cardinality).
    pub driver_kind: Arc<str>,
}

/// Stable, low-cardinality labels bound to a northward supervised instance.
#[derive(Clone, Debug)]
pub struct NorthwardObserverLabels {
    /// App identifier.
    pub app_id: i32,
    /// Plugin kind/type string (low-cardinality).
    pub plugin_kind: Arc<str>,
}

/// Factory that creates per-instance observers with already-bound labels.
///
/// # Design notes
/// - The host (gateway core) should provide the implementation.
/// - This keeps metrics/logging wiring out of the SDK.
pub trait ObserverFactory: Send + Sync + 'static {
    /// Create a southward observer for a specific channel instance.
    #[inline]
    fn create_southward(&self, _labels: SouthwardObserverLabels) -> Arc<dyn Observer> {
        noop_observer_arc()
    }

    /// Create a northward observer for a specific app instance.
    #[inline]
    fn create_northward(&self, _labels: NorthwardObserverLabels) -> Arc<dyn Observer> {
        noop_observer_arc()
    }
}

/// A no-op observer for tests/offline tools.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserver;

impl Observer for NoopObserver {
    #[inline]
    fn on_state(&self, _state: &ConnectionState) {}

    #[inline]
    fn on_failure(&self, _report: &FailureReport) {}

    #[inline]
    fn on_backoff(&self, _delay: Duration, _budget: &RetryBudgetSnapshot) {}
}

/// A no-op observer factory for tests/offline tools.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopObserverFactory;

impl ObserverFactory for NoopObserverFactory {
    // Use default implementations.
}

#[inline]
fn noop_observer_arc() -> Arc<dyn Observer> {
    static OBS: OnceLock<Arc<dyn Observer>> = OnceLock::new();
    Arc::clone(OBS.get_or_init(|| Arc::new(NoopObserver)))
}
