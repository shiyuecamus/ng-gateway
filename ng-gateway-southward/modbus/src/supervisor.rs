use super::types::ModbusConnection;
use crate::types::{ModbusChannel, ModbusChannelConfig};
use arc_swap::ArcSwapOption;
use backoff::{backoff::Backoff, ExponentialBackoff};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, SouthwardConnectionState,
};
use std::{
    net::SocketAddr,
    sync::{
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

/// Shared Modbus session entry guarded by a single async mutex.
/// The supervisor owns lifecycle and reconnection.
pub(super) struct SessionEntry {
    /// Underlying Modbus context protected by an async mutex
    pub ctx: ArcSwapOption<Mutex<Context>>,
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
            ctx: ArcSwapOption::from(None),
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

    async fn connect_once(cfg: &ModbusChannelConfig) -> DriverResult<Context> {
        match &cfg.connection {
            ModbusConnection::Tcp { host, port } => {
                let addr = format!("{}:{}", host, port)
                    .parse::<SocketAddr>()
                    .map_err(|e| {
                        DriverError::ConfigurationError(format!("Invalid socket address: {}", e))
                    })?;
                tcp::connect(addr).await.map_err(|e| {
                    DriverError::SessionError(format!("Modbus TCP connect error: {e}"))
                })
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
                    Ok(stream) => Ok(rtu::attach(stream)),
                    Err(e) => Err(DriverError::SessionError(format!(
                        "Failed to open serial port {port}: {e}"
                    ))),
                }
            }
        }
    }

    /// Run supervisor loop: maintain a single healthy connection and reconnect on demand.
    /// This method spawns a background task and returns immediately.
    pub async fn run(self, channel: Arc<ModbusChannel>) {
        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();
        let mut reconnect_rx = self.reconnect_rx;

        // reset flags
        shared.shutdown.store(false, Ordering::Release);

        tokio::spawn(async move {
            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);
                // Connect with exponential backoff
                let mut bo: ExponentialBackoff =
                    build_exponential_backoff(&channel.connection_policy.backoff);
                let mut attempt: u32 = 0;
                let ctx: Context = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx
                            .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        return;
                    }
                    match Self::connect_once(&channel.config).await {
                        Ok(ctx) => break ctx,
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

                // Store context and mark healthy
                let ctx = Arc::new(Mutex::new(ctx));
                if let Some(old) = shared.ctx.swap(Some(Arc::clone(&ctx))) {
                    let _ = timeout(StdDuration::from_secs(2), async move {
                        let mut old_guard = old.lock().await;
                        old_guard.disconnect().await
                    })
                    .await;
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
                        if let Some(c) = shared.ctx.swap(None) {
                            let _ = timeout(StdDuration::from_secs(2), async move {
                                let mut cg = c.lock().await;
                                cg.disconnect().await
                            })
                            .await;
                        }
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        // Drop connection and restart loop
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                        if let Some(c) = shared.ctx.swap(None) {
                            let _ = timeout(StdDuration::from_secs(2), async move {
                                let mut cg = c.lock().await;
                                cg.disconnect().await
                            })
                            .await;
                        }
                        // continue outer loop
                    }
                }
            }
        });
    }
}
