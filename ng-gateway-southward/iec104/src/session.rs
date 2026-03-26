//! IEC104 supervised session implementation.
//!
//! This module contains the per-attempt session lifecycle that the SDK supervisor drives:
//! - `init()`: define readiness (IEC104 link becomes Active) and perform optional startup actions.
//! - `run()`: drain incoming ASDUs and publish northward data until disconnect/cancel.

use super::{
    handle::{Iec104Handle, StartupAction},
    protocol::{
        session::{Session as ProtoSession, SessionEventLoop, SessionLifecycleState},
        Cause, CauseOfTransmission, ObjectQCC, ObjectQOI,
    },
    types::Iec104Channel,
};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError, DriverResult,
};
use std::{pin::Pin, sync::Arc};
use tokio::{
    task::JoinHandle,
    time::{timeout, Duration, Interval, Sleep},
};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Interrogation target — lightweight, Copy, no enum overhead per tick
// ---------------------------------------------------------------------------

/// Pre-resolved target for a single interrogation command (GI or CI).
#[derive(Debug, Clone, Copy)]
enum InterrogationTarget {
    General { ca: u16, qoi: u8 },
    Counter { ca: u16, qcc: u8 },
}

impl InterrogationTarget {
    /// Best-effort send. Errors are logged, never propagated.
    async fn fire(self, session: &Arc<ProtoSession>, timeout_ms: u64) {
        let cot = CauseOfTransmission::new(false, false, Cause::Activation);
        match self {
            Self::General { ca, qoi } => {
                let s = Arc::clone(session);
                let qoi = ObjectQOI::new(qoi);
                match Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                    s.interrogation_cmd(cot, ca, qoi).await
                })
                .await
                {
                    Ok(()) => tracing::debug!(ca, "periodic GI sent"),
                    Err(e) => tracing::warn!(ca, error = %e, "periodic GI failed"),
                }
            }
            Self::Counter { ca, qcc } => {
                let s = Arc::clone(session);
                let qcc = ObjectQCC::new(qcc);
                match Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                    s.counter_interrogation_cmd(cot, ca, qcc).await
                })
                .await
                {
                    Ok(()) => tracing::debug!(ca, "periodic CI sent"),
                    Err(e) => tracing::warn!(ca, error = %e, "periodic CI failed"),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Deadline — resettable one-shot timer (zero-cost when disabled)
// ---------------------------------------------------------------------------

/// Resettable one-shot deadline backed by a pinned `tokio::time::Sleep`.
///
/// When `None`, `.wait()` pends forever with zero polling overhead.
/// When `Some`, the inner `Sleep` is reset in-place on each `reset()` —
/// no allocation, no future reconstruction per loop iteration.
struct Deadline(Option<Pin<Box<Sleep>>>);

impl Deadline {
    fn new(dur: Option<Duration>) -> Self {
        Self(dur.map(|d| Box::pin(tokio::time::sleep(d))))
    }

    /// Reset the inner timer from "now".  No-op when disabled.
    fn reset(&mut self, dur: Duration) {
        if let Some(ref mut s) = self.0 {
            s.as_mut().reset(tokio::time::Instant::now() + dur);
        }
    }

    /// Await the deadline.  Pends forever when disabled.
    async fn wait(&mut self) {
        match self.0 {
            Some(ref mut s) => s.as_mut().await,
            None => std::future::pending::<()>().await,
        }
    }
}

// ---------------------------------------------------------------------------
// OptionalInterval — zero-cost wrapper for an optional periodic timer
// ---------------------------------------------------------------------------

/// Wrapper around an optional `tokio::time::Interval`.
///
/// `tick()` pends forever when disabled — the `select!` branch is compiled
/// in but never fires, keeping the hot-path branch count constant.
struct OptionalInterval(Option<Interval>);

impl OptionalInterval {
    fn new(secs: u64) -> Self {
        if secs == 0 {
            return Self(None);
        }
        let mut iv = tokio::time::interval(Duration::from_secs(secs));
        iv.reset();
        Self(Some(iv))
    }

    async fn tick(&mut self) {
        match self.0 {
            Some(ref mut iv) => {
                iv.tick().await;
            }
            None => std::future::pending::<()>().await,
        }
    }
}

// ---------------------------------------------------------------------------
// Iec104Session
// ---------------------------------------------------------------------------

/// IEC104 supervised session for a single attempt.
pub struct Iec104Session {
    pub(crate) handle: Arc<Iec104Handle>,
    pub(crate) channel: Arc<Iec104Channel>,
    pub(crate) proto_session: Arc<ProtoSession>,
    pub(crate) event_loop: Option<SessionEventLoop>,
    pub(crate) event_loop_cancel: Option<CancellationToken>,
    pub(crate) event_loop_join: Option<JoinHandle<()>>,
    pub(crate) startup_actions: Vec<StartupAction>,
}

#[async_trait::async_trait]
impl Session for Iec104Session {
    type Handle = Iec104Handle;
    type Error = DriverError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle
            .set_reconnect_handle(Some(ctx.reconnect.clone()));

        let Some(ev) = self.event_loop.take() else {
            return Ok(());
        };

        let cancel = ev.cancel_token();
        let join = ev.spawn();
        self.event_loop_cancel = Some(cancel.clone());
        self.event_loop_join = Some(join);

        // Ready definition: link becomes Active.
        let wait_active = self
            .proto_session
            .wait_for_state(SessionLifecycleState::Active);
        let timeout_ms = self.channel.connection_policy.connect_timeout_ms.max(1);
        let ready = tokio::select! {
            _ = ctx.cancel.cancelled() => false,
            res = timeout(Duration::from_millis(timeout_ms), wait_active) => res.unwrap_or(false),
        };
        if !ready {
            cancel.cancel();
            return Err(DriverError::Timeout(Duration::from_millis(timeout_ms)));
        }

        // Publish protocol session for data-plane usage.
        self.handle
            .attach_session(Some(Arc::clone(&self.proto_session)));

        // Optional startup sequence (GI/CI).
        let write_timeout_ms = self.channel.connection_policy.write_timeout_ms.max(1);
        for action in &self.startup_actions {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                _ = fire_startup_action(&self.proto_session, write_timeout_ms, action) => {}
            }
        }

        Ok(())
    }

    async fn run(mut self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let mut asdu_rx = match self.proto_session.take_asdu_receiver().await {
            Some(rx) => rx,
            None => {
                self.handle.attach_session(None);
                return Ok(RunOutcome::Disconnected);
            }
        };

        let cfg = &self.channel.config;
        let write_timeout_ms = self.channel.connection_policy.write_timeout_ms.max(1);

        // Pre-resolve interrogation targets once — avoid per-tick enum traversal.
        let gi_targets = resolve_targets(&self.startup_actions, true);
        let ci_targets = resolve_targets(&self.startup_actions, false);

        let mut gi_tick = OptionalInterval::new(
            if cfg.auto_startup_general_interrogation && !gi_targets.is_empty() {
                cfg.periodic_gi_interval_secs
            } else {
                0
            },
        );
        let mut ci_tick = OptionalInterval::new(
            if cfg.auto_startup_counter_interrogation && !ci_targets.is_empty() {
                cfg.periodic_ci_interval_secs
            } else {
                0
            },
        );

        let silence_dur = if cfg.data_silence_timeout_secs > 0 {
            Some(Duration::from_secs(cfg.data_silence_timeout_secs))
        } else {
            None
        };
        let mut silence = Deadline::new(silence_dur);

        let mut lifecycle_rx = self.proto_session.lifecycle();

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,

                changed = lifecycle_rx.changed() => {
                    if changed.is_err() { break; }
                    match lifecycle_rx.borrow().clone() {
                        SessionLifecycleState::Closed | SessionLifecycleState::Failed(_) => break,
                        _ => {}
                    }
                }

                msg = asdu_rx.recv() => {
                    match msg {
                        Some(mut asdu) => {
                            if let Some(dur) = silence_dur {
                                silence.reset(dur);
                            }
                            self.handle.process_asdu(&mut asdu);
                        }
                        None => break,
                    }
                }

                _ = gi_tick.tick() => {
                    fire_targets(&self.proto_session, &gi_targets, write_timeout_ms).await;
                }

                _ = ci_tick.tick() => {
                    fire_targets(&self.proto_session, &ci_targets, write_timeout_ms).await;
                }

                _ = silence.wait() => {
                    tracing::warn!(
                        channel_id = self.channel.id,
                        timeout_secs = cfg.data_silence_timeout_secs,
                        "data silence timeout — no ASDU received; requesting reconnect"
                    );
                    self.handle.request_reconnect("data silence timeout");
                    break;
                }
            }
        }

        // Cleanup
        self.handle.attach_session(None);
        self.handle.set_reconnect_handle(None);
        if let Some(cancel) = self.event_loop_cancel.take() {
            cancel.cancel();
        }
        if let Some(join) = self.event_loop_join.take() {
            let _ = join.await;
        }

        Ok(RunOutcome::Disconnected)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract `InterrogationTarget`s from startup actions (once at run-start).
