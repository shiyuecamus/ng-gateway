//! S7 supervised session implementation.
//!
//! This session owns the attempt-scoped protocol resources:
//! - connected transport (via `create_with_stream`)
//! - protocol IO driver spawned by `SessionEventLoop`
//!
//! It attaches the protocol session into `S7Handle` once Ready.

use super::{
    handle::S7Handle,
    protocol::session::{Session as ProtoSession, SessionEventLoop, SessionLifecycleState},
};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;

/// S7 attempt session.
pub struct S7Session {
    handle: Arc<S7Handle>,
    proto_session: Arc<ProtoSession>,
    event_loop: Option<SessionEventLoop>,
}

impl S7Session {
    /// Create an attempt session from connected protocol primitives.
    #[inline]
    pub fn new(
        handle: Arc<S7Handle>,
        proto_session: Arc<ProtoSession>,
        event_loop: SessionEventLoop,
    ) -> Self {
        Self {
            handle,
            proto_session,
            event_loop: Some(event_loop),
        }
    }
}

#[async_trait::async_trait]
impl Session for S7Session {
    type Handle = S7Handle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        // Start protocol IO driver.
        let ev = self.event_loop.take().ok_or(DriverError::ExecutionError(
            "s7 event_loop consumed".to_string(),
        ))?;
        let _join = ev.spawn();

        // Wait for the protocol session to become Active.
        let active = tokio::select! {
            _ = ctx.cancel.cancelled() => false,
            ok = self.proto_session.wait_for_active() => ok,
        };
        if !active {
            let _ = self.proto_session.shutdown().await;
            return Err(DriverError::SessionError(
                "s7 protocol session did not become Active".to_string(),
            ));
        }

        // Publish data-plane handle.
        self.handle.attach_session(Arc::clone(&self.proto_session));
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let mut lifecycle_rx = self.proto_session.lifecycle();
        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    self.handle.detach_session();
                    let _ = self.proto_session.shutdown().await;
                    return Ok(RunOutcome::Disconnected);
                }
                res = lifecycle_rx.changed() => {
                    if res.is_err() {
                        self.handle.detach_session();
                        let _ = self.proto_session.shutdown().await;
                        return Ok(RunOutcome::Disconnected);
                    }
                    let state = *lifecycle_rx.borrow();
                    match state {
                        | SessionLifecycleState::Closed
                        | SessionLifecycleState::Failed => {
                            self.handle.detach_session();
                            let _ = self.proto_session.shutdown().await;
                            return Ok(RunOutcome::ReconnectRequested(Arc::<str>::from(
                                "s7 protocol session ended",
                            )));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
