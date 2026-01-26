use crate::{
    protocol::session::{Cjt188Session, Cjt188SessionImpl, SessionConfig},
    types::{Cjt188Channel, Cjt188Connection},
};
use arc_swap::ArcSwapOption;
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout, DriverError, DriverResult,
    RetryController, RetryDecision, SerialConnectConfig, SouthwardConnectionState,
    SouthwardTransportMeter,
};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    time::sleep,
};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

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
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    started: AtomicBool,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl Cjt188Supervisor {
    pub fn new(
        shared: SharedSession,
        cancel_token: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
        reconnect_rx: mpsc::Receiver<()>,
        transport_meter: Arc<dyn SouthwardTransportMeter>,
    ) -> Self {
        Self {
            shared,
            cancel_token,
            state_tx,
            reconnect_rx: Mutex::new(Some(reconnect_rx)),
            started: AtomicBool::new(false),
            transport_meter,
        }
    }

    async fn connect_once(
        cfg: &Cjt188Channel,
        connect_timeout_ms: u64,
        transport_meter: Arc<dyn SouthwardTransportMeter>,
    ) -> DriverResult<Arc<dyn Cjt188Session>> {
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
                let io = connect_serial_metered(
                    SerialConnectConfig {
                        port: port.to_string(),
                        baud_rate: *baud_rate,
                        data_bits: (*data_bits).into(),
                        stop_bits: (*stop_bits).into(),
                        parity: (*parity).into(),
                    },
                    Arc::clone(&transport_meter),
                )
                .map_err(|e| DriverError::SessionError(e.to_string()))?;
                Ok(Arc::new(Cjt188SessionImpl::new(io, session_cfg, version)))
            }
            Cjt188Connection::Tcp { host, port } => {
                let addr = format!("{}:{}", host, port)
                    .parse::<std::net::SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid CJ/T 188 TCP address {host}:{port}: {e}"
                        ))
                    })?;
                let io = connect_tcp_metered_with_timeout(
                    addr,
                    Arc::clone(&transport_meter),
                    connect_timeout_ms,
                )
                .await
                .map_err(|e| DriverError::SessionError(format!("TCP connect failed: {e}")))?;
                Ok(Arc::new(Cjt188SessionImpl::new(io, session_cfg, version)))
            }
        }
    }

    pub async fn run(&self, channel: Arc<Cjt188Channel>) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();
        let mut reconnect_rx = self
            .reconnect_rx
            .lock()
            .await
            .take()
            .ok_or(DriverError::ExecutionError("reconnect rx consumed".into()))?;
        let transport_meter = Arc::clone(&self.transport_meter);

        // Ensure the supervisor task inherits the driver's `channel_id` span so that
        // dependency logs remain attributable and per-channel filtering works on the host.
        let span = tracing::info_span!("cjt188-supervisor", channel_id = channel.id);
        tokio::spawn(async move {
            shared.shutdown.store(false, Ordering::Release);

            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);

                let mut retry = RetryController::new(&channel.connection_policy.backoff);

                let session: Arc<dyn Cjt188Session> = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                        return;
                    }

                    match Self::connect_once(
                        &channel,
                        channel.connection_policy.connect_timeout_ms,
                        Arc::clone(&transport_meter),
                    )
                    .await
                    {
                        Ok(sess) => break sess,
                        Err(e) => {
                            {
                                let mut last = shared.last_error.lock().await;
                                *last = Some(e.to_string());
                            }
                            shared.healthy.store(false, Ordering::Relaxed);
                            let msg = e.to_string();
                            let _ = state_tx.send(SouthwardConnectionState::Failed(msg));
                            match retry.on_failure() {
                                RetryDecision::RetryAfter(delay) => {
                                    let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                    tokio::select! {
                                        _ = cancel.cancelled() => {
                                            shared.shutdown.store(true, Ordering::Release);
                                            let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                            return;
                                        }
                                        _ = sleep(delay) => {}
                                    }
                                    continue;
                                }
                                RetryDecision::Exhausted => return,
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
                        let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        shared.healthy.store(false, Ordering::Release);
                        shared.session.store(None);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                    }
                }
            }
        }
        .instrument(span));
        Ok(())
    }
}