///
/// `gi == true` selects GI targets; `gi == false` selects CI targets.
fn resolve_targets(actions: &[StartupAction], gi: bool) -> Vec<InterrogationTarget> {
    actions
        .iter()
        .filter_map(|a| match a {
            StartupAction::GeneralInterrogation { ca, qoi } if gi => {
                Some(InterrogationTarget::General { ca: *ca, qoi: *qoi })
            }
            StartupAction::CounterInterrogation { ca, qcc } if !gi => {
                Some(InterrogationTarget::Counter { ca: *ca, qcc: *qcc })
            }
            _ => None,
        })
        .collect()
}

/// Fire all targets in a slice.  Best-effort, errors logged.
async fn fire_targets(
    session: &Arc<ProtoSession>,
    targets: &[InterrogationTarget],
    timeout_ms: u64,
) {
    for &t in targets {
        t.fire(session, timeout_ms).await;
    }
}

/// Fire a single startup action.  Used during `init()`.
async fn fire_startup_action(
    session: &Arc<ProtoSession>,
    timeout_ms: u64,
    action: &StartupAction,
) -> DriverResult<()> {
    let cot = CauseOfTransmission::new(false, false, Cause::Activation);
    match *action {
        StartupAction::CounterInterrogation { ca, qcc } => {
            let qcc = ObjectQCC::new(qcc);
            let s = Arc::clone(session);
            Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                s.counter_interrogation_cmd(cot, ca, qcc).await
            })
            .await
        }
        StartupAction::GeneralInterrogation { ca, qoi } => {
            let qoi = ObjectQOI::new(qoi);
            let s = Arc::clone(session);
            Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                s.interrogation_cmd(cot, ca, qoi).await
            })
            .await
        }
    }
}
