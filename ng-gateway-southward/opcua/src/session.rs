//! OPC UA supervised session implementation.
//!
//! This session drives the `opcua::client::SessionEventLoop` and publishes a live
//! `opcua::client::Session` into `OpcUaHandle` for data-plane reads/writes.
//!
//! `Session::init()` waits until the underlying event loop reports the first active state,
//! then probes server capabilities and (optionally) starts subscription management.

use super::{
    capacity::probe_capacity, connector::MeteredTcpConnector, handle::OpcUaHandle,
    subscribe::SubscriptionCommand,
};
use futures::{pin_mut, StreamExt};
use ng_gateway_sdk::{
    supervision::{RunOutcome, Session, SessionContext},
    DriverError,
};
use opcua::client::{Session as UaSession, SessionActivity, SessionEventLoop, SessionPollResult};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

#[derive(Debug, Clone)]
enum LoopEvent {
    Active,
    ConnectionLost,
    Error,
}

/// OPC UA attempt session.
pub struct OpcUaSession {
    handle: Arc<OpcUaHandle>,
    session: Arc<UaSession>,
    ev: Option<SessionEventLoop<MeteredTcpConnector>>,
    rx: Option<mpsc::Receiver<LoopEvent>>,
}

impl OpcUaSession {
    pub(crate) fn new(
        handle: Arc<OpcUaHandle>,
        session: Arc<UaSession>,
        ev: SessionEventLoop<MeteredTcpConnector>,
    ) -> Self {
        Self {
            handle,
            session,
            ev: Some(ev),
            rx: None,
        }
    }

    async fn wait_first_active(
        rx: &mut mpsc::Receiver<LoopEvent>,
        cancel: &CancellationToken,
    ) -> Result<(), DriverError> {
        // Wait for the first "Active" signal from the event loop driver task.
        // If the loop ends or reports failure before becoming active, treat the attempt as failed.
        tokio::select! {
            _ = cancel.cancelled() => Err(DriverError::ServiceUnavailable),
            maybe = rx.recv() => {
                match maybe {
                    Some(LoopEvent::Active) => Ok(()),
                    Some(LoopEvent::ConnectionLost) | Some(LoopEvent::Error) | None => {
                        Err(DriverError::SessionError("OPC UA session did not become active".to_string()))
                    }
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl Session for OpcUaSession {
    type Handle = OpcUaHandle;
    type Error = DriverError;

    fn handle(&self) -> &Arc<Self::Handle> {
        &self.handle
    }

    async fn init(&mut self, ctx: &SessionContext) -> Result<(), Self::Error> {
        self.handle.set_reconnect(ctx.reconnect.clone());

        let ev = self.ev.take().ok_or(DriverError::SessionError(
            "OPC UA event loop already taken".to_string(),
        ))?;

        let (tx, mut rx) = mpsc::channel::<LoopEvent>(64);
        let cancel = ctx.cancel.clone();
        let session = Arc::clone(&self.session);
        let handle = Arc::clone(&self.handle);

        // Spawn the event loop driver task (attempt-scoped).
        //
        // IMPORTANT:
        // Preserve the current tracing span (contains `channel_id`) for the spawned task,
        // so per-channel dynamic log level overrides can work reliably.
        tokio::spawn(
            async move {
                let stream = ev.enter();
                pin_mut!(stream);
                while let Some(item) = tokio::select! {
                    _ = cancel.cancelled() => None,
                    v = stream.next() => v,
                } {
                    match item {
                        Ok(poll) => match poll {
                            SessionPollResult::Reconnected(_) | SessionPollResult::Transport(_) => {
                                let _ = tx.send(LoopEvent::Active).await;
                            }
                            SessionPollResult::SessionActivity(act) => {
                                if matches!(act, SessionActivity::KeepAliveSucceeded) {
                                    let _ = tx.send(LoopEvent::Active).await;
                                }
                            }
                            SessionPollResult::ConnectionLost(_) => {
                                let _ = tx.send(LoopEvent::ConnectionLost).await;
                                break;
                            }
                            _ => {}
                        },
                        Err(_) => {
                            let _ = tx.send(LoopEvent::Error).await;
                            break;
                        }
                    }
                }

                // On cancellation or termination: detach session from handle.
                handle.detach_session();

                // Best-effort close.
                session.disable_reconnects();
                let _ =
                    tokio::time::timeout(std::time::Duration::from_secs(2), session.disconnect())
                        .await;
            }
            .in_current_span(),
        );

        // Wait until active (Initializing phase).
        Self::wait_first_active(&mut rx, &ctx.cancel).await?;

        // Publish session for data-plane.
        self.handle.attach_session(Arc::clone(&self.session));

        // Capacity probe (best-effort, but done before returning init for stable batching).
        let cap = probe_capacity(&self.session).await;
        let mut effective = opcua::types::constants::MAX_ARRAY_LENGTH.max(1);
        if let Some(n) = cap.read.max_nodes_per_read {
            effective = effective.min(n as usize);
        }
        if let Some(n) = cap.read.max_array_length {
            effective = effective.min(n as usize);
        }
        self.handle.set_read_chunk_size(effective.max(1));

        // Start subscription manager (attempt-scoped) and notify it with the new session.
        if let Some(mgr) = self
            .handle
            .replace_subscription_manager(ctx.cancel.child_token())
            .await
        {
            mgr.send_command(SubscriptionCommand::NewSession {
                session: Arc::clone(&self.session),
                capacity: cap.subscription,
            })
            .await;
        }

        self.rx = Some(rx);
        Ok(())
    }

    async fn run(self, ctx: SessionContext) -> Result<RunOutcome, Self::Error> {
        let mut rx = self.rx.unwrap_or_else(|| {
            // Should not happen: init always sets rx.
            let (_tx, rx) = mpsc::channel(1);
            rx
        });

        loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => {
                    self.handle.detach_session();
                    return Ok(RunOutcome::Disconnected);
                }
                ev = rx.recv() => {
                    match ev {
                        Some(LoopEvent::Active) => {
                            // keep running
                        }
                        Some(LoopEvent::ConnectionLost) | Some(LoopEvent::Error) | None => {
                            self.handle.detach_session();
                            return Ok(RunOutcome::ReconnectRequested(Arc::<str>::from(
                                "opcua event loop ended",
                            )));
                        }
                    }
                }
            }
        }
    }
}
