use super::types::ModbusConnection;
use crate::types::{ModbusChannel, ModbusChannelConfig};
use arc_swap::ArcSwapOption;
use backoff::{backoff::Backoff, ExponentialBackoff};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, InstrumentedTransportFactory,
    MeteredStream, SouthwardConnectionState, SouthwardTransportMeter,
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
use tokio_serial::SerialPortBuilderExt;
use tokio_util::sync::CancellationToken;

/// Transport observability wiring needed by the Modbus supervisor.
///
/// This is moved into the supervisor at `start()` time to avoid any cloning at call sites.
#[derive(Debug)]
pub(super) struct ModbusObservability {
    pub channel_id: i32,
    pub driver: Arc<str>,
    pub meter: Arc<dyn SouthwardTransportMeter>,
    pub transport: Arc<dyn InstrumentedTransportFactory>,
}

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
    /// Health flag indicating the current connectivity
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
    pub reconnect_rx: mpsc::Receiver<()>,
}

impl SessionSupervisor {
    #[inline]
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

    async fn connect_pool(
        cfg: &ModbusChannelConfig,
        obs: &ModbusObservability,
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
                    let stream = obs
                        .transport
                        .connect_tcp(
                            obs.channel_id,
                            Arc::clone(&obs.driver),
                            None,
                            addr,
                            Arc::clone(&obs.meter),
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
                let builder = tokio_serial::new(port, *baud_rate)
                    .data_bits((*data_bits).into())
                    .stop_bits((*stop_bits).into())
                    .parity((*parity).into());
                match builder.open_native_async() {
                    Ok(stream) => {
                        let metered = MeteredStream::new(
                            stream,
                            Arc::clone(&obs.meter),
                            obs.channel_id,
                            Arc::clone(&obs.driver),
                            None,
                        );
                        Ok(SessionPool::new(vec![rtu::attach(metered)]))
                    }
                    Err(e) => Err(DriverError::SessionError(format!(
                        "Failed to open serial port {port}: {e}"
                    ))),
                }
            }
        }
    }

    /// Run supervisor loop: maintain a single healthy connection and reconnect on demand.
    /// This method spawns a background task and returns immediately.
    pub async fn run(self, channel: Arc<ModbusChannel>, obs: ModbusObservability) {
        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();
        let mut reconnect_rx = self.reconnect_rx;
        let obs = Arc::new(obs);

        // reset flags
        shared.shutdown.store(false, Ordering::Release);

        tokio::spawn(async move {
            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);
                // Connect with exponential backoff
                let mut bo: ExponentialBackoff =
                    build_exponential_backoff(&channel.connection_policy.backoff);
                let mut attempt: u32 = 0;
                let pool: SessionPool = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx
                            .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        return;
                    }
                    match Self::connect_pool(&channel.config, obs.as_ref()).await {
                        Ok(pool) => break pool,
                        Err(e) => {
                            shared.healthy.store(false, Ordering::Relaxed);
                            let _ = state_tx.send(SouthwardConnectionState::Failed(e.to_string()));
                            attempt = attempt.saturating_add(1);
                            let delay = bo.next_backoff().unwrap_or_else(|| {
                                StdDuration::from_millis(
                                    channel.connection_policy.backoff.max_interval_ms,
                                )
                            });
                            tracing::warn!(attempt = attempt, delay_ms = delay.as_millis() as u64, error = %e, "Modbus connect retry");
                            let _ = shared
                                .last_error
                                .lock()
                                .map(|mut g| *g = Some(e.to_string()));
                            tokio::select! {
                                _ = cancel.cancelled() => {
                                    shared.shutdown.store(true, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                                    return;
                                }
                                _ = sleep(delay) => {}
                            }
                        }
                    }
                };

                // Store pool and mark healthy
                let pool = Arc::new(pool);
                if let Some(old) = shared.pool.swap(Some(Arc::clone(&pool))) {
                    old.disconnect_all(StdDuration::from_secs(2)).await;
                }
                shared.healthy.store(true, Ordering::Release);
                let _ = state_tx.send(SouthwardConnectionState::Connected);

                // Drain any stale notifications to avoid immediate redundant reconnect.
                while reconnect_rx.try_recv().is_ok() {}

                // Wait for either external cancel or reconnect request
                tokio::select! {
                    _ = cancel.cancelled() => {
                        shared.shutdown.store(true, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
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
        });
    }
}
