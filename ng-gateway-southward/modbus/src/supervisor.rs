use crate::types::{ModbusChannel, ModbusChannelConfig, ModbusConnection};
use arc_swap::ArcSwapOption;
use ng_gateway_sdk::{
    connect_serial_metered, connect_tcp_metered_with_timeout, DriverError, DriverResult,
    RetryController, RetryDecision, SerialConnectConfig, SouthwardConnectionState,
    SouthwardTransportMeter,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::AtomicUsize,
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};
use tokio::{
    sync::{mpsc, watch, Mutex},
    time::{sleep, timeout},
};
use tokio_modbus::client::{rtu, tcp, Client as _, Context};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// A pool of Modbus contexts (each is single-flight via `Mutex<Context>`).
///
/// # Design
/// - Each `Context` is **NOT** safe for concurrent requests, hence the per-context mutex.
/// - The pool allows parallelism across different collection groups (e.g. different slave IDs)
///   on Modbus TCP, where multiple independent TCP connections can increase throughput.
pub(super) struct SessionPool {
    contexts: Vec<Arc<Mutex<Context>>>,
    rr: AtomicUsize,
}

impl SessionPool {
    /// Create a new session pool from connected contexts.
    pub fn new(contexts: Vec<Context>) -> Self {
        let contexts = contexts
            .into_iter()
            .map(|c| Arc::new(Mutex::new(c)))
            .collect::<Vec<_>>();
        Self {
            contexts,
            rr: AtomicUsize::new(0),
        }
    }

    /// Pick one context using round-robin.
    #[inline]
    pub fn pick(&self) -> Option<Arc<Mutex<Context>>> {
        let n = self.contexts.len();
        if n == 0 {
            return None;
        }
        let i = self.rr.fetch_add(1, Ordering::Relaxed) % n;
        Some(Arc::clone(&self.contexts[i]))
    }

    /// Disconnect all contexts best-effort with a per-context timeout.
    pub async fn disconnect_all(self: Arc<Self>, timeout_each: StdDuration) {
        for ctx in &self.contexts {
            let ctx = Arc::clone(ctx);
            let _ = timeout(timeout_each, async move {
                let mut g = ctx.lock().await;
                g.disconnect().await
            })
            .await;
        }
    }
}

/// Shared Modbus session entry guarded by pool selection + per-context async mutex.
/// The supervisor owns lifecycle and reconnection.
pub(super) struct SessionEntry {
    /// Underlying Modbus context pool (TCP: N, RTU: 1).
    pub pool: ArcSwapOption<SessionPool>,
    /// Health flag indicating the recent successful operations (best-effort).
    pub healthy: AtomicBool,
    /// Shutdown flag to prevent further reconnects
    pub shutdown: AtomicBool,
    /// Last error for observability
    pub last_error: std::sync::Mutex<Option<String>>,
    /// Sender side for reconnection requests; receiver is owned by the supervisor
    pub reconnect_tx: mpsc::Sender<()>,
}

impl SessionEntry {
    /// Create a new empty session entry
    pub fn new_empty(reconnect_tx: mpsc::Sender<()>) -> Self {
        Self {
            pool: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
            reconnect_tx,
        }
    }
}

/// Shared pointer type for session entry
pub(super) type SharedSession = Arc<SessionEntry>;

