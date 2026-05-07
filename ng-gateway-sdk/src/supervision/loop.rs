//! Unified supervision loop implementation.
//!
//! This module provides a reusable lifecycle controller that:
//! - publishes `watch<Arc<ConnectionState>>` snapshots
//! - publishes/clears the data-plane handle via `HandleCell`
//! - enforces retry budget/backoff via `RetryController`
//! - supports best-effort reconnect requests (control plane)

use super::{
    protocol::{Connector, ReconnectHandle, RunOutcome, Session, SessionContext},
    ConnectionState, FailureKind, FailurePhase, FailureReport, HandleCell, NoopObserver, Observer,
    Phase, RetryBudgetSnapshot,
};
use crate::{NorthwardResult, RetryController, RetryDecision, RetryPolicy};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info_span, Instrument, Span};

/// Configuration for a supervised component loop.
#[derive(Clone)]
pub struct SupervisorParams {
    /// Retry/backoff policy.
    pub retry_policy: RetryPolicy,
    /// Max queued reconnect requests (best-effort control plane).
    pub reconnect_queue: usize,
}

impl Default for SupervisorParams {
    fn default() -> Self {
        Self {
            retry_policy: RetryPolicy::default(),
            reconnect_queue: 8,
        }
    }
}

/// Handle to interact with a running supervisor loop.
#[derive(Clone)]
pub struct SupervisorHandle {
    cancel: CancellationToken,
}

