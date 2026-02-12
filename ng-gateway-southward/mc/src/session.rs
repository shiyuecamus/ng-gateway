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
        // Merge lifecycle watchers from all sessions: if ANY session goes
        // Closed/Failed, we request reconnect for the entire pool.
        let mut lifecycle_rxs: Vec<watch::Receiver<SessionLifecycleState>> =
            self.proto_sessions.iter().map(|s| s.lifecycle()).collect();

        loop {
            // Build a future that resolves when ANY lifecycle watcher fires.
            let any_changed = async {
                // Wait for the first watcher to change.
                for rx in lifecycle_rxs.iter_mut() {
                    // Use tokio::select! across all watchers.
                    // Since we can't dynamically select! on a Vec, we use a helper.
                    if rx.has_changed().unwrap_or(true) {
                        let state = *rx.borrow_and_update();
                        if matches!(
                            state,
                            SessionLifecycleState::Closed | SessionLifecycleState::Failed
                        ) {
                            return true;
                        }
                    }
                }
                // If no immediate change, wait for any watcher to fire.
                let _ = futures::future::select_all(
                    lifecycle_rxs.iter_mut().map(|rx| Box::pin(rx.changed())),
                )
                .await;
                // Re-check states after a change.
                for rx in lifecycle_rxs.iter() {
                    let state = *rx.borrow();
                    if matches!(
                        state,
                        SessionLifecycleState::Closed | SessionLifecycleState::Failed
                    ) {
                        return true;
                    }
                }
                false
            };

            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    if let Some(pool) = self.handle.detach_pool() {
                        pool.shutdown_all().await;
                    }
                    return Ok(RunOutcome::Disconnected);
                }
                failed = any_changed => {
                    if failed {
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
    }
}
