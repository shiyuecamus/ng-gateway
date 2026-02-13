//! MC supervised session implementation.
//!
//! This session owns the attempt-scoped protocol resources:
//! - connected transports (via `create_with_stream`)
//! - protocol IO drivers spawned by `SessionEventLoop`
//!
//! It attaches a session pool into `McHandle` once all connections are Ready.

use crate::{
    handle::{McHandle, McSessionPool},
    protocol::session::{Session as ProtoSession, SessionEventLoop, SessionLifecycleState},
};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use std::sync::Arc;
use tokio::sync::watch;

/// MC attempt session with a pool of protocol connections.
pub struct McSession {
    handle: Arc<McHandle>,
    proto_sessions: Vec<Arc<ProtoSession>>,
    event_loops: Vec<Option<SessionEventLoop>>,
}

impl McSession {
    /// Create an attempt session from connected protocol primitives.
    ///
    /// For a pool of size N, pass N proto_sessions and N event_loops.
    /// For backward compatibility (pool_size=1), pass a single-element vector.
    #[inline]
    pub fn new(
        handle: Arc<McHandle>,
        proto_sessions: Vec<Arc<ProtoSession>>,
        event_loops: Vec<SessionEventLoop>,
    ) -> Self {
        Self {
            handle,
            proto_sessions,
            event_loops: event_loops.into_iter().map(Some).collect(),
        }
    }
}

#[async_trait::async_trait]
impl Session for McSession {
    type Handle = McHandle;
    type Error = DriverError;

    #[inline]
    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        // Start all protocol IO drivers.
        for ev in self.event_loops.iter_mut() {
            if let Some(ev) = ev.take() {
                let _join = ev.spawn();
            }
        }

        // Wait for all protocol sessions to become Active.
        for session in &self.proto_sessions {
            let active = tokio::select! {
                _ = ctx.cancel.cancelled() => false,
                ok = session.wait_for_active() => ok,
            };
            if !active {
                // Shutdown all sessions on partial failure via a temporary pool.
                let pool = McSessionPool::new(self.proto_sessions.clone());
                pool.shutdown_all().await;
                return Err(DriverError::SessionError(
                    "mc protocol session did not become Active".to_string(),
                ));
            }
        }

        // Publish pool to data-plane handle.
        let pool = McSessionPool::new(self.proto_sessions.clone());
        self.handle.attach_pool(Arc::new(pool));
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        // Monitor all pool member lifecycles concurrently.
        //
        // Each watcher waits for its session to reach Closed or Failed via
        // `wait_for`, which correctly calls `borrow_and_update()` internally,
        // avoiding the state-tracking bugs of manual `has_changed` + `select_all`.
        let mut lifecycle_rxs: Vec<watch::Receiver<SessionLifecycleState>> =
            self.proto_sessions.iter().map(|s| s.lifecycle()).collect();

        // Build a future per watcher that resolves when the session reaches
        // Closed or Failed. `select_all` resolves on the first one, then we
        // trigger a full pool reconnect.
        let futs = lifecycle_rxs.iter_mut().map(|rx| {
            Box::pin(async move {
                rx.wait_for(|s| {
                    matches!(
                        s,
                        SessionLifecycleState::Closed | SessionLifecycleState::Failed
                    )
                })
                .await
                .is_ok()
            })
        });

        tokio::select! {
            _ = ctx.cancel.cancelled() => {
                if let Some(pool) = self.handle.detach_pool() {
                    pool.shutdown_all().await;
                }
                return Ok(RunOutcome::Disconnected);
            }
            (_failed, _idx, _remaining) = futures::future::select_all(futs) => {
                // Any session ended (Closed/Failed) or the watcher channel closed.
                // Either way, tear down the entire pool and request reconnect.
                if let Some(pool) = self.handle.detach_pool() {
                    pool.shutdown_all().await;
                }
                return Ok(RunOutcome::ReconnectRequested(Arc::<str>::from(
                    "mc protocol session ended",
                )));
            }
        }
    }
}
