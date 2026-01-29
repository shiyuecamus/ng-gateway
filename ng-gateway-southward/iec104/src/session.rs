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
use std::sync::Arc;
use tokio::{
    task::JoinHandle,
    time::{timeout, Duration},
};
use tokio_util::sync::CancellationToken;

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
        for action in self.startup_actions.clone() {
            tokio::select! {
                _ = ctx.cancel.cancelled() => break,
                res = run_startup_action(
                    Arc::clone(&self.proto_session),
                    self.channel.connection_policy.write_timeout_ms.max(1),
                    action
                ) => { let _ = res; }
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
                        Some(mut asdu) => self.handle.process_asdu(&mut asdu),
                        None => break,
                    }
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

async fn run_startup_action(
    session: Arc<ProtoSession>,
    timeout_ms: u64,
    action: StartupAction,
) -> DriverResult<()> {
    let cot = CauseOfTransmission::new(false, false, Cause::Activation);
    match action {
        StartupAction::CounterInterrogation { ca, qcc } => {
            let qcc = ObjectQCC::new(qcc);
            Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                session.counter_interrogation_cmd(cot, ca, qcc).await
            })
            .await
        }
        StartupAction::GeneralInterrogation { ca, qoi } => {
            let qoi = ObjectQOI::new(qoi);
            Iec104Handle::spawn_session_with_timeout(timeout_ms, async move {
                session.interrogation_cmd(cot, ca, qoi).await
            })
            .await
        }
    }
}
