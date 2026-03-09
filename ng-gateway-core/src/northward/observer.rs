//! Northward supervision observers (core-owned).
//!
//! This mirrors `southward/observer.rs`: the SDK supervision loop owns lifecycle semantics,
//! while core attaches side effects (metrics updates, buffer flush, and a host-owned
//! connection-state snapshot) through `Observer`.
//!
//! # Performance & safety
//! - Called on **control-plane** transitions only (low frequency).
//! - MUST be non-blocking: uses `try_send` when flushing buffer.
//! - State snapshot updates are lock-free via `ArcSwap`.

use super::{
    super::observer::{CompositeObserver, CoreObserverFactory},
    actor::{AppActor, AppIo, BufferQueue},
};
use arc_swap::ArcSwap;
use ng_gateway_common::metrics::{
    channel::{InstrumentedSender, QueueObserver},
    northward::NorthwardAppMetricHandles,
};
use ng_gateway_sdk::{
    supervision::{NorthwardObserverLabels, Observer, ObserverFactory, RetryBudgetSnapshot},
    ConnectionState, FailureReport, NorthwardData, Phase, QueuePolicy,
};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use tracing::warn;

/// Lock-free connection state cell shared between AppActor and its supervision observer.
pub(crate) type ConnectionStateCell = Arc<ArcSwap<ConnectionState>>;

/// Per-app observer factory for northward plugins.
#[derive(Clone)]
pub(crate) struct NorthwardAppObserverFactory {
    app_id: i32,
    prom: Arc<NorthwardAppMetricHandles>,
    queue_policy: QueuePolicy,
    buffer_queue: BufferQueue,
    buffer_observer: QueueObserver,
    data_tx: InstrumentedSender<Arc<NorthwardData>>,
    conn_state: ConnectionStateCell,
}

impl NorthwardAppObserverFactory {
    #[inline]
    pub(crate) fn new(app_id: i32, io: &AppIo, queue_policy: QueuePolicy) -> Self {
        Self {
            app_id,
            prom: Arc::clone(&io.prom),
            queue_policy,
            buffer_queue: Arc::clone(&io.buffer_queue),
            buffer_observer: io.buffer_observer.clone(),
            data_tx: io.data_tx.clone(),
            conn_state: Arc::clone(&io.conn_state),
        }
    }
}

impl ObserverFactory for NorthwardAppObserverFactory {
    fn create_northward(&self, labels: NorthwardObserverLabels) -> Arc<dyn Observer> {
        // Best-effort sanity check without panicking.
        if labels.app_id != self.app_id {
            warn!(
                expected_app_id = self.app_id,
                got_app_id = labels.app_id,
                "northward observer labels mismatch"
            );
        }

        let side_effects: Arc<dyn Observer> = Arc::new(NorthwardAppObserver {
            app_id: self.app_id,
            prom: Arc::clone(&self.prom),
            queue_policy: self.queue_policy,
            buffer_queue: Arc::clone(&self.buffer_queue),
            buffer_observer: self.buffer_observer.clone(),
            data_tx: self.data_tx.clone(),
            conn_state: Arc::clone(&self.conn_state),
            last_phase: AtomicU8::new(u8::from(Phase::Disconnected)),
        });

        let logging: Arc<dyn Observer> = CoreObserverFactory::new().create_northward(labels);
        Arc::new(CompositeObserver::new(vec![logging, side_effects]))
    }
}

struct NorthwardAppObserver {
    app_id: i32,
    prom: Arc<NorthwardAppMetricHandles>,
    queue_policy: QueuePolicy,
    buffer_queue: BufferQueue,
    buffer_observer: QueueObserver,
    data_tx: InstrumentedSender<Arc<NorthwardData>>,
    conn_state: ConnectionStateCell,
    last_phase: AtomicU8,
}

impl Observer for NorthwardAppObserver {
    fn on_state(&self, state: &ConnectionState) {
        // Update cached state snapshot for REST/UI (always).
        self.conn_state.store(Arc::new(state.clone()));

        let now = u8::from(state.phase);
        let prev = self.last_phase.swap(now, Ordering::AcqRel);
        if prev == now {
            return;
        }

        // Update connected gauge.
        self.prom.set_connected(state.is_connected());

        // Count reconnect transitions.
        if state.is_reconnecting() && prev != u8::from(Phase::Reconnecting) {
            self.prom.inc_reconnect();
        }

        // Record failures (non-message).
        if state.is_failed() && prev != u8::from(Phase::Failed) {
            self.prom.record_error_event();
        }

        // Flush buffer on transition to Connected.
        if state.phase == Phase::Connected && self.queue_policy.buffer_enabled {
            if let Err(e) = AppActor::flush_buffer(
                &self.buffer_queue,
                &self.data_tx,
                self.queue_policy,
                self.app_id,
                &self.prom,
                &self.buffer_observer,
            ) {
                warn!(app_id = self.app_id, error = %e, "Failed to flush buffer");
            }
        }
    }

    #[inline]
    fn on_failure(&self, _report: &FailureReport) {
        self.prom.record_error_event();
    }

    #[inline]
    fn on_backoff(&self, _delay: Duration, _budget: &RetryBudgetSnapshot) {}
}
