use crate::{
    protocol::session::{Cjt188Session, Cjt188SessionImpl, SessionConfig},
    types::{Cjt188Channel, Cjt188Connection},
};
use arc_swap::ArcSwapOption;
use backoff::backoff::Backoff;
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, SouthwardConnectionState,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch, Mutex};
use tokio::time::sleep;
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;

/// Lightweight wrapper around a session trait object.
pub struct SessionHandle(pub Arc<dyn Cjt188Session>);

/// Shared session entry.
pub struct SessionEntry {
    pub session: ArcSwapOption<SessionHandle>,
    pub healthy: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_error: Mutex<Option<String>>,
    pub consecutive_timeouts: AtomicU64,
    pub timeout_reconnect_threshold: u32,
    pub reconnect_tx: mpsc::Sender<()>,
}

impl SessionEntry {
    pub fn new_empty(reconnect_tx: mpsc::Sender<()>, timeout_reconnect_threshold: u32) -> Self {
        Self {
            session: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: Mutex::new(None),
            consecutive_timeouts: AtomicU64::new(0),
            timeout_reconnect_threshold,
            reconnect_tx,
        }
    }
}

pub type SharedSession = Arc<SessionEntry>;

/// High-availability supervisor.
pub struct Cjt188Supervisor {
    pub shared: SharedSession,
    cancel_token: CancellationToken,
    state_tx: watch::Sender<SouthwardConnectionState>,
    reconnect_rx: mpsc::Receiver<()>,
}

impl Cjt188Supervisor {
    pub fn new(
        shared: SharedSession,
        cancel_token: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
        reconnect_rx: mpsc::Receiver<()>,
    ) -> Self {
        Self {
            shared,
            cancel_token,
            state_tx,
            reconnect_rx,
        }
    }

    async fn connect_once(cfg: &Cjt188Channel) -> DriverResult<Arc<dyn Cjt188Session>> {
        let session_cfg = SessionConfig::new(cfg.config.wakeup_preamble.clone());
        let version = cfg.config.version;

        match &cfg.config.connection {
            Cjt188Connection::Serial {
                port,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                let serial = tokio_serial::new(port, *baud_rate)
                    .data_bits((*data_bits).into())
                    .stop_bits((*stop_bits).into())
                    .parity((*parity).into())
                    .open_native_async()
                    .map_err(|e| DriverError::SessionError(e.to_string()))?;

                Ok(Arc::new(Cjt188SessionImpl::new(
                    serial,
                    session_cfg,
                    version,
                )))
            }
            Cjt188Connection::Tcp { host, port } => {
                let addr = format!("{}:{}", host, port);
                let stream = tokio::net::TcpStream::connect(&addr)
                    .await
                    .map_err(|e| DriverError::SessionError(format!("TCP connect failed: {}", e)))?;
                Ok(Arc::new(Cjt188SessionImpl::new(
                    stream,
                    session_cfg,
                    version,
                )))
            }
        }
    }

    pub async fn run(self, channel: Arc<Cjt188Channel>) -> DriverResult<()> {
        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();
        let mut reconnect_rx = self.reconnect_rx;

        tokio::spawn(async move {
            shared.shutdown.store(false, Ordering::Release);

            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);

                let mut backoff = build_exponential_backoff(&channel.connection_policy.backoff);
                let mut attempt: u64 = 0;

                let session: Arc<dyn Cjt188Session> = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".into()));
                        return;
                    }

                    match Self::connect_once(&channel).await {
                        Ok(sess) => break sess,
                        Err(e) => {
                            {
                                let mut last = shared.last_error.lock().await;
                                *last = Some(e.to_string());
                            }
                            shared.healthy.store(false, Ordering::Relaxed);
                            let _ = state_tx.send(SouthwardConnectionState::Failed(e.to_string()));
                            attempt = attempt.saturating_add(1);
                            let delay = backoff.next_backoff().unwrap_or_else(|| {
                                Duration::from_millis(
                                    channel.connection_policy.backoff.max_interval_ms,
                                )
                            });

                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    shared.shutdown.store(true, Ordering::Release);
                                    return;
                                }
                                _ = sleep(delay) => {}
                            }
                        }
                    }
                };

                shared.session.store(Some(Arc::new(SessionHandle(session))));
                shared.healthy.store(true, Ordering::Release);
                shared.consecutive_timeouts.store(0, Ordering::Release);
                let _ = state_tx.send(SouthwardConnectionState::Connected);

                while reconnect_rx.try_recv().is_ok() {}

                tokio::select! {
                    _ = cancel.cancelled() => {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.session.store(None);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".into()));
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        shared.healthy.store(false, Ordering::Release);
                        shared.session.store(None);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                    }
                }
            }
        });
        Ok(())
    }
}
