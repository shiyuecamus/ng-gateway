use crate::{
    protocol::{
        error::ProtocolError,
        session::{Dl645Session, Dl645SessionImpl, SessionConfig},
    },
    types::{Dl645Channel, Dl645ChannelConfig, Dl645Connection},
};
use arc_swap::ArcSwapOption;
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout, DriverError, DriverResult,
    RetryController, RetryDecision, SerialConnectConfig, SouthwardConnectionState,
    SouthwardTransportMeter,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::{mpsc, watch, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

impl From<ProtocolError> for DriverError {
    /// Map protocol-level errors into the gateway's `DriverError` domain.
    ///
    /// - Codec/structural issues are treated as `CodecError`.
    /// - Semantic and device-level exceptions are mapped to `ExecutionError`.
    /// - Timeouts are mapped to `Timeout`, preserving the duration.
    /// - Transport/IO failures are mapped to `SessionError` so that the
    ///   supervisor can treat them as fatal for the underlying link.
    fn from(err: ProtocolError) -> Self {
        match err {
            ProtocolError::InvalidFrame(_)
            | ProtocolError::ChecksumMismatch
            | ProtocolError::InvalidControl(_)
            | ProtocolError::FrameTooLarge(_) => DriverError::CodecError(err.to_string()),
            ProtocolError::Exception(_) | ProtocolError::Semantic(_) => {
                DriverError::ExecutionError(err.to_string())
            }
            ProtocolError::Timeout(d) => DriverError::Timeout(d),
            ProtocolError::Transport(msg) => DriverError::SessionError(msg),
            ProtocolError::Io(e) => DriverError::SessionError(e.to_string()),
        }
    }
}

/// Lightweight wrapper around a DL/T 645 session trait object to allow
/// storing it inside `ArcSwapOption` (which requires a sized type).
pub struct SessionHandle(pub Arc<dyn Dl645Session>);

/// Shared DL/T 645 session entry for a single channel.
///
/// This structure provides lock-free access to the underlying session in the
/// data-plane and aggregates basic health and error counters for observability.
pub struct SessionEntry {
    /// Current active session if any (lock-free access for hot path).
    pub session: ArcSwapOption<SessionHandle>,
    /// Health indicator for fast-path checks.
    pub healthy: AtomicBool,
    /// Shutdown flag set when supervisor stops.
    pub shutdown: AtomicBool,
    /// Last error message for observability.
    pub last_error: Mutex<Option<String>>,
    /// Number of consecutive timeouts observed on the data-plane.
    ///
    /// This counter is incremented for each request-level timeout reported by
    /// the bus event loop and reset on any successful response. When the value
    /// reaches `timeout_reconnect_threshold`, the bus loop will trigger a
    /// reconnect cycle via `reconnect_tx`.
    pub consecutive_timeouts: AtomicU64,
    /// Threshold at which consecutive timeouts should force a reconnect.
    ///
    /// This value is derived from `Dl645ChannelConfig::max_consecutive_timeouts_before_reconnect`
    /// and treated as read‑only during runtime.
    pub timeout_reconnect_threshold: u32,
    /// Sender side used by data-plane to request a reconnect.
    ///
    /// The corresponding receiver is owned by `Dl645Supervisor` which performs
    /// teardown and exponential backoff according to channel policy.
    pub reconnect_tx: mpsc::Sender<()>,
}

impl SessionEntry {
    /// Create an empty shared entry with no active session.
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

/// Shared pointer type for session entry.
pub type SharedSession = Arc<SessionEntry>;

/// High-availability supervisor for a single DL/T 645 bus.
///
/// The supervisor owns the session lifecycle and performs reconnect with
/// exponential backoff based on the channel's `ConnectionPolicy`.
pub struct Dl645Supervisor {
    /// Shared session state and health for data-plane usage.
    pub shared: SharedSession,
    /// Driver-level cancellation token used to terminate the supervisor loop.
    cancel_token: CancellationToken,
    /// Connection state watch sender maintained by supervisor.
    state_tx: watch::Sender<SouthwardConnectionState>,
    /// Receiver for reconnect requests emitted by the data-plane.
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    started: AtomicBool,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl Dl645Supervisor {
    /// Create a new supervisor for the given shared session and cancellation token.
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

    /// Establish a single DL/T 645 session according to the provided channel config.
    ///
    /// This helper mirrors the `connect_once` pattern used by the Modbus driver
    /// and keeps transport-specific logic encapsulated while the supervisor
    /// focuses on lifecycle and backoff.
    async fn connect_once(
        cfg: &Dl645ChannelConfig,
        connect_timeout_ms: u64,
        transport_meter: &Arc<dyn SouthwardTransportMeter>,
    ) -> DriverResult<Arc<dyn Dl645Session>> {
        match &cfg.connection {
            Dl645Connection::Serial {
                port,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                let session_cfg = SessionConfig::new(cfg.wakeup_preamble.clone(), cfg.version);
                let io = connect_serial_metered(
                    SerialConnectConfig {
                        port: port.to_string(),
                        baud_rate: *baud_rate,
                        data_bits: (*data_bits).into(),
                        stop_bits: (*stop_bits).into(),
                        parity: (*parity).into(),
                    },
                    Arc::clone(transport_meter),
                )
                .map_err(|e| DriverError::SessionError(e.to_string()))?;
                Ok(Arc::new(Dl645SessionImpl::new(io, session_cfg)))
            }
            Dl645Connection::Tcp { host, port } => {
                let addr = format!("{}:{}", host, port)
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!(
                            "Invalid DL/T 645 TCP address {host}:{port}: {e}"
                        ))
                    })?;
                let session_cfg = SessionConfig::new(cfg.wakeup_preamble.clone(), cfg.version);
                let io = connect_tcp_metered_with_timeout(
                    addr,
                    Arc::clone(transport_meter),
                    connect_timeout_ms,
                )
                .await
                .map_err(|e| DriverError::SessionError(format!("TCP connect failed: {e}")))?;
                Ok(Arc::new(Dl645SessionImpl::new(io, session_cfg)))
            }
        }
    }

    /// Run supervisor loop: maintain a single healthy session and reconnect on demand.
    ///
    /// This mirrors the Modbus `SessionSupervisor` design: a single outer loop with
    /// exponential backoff which owns the underlying `Dl645Session` lifecycle.
    pub async fn run(&self, channel: Arc<Dl645Channel>) -> DriverResult<()> {
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
        let span = tracing::info_span!("dlt645-supervisor", channel_id = channel.id);
        tokio::spawn(async move {
            // reset flags
            shared.shutdown.store(false, Ordering::Release);

            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);

                // Connect with unified retry controller.
                let mut retry = RetryController::new(&channel.connection_policy.backoff);

                let session: Arc<dyn Dl645Session> = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx
                            .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        return;
                    }

                    match Self::connect_once(
                        &channel.config,
                        channel.connection_policy.connect_timeout_ms,
                        &transport_meter,
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
                                            shared.healthy.store(false, Ordering::Release);
                                            let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                            return;
                                        }
                                        _ = tokio::time::sleep(delay) => {}
                                    }
                                    continue;
                                }
                                RetryDecision::Exhausted => return,
                            }
                        }
                    }
                };

                // Store session and mark healthy.
                shared
                    .session
                    .store(Some(Arc::new(SessionHandle(Arc::clone(&session)))));
                shared.healthy.store(true, Ordering::Release);
                shared.consecutive_timeouts.store(0, Ordering::Release);
                let _ = state_tx.send(SouthwardConnectionState::Connected);
                retry.reset();

                // Drain any stale notifications to avoid immediate redundant reconnect.
                while reconnect_rx.try_recv().is_ok() {}

                // Wait for either external cancel or reconnect request.
                tokio::select! {
                    _ = cancel.cancelled() => {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.session.store(None);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        // Drop connection and restart loop.
                        shared.healthy.store(false, Ordering::Release);
                        shared.session.store(None);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                        // continue outer loop
                    }
                }
            }
        }
        .instrument(span));
        Ok(())
    }
}