impl SupervisorHandle {
    /// Request a graceful stop (no more reconnect attempts).
    #[inline]
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

/// The supervision loop controller.
///
/// This struct is intended to be embedded in `SupervisedDriver`/`SupervisedPlugin`.
pub struct SupervisorLoop<C>
where
    C: Connector,
{
    connector: C,
    params: SupervisorParams,
    observer: Arc<dyn Observer>,
    span: Span,

    state_tx: watch::Sender<Arc<ConnectionState>>,
    handle_cell: HandleCell<C::Handle>,

    cancel: CancellationToken,
}

impl<C> SupervisorLoop<C>
where
    C: Connector,
{
    /// Create a new supervisor loop with a custom observer.
    pub fn new(
        connector: C,
        params: SupervisorParams,
        observer: Arc<dyn Observer>,
    ) -> (Self, watch::Receiver<Arc<ConnectionState>>) {
        Self::new_with_span(connector, params, observer, Span::none())
    }

    /// Create a new supervisor loop with a custom observer and a tracing span.
    pub fn new_with_span(
        connector: C,
        params: SupervisorParams,
        observer: Arc<dyn Observer>,
        span: Span,
    ) -> (Self, watch::Receiver<Arc<ConnectionState>>) {
        let init = ConnectionState::arc_now(Phase::Disconnected, 0);
        let (state_tx, state_rx) = watch::channel(init);
        let cancel = CancellationToken::new();
        (
            Self {
                connector,
                params,
                observer,
                span,
                state_tx,
                handle_cell: HandleCell::new(),
                cancel,
            },
            state_rx,
        )
    }

    /// Create a new supervisor loop with a no-op observer.
    #[inline]
    pub fn new_noop(
        connector: C,
        params: SupervisorParams,
    ) -> (Self, watch::Receiver<Arc<ConnectionState>>) {
        Self::new(connector, params, Arc::new(NoopObserver))
    }

    /// Create a new supervisor loop with a no-op observer and a tracing span.
    #[inline]
    pub fn new_noop_with_span(
        connector: C,
        params: SupervisorParams,
        span: Span,
    ) -> (Self, watch::Receiver<Arc<ConnectionState>>) {
        Self::new_with_span(connector, params, Arc::new(NoopObserver), span)
    }

    /// Subscribe to state snapshots.
    #[inline]
    pub fn subscribe_state(&self) -> watch::Receiver<Arc<ConnectionState>> {
        self.state_tx.subscribe()
    }

    /// Hot-load the current data-plane handle.
    #[inline]
    pub fn load_handle(&self) -> Option<Arc<C::Handle>> {
        self.handle_cell.load()
    }

    /// Invoke a connector-defined low-frequency capability.
    ///
    /// This intentionally does not require a published data-plane handle so
    /// callers can inspect connector-owned state even before the session reaches
    /// the Ready phase.
    #[inline]
    pub async fn invoke_capability(
        &self,
        capability_id: &str,
        request: serde_json::Value,
    ) -> NorthwardResult<serde_json::Value> {
        self.connector
            .invoke_capability(capability_id, request)
            .await
    }

    /// Start the background supervision task.
    pub fn start(self: Arc<Self>) -> SupervisorHandle {
        let cancel = self.cancel.clone();
        let span = self.span.clone();
        tokio::spawn(async move { self.run_loop().await }.instrument(span));
        SupervisorHandle { cancel }
    }

    async fn run_loop(self: Arc<Self>) {
        let (reconnect_tx, mut reconnect_rx) =
            mpsc::channel::<Arc<str>>(self.params.reconnect_queue.max(1));
        let reconnect = ReconnectHandle::new(reconnect_tx);

        let mut attempt: u64 = 0;
        let mut retry = RetryController::new(&self.params.retry_policy);

        loop {
            if self.cancel.is_cancelled() {
                self.publish_state(ConnectionState::arc_now(Phase::Disconnected, attempt));
                self.handle_cell.store(None);
                return;
            }

            attempt = attempt.saturating_add(1);
            self.publish_state(ConnectionState::arc_now(Phase::Connecting, attempt));

            let attempt_span =
                info_span!(parent: &self.span, "supervision-attempt", attempt = attempt);
            let attempt_cancel = self.cancel.child_token();
            let ctx = SessionContext {
                cancel: attempt_cancel.clone(),
                reconnect: reconnect.clone(),
                span: attempt_span,
                attempt,
            };

            // Phase A: connect
            let mut session = match self.connector.connect(ctx.clone()).await {
                Ok(s) => s,
                Err(err) => {
                    let report = self.build_failure_report(FailurePhase::Connect, &err);
                    match report.kind {
                        FailureKind::Stop | FailureKind::Fatal => {
                            self.publish_failure_and_state(Phase::Failed, attempt, report);
                            return;
                        }
                        FailureKind::Retryable => {
                            if self.backoff_or_fail(&mut retry, attempt, report).await {
                                continue;
                            }
                            return;
                        }
                    }
                }
            };

            // Phase B: init
            self.publish_state(ConnectionState::arc_now(Phase::Initializing, attempt));
            if let Err(err) = session.init(&ctx).await {
                let report = self.build_failure_report(FailurePhase::Init, &err);
                match report.kind {
                    FailureKind::Stop | FailureKind::Fatal => {
                        self.publish_failure_and_state(Phase::Failed, attempt, report);
                        return;
                    }
                    FailureKind::Retryable => {
                        if self.backoff_or_fail(&mut retry, attempt, report).await {
                            continue;
                        }
                        return;
                    }
                }
            }

            // Phase C: connected/ready - publish handle
            retry.reset();
            self.handle_cell.store(Some(Arc::clone(session.handle())));
            self.publish_state(ConnectionState::arc_now(Phase::Connected, attempt));

            // Drain stale reconnect requests.
            while reconnect_rx.try_recv().is_ok() {}

            // Drive the session until completion, or interrupt by reconnect request / cancellation.
            let run_ctx = ctx.clone();
            // IMPORTANT:
            // `tokio::spawn` does NOT inherit the current tracing span by default.
            // We MUST instrument the spawned `run()` future with the attempt span so
            // per-channel / per-app dynamic log level overrides can work reliably.
            let run_span = run_ctx.span.clone();
            let mut run_task =
                tokio::spawn(async move { session.run(run_ctx).await }.instrument(run_span));

            enum StopReason<E> {
                Cancelled,
                Reconnect(Option<Arc<str>>),
                Done(Result<Result<RunOutcome, E>, tokio::task::JoinError>),
            }

            let reason: StopReason<<C::Session as Session>::Error> = tokio::select! {
                _ = self.cancel.cancelled() => StopReason::Cancelled,
                req = reconnect_rx.recv() => StopReason::Reconnect(req),
                join = &mut run_task => StopReason::Done(join),
            };

            let outcome = match reason {
                StopReason::Cancelled => {
                    attempt_cancel.cancel();
                    let _ = run_task.await;
                    RunOutcome::Disconnected
                }
                StopReason::Reconnect(req) => {
                    attempt_cancel.cancel();
                    let _ = run_task.await;
                    match req {
                        Some(reason) => RunOutcome::ReconnectRequested(reason),
                        None => RunOutcome::Disconnected,
                    }
                }
                StopReason::Done(join) => match join {
                    Ok(out) => match out {
                        Ok(outcome) => outcome,
                        Err(err) => {
                            RunOutcome::Fatal(self.build_failure_report(FailurePhase::Run, &err))
                        }
                    },
                    Err(join_err) => RunOutcome::Fatal(FailureReport {
                        phase: FailurePhase::Run,
                        kind: FailureKind::Fatal,
                        summary: Arc::<str>::from(join_err.to_string()),
                        code: Some(Arc::<str>::from("join_error")),
                    }),
                },
            };

            // Clear handle before next attempt.
            self.handle_cell.store(None);

            match outcome {
                RunOutcome::Disconnected => {
                    self.publish_state(ConnectionState::arc_now(Phase::Disconnected, attempt));
                    // reconnect based on policy (treated as retryable run failure)
                    let report = FailureReport {
                        phase: FailurePhase::Run,
                        kind: FailureKind::Retryable,
                        summary: Arc::<str>::from("disconnected"),
                        code: Some(Arc::<str>::from("disconnected")),
                    };
                    if self.backoff_or_fail(&mut retry, attempt, report).await {
                        continue;
                    }
                    return;
                }
                RunOutcome::ReconnectRequested(reason) => {
                    self.publish_state(ConnectionState::arc_now(Phase::Reconnecting, attempt));
                    // Reset retry on explicit reconnect (treat as healthy cycle boundary).
                    retry.reset();
                    let _ = reason;
                    continue;
                }
                RunOutcome::Fatal(report) => {
                    self.publish_failure_and_state(Phase::Failed, attempt, report);
                    return;
                }
            }
        }
    }

    fn publish_state(&self, state: Arc<ConnectionState>) {
        let _ = self.state_tx.send(Arc::clone(&state));
        self.observer.on_state(state.as_ref());
    }

    fn publish_failure_and_state(&self, phase: Phase, attempt: u64, report: FailureReport) {
        self.observer.on_failure(&report);
        let report = Arc::new(report);
        let mut st = ConnectionState::now(phase, attempt);
        st.last_failure = Some(report);
        self.publish_state(Arc::new(st));
        self.handle_cell.store(None);
    }

    #[inline]
    fn build_failure_report(
        &self,
        phase: FailurePhase,
        err: &<C::Session as Session>::Error,
    ) -> FailureReport {
        // NOTE:
        // We rely on the connector to classify and provide a low-cost summary string.
        let kind = self.connector.classify_error(phase, err);
        let summary = self.connector.error_summary(err);
        let code = self.connector.error_code(err);
        FailureReport {
            phase,
            kind,
            summary,
            code,
        }
    }

    async fn backoff_or_fail(
        &self,
        retry: &mut RetryController,
        attempt: u64,
        report: FailureReport,
    ) -> bool {
        self.observer.on_failure(&report);
        match retry.on_failure() {
            RetryDecision::RetryAfter(delay) => {
                let budget = retry.budget_snapshot();
                self.observer.on_backoff(delay, &budget);
                let mut st = ConnectionState::now(Phase::Reconnecting, attempt);
                st.backoff = Some(delay);
                st.last_failure = Some(Arc::new(report));
                st.budget = budget;
                self.publish_state(Arc::new(st));
                tokio::select! {
                    _ = self.cancel.cancelled() => false,
                    _ = tokio::time::sleep(delay) => true,
                }
            }
            RetryDecision::Exhausted => {
                let mut st = ConnectionState::now(Phase::Failed, attempt);
                st.last_failure = Some(Arc::new(report));
                st.budget = RetryBudgetSnapshot {
                    exhausted: true,
                    remaining_hint: Some(0),
                };
                self.publish_state(Arc::new(st));
                false
            }
        }
    }
}