/// Single-connection supervisor with exponential backoff reconnection.
pub(super) struct SessionSupervisor {
    pub shared: SharedSession,
    pub cancel_token: CancellationToken,
    pub state_tx: watch::Sender<SouthwardConnectionState>,
    reconnect_rx: Mutex<Option<mpsc::Receiver<()>>>,
    started: AtomicBool,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl SessionSupervisor {
    #[inline]
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

    async fn connect_pool(
        cfg: &ModbusChannelConfig,
        transport_meter: &Arc<dyn SouthwardTransportMeter>,
        connect_timeout_ms: u64,
    ) -> DriverResult<SessionPool> {
        match &cfg.connection {
            ModbusConnection::Tcp { host, port } => {
                let addr = format!("{}:{}", host, port)
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!("Invalid socket address: {}", e))
                    })?;
                // Pool size is only meaningful for TCP; clamp to [1, 8] for safety.
                let size = cfg.tcp_pool_size.clamp(1, 8) as usize;
                let mut contexts = Vec::with_capacity(size);
                for _ in 0..size {
                    let stream = connect_tcp_metered_with_timeout(
                        addr,
                        Arc::clone(transport_meter),
                        connect_timeout_ms,
                    )
                    .await
                    .map_err(|e| {
                        DriverError::SessionError(format!("Modbus TCP connect error: {e}"))
                    })?;
                    let ctx = tcp::attach(stream);
                    contexts.push(ctx);
                }
                Ok(SessionPool::new(contexts))
            }
            ModbusConnection::Rtu {
                port,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                let stream = connect_serial_metered(
                    SerialConnectConfig {
                        port: port.to_string(),
                        baud_rate: *baud_rate,
                        data_bits: (*data_bits).into(),
                        stop_bits: (*stop_bits).into(),
                        parity: (*parity).into(),
                    },
                    Arc::clone(transport_meter),
                )
                .map_err(|e| {
                    DriverError::SessionError(format!("Failed to open serial port {port}: {e}"))
                })?;
                Ok(SessionPool::new(vec![rtu::attach(stream)]))
            }
        }
    }

    /// Run supervisor loop: maintain a single healthy connection and reconnect on demand.
    /// This method spawns a background task and returns immediately.
    pub async fn run(&self, channel: Arc<ModbusChannel>) -> DriverResult<()> {
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
        let mut reconnect_rx =
            self.reconnect_rx
                .lock()
                .await
                .take()
                .ok_or(DriverError::ConfigurationError(
                    "reconnect receiver already consumed".into(),
                ))?;
        let transport_meter = Arc::clone(&self.transport_meter);

        // reset flags
        shared.shutdown.store(false, Ordering::Release);

        // IMPORTANT:
        // This task must inherit `channel_id` so dependency logs emitted inside reconnection,
        // transport setup, or protocol library internals can be filtered per channel on host side.
        let span = tracing::info_span!("modbus-supervisor", channel_id = channel.id);
        tokio::spawn(async move {
            let mut ever_connected = false;
            let mut retry = RetryController::new(&channel.connection_policy.backoff);
            loop {
                // Semantics:
                // - First connect: `Connecting`
                // - After ever connected: `Reconnecting` while retrying
                let _ = state_tx.send(if ever_connected {
                    SouthwardConnectionState::Reconnecting
                } else {
                    SouthwardConnectionState::Connecting
                });
                let pool: SessionPool = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                        return;
                    }
                    match Self::connect_pool(
                        &channel.config,
                        &transport_meter,
                        channel.connection_policy.connect_timeout_ms,
                    )
                    .await
                    {
                        Ok(pool) => break pool,
                        Err(e) => {
                            shared.healthy.store(false, Ordering::Release);
                            let msg = e.to_string();
                            let _ = shared.last_error.lock().map(|mut g| *g = Some(msg.clone()));
                            // failure immediately visible + reconnect process visible
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
                                        _ = sleep(delay) => {}
                                    }
                                    continue;
                                }
                                RetryDecision::Exhausted => return,
                            }
                        }
                    }
                };

                // Store pool and treat transport connect as `Connected` (user requested).
                let pool = Arc::new(pool);
                if let Some(old) = shared.pool.swap(Some(Arc::clone(&pool))) {
                    old.disconnect_all(StdDuration::from_secs(2)).await;
                }
                shared.healthy.store(true, Ordering::Release);
                ever_connected = true;
                retry.reset();
                let _ = state_tx.send(SouthwardConnectionState::Connected);

                // Drain any stale notifications to avoid immediate redundant reconnect.
                while reconnect_rx.try_recv().is_ok() {}

                // Wait for either external cancel or reconnect request
                tokio::select! {
                    _ = cancel.cancelled() => {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                        // Best-effort disconnect
                        if let Some(p) = shared.pool.swap(None) {
                            p.disconnect_all(StdDuration::from_secs(2)).await;
                        }
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        // Drop connection and restart loop
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                        if let Some(p) = shared.pool.swap(None) {
                            p.disconnect_all(StdDuration::from_secs(2)).await;
                        }
                        // continue outer loop
                    }
                }
            }
        }
        .instrument(span));
        Ok(())
    }
}
