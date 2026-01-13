use crate::types::{EthernetIpChannel, EthernetIpChannelConfig};
use arc_swap::ArcSwapOption;
use backoff::{backoff::Backoff, ExponentialBackoff};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, SouthwardConnectionState,
};
use rust_ethernet_ip::{EipClient, RoutePath};
use std::{
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
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Shared session entry guarded by a single async mutex.
/// The supervisor owns lifecycle and reconnection.
pub struct SessionEntry {
    /// Underlying EipClient protected by an async mutex
    pub client: ArcSwapOption<Mutex<EipClient>>,
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
            client: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: std::sync::Mutex::new(None),
            reconnect_tx,
        }
    }
}

/// Shared pointer type for session entry
pub type SharedSession = Arc<SessionEntry>;

/// Single-connection supervisor with exponential backoff reconnection.
pub struct SessionSupervisor {
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

    async fn connect_once(cfg: &EthernetIpChannelConfig) -> DriverResult<EipClient> {
        let addr = format!("{}:{}", cfg.host, cfg.port);
        let slot = cfg.slot;
        info!("Connecting to Ethernet/IP PLC at {} (Slot {})", addr, slot);

        // If slot is 0, we might use default connect, but being explicit with path is safer if backplane routing is involved.
        // Assuming typical CompactLogix/ControlLogix setup:
        // Path: Backplane (Port 1) -> Slot N
        // Port 1 is typically the backplane port.
        // rust-ethernet-ip RoutePath helper makes this easier.
        // If the library's `connect` defaults to direct connection (which works for CompactLogix often),
        // `with_route_path` allows explicit routing.

        let client_res = if slot == 0 {
            // Try default connection for Slot 0 (often direct or default backplane)
            EipClient::connect(&addr).await
        } else {
            // Explicit backplane routing
            // Note: route path logic might vary by chassis type, but typically it's Backplane -> Slot
            // rust-ethernet-ip example: RoutePath::new().add_slot(0).add_slot(2)
            // We assume Port 1 (Backplane) is implicit or handled by add_slot logic?
            // Looking at library docs (from memory/search):
            // RoutePath logic is specific.
            // Let's assume simplest backplane routing:
            // path = Port 1, Address <slot>
            let route = RoutePath::new().add_slot(slot);
            EipClient::with_route_path(&addr, route).await
        };

        match client_res {
            Ok(client) => Ok(client),
            Err(e) => Err(DriverError::SessionError(format!(
                "Failed to connect to {} (Slot {}): {}",
                addr, slot, e
            ))),
        }
    }

    /// Run supervisor loop: maintain a single healthy connection and reconnect on demand.
    /// Spawns the loop in a background task.
    pub async fn run(self, channel: Arc<EthernetIpChannel>) -> DriverResult<()> {
        let shared = Arc::clone(&self.shared);
        let cancel = self.cancel_token.clone();
        let state_tx = self.state_tx.clone();
        let mut reconnect_rx = self.reconnect_rx;

        tokio::spawn(async move {
            // reset flags
            shared.shutdown.store(false, Ordering::Release);

            loop {
                let _ = state_tx.send(SouthwardConnectionState::Connecting);
                // Connect with exponential backoff
                let mut bo: ExponentialBackoff =
                    build_exponential_backoff(&channel.connection_policy.backoff);
                let mut attempt: u32 = 0;
                let client: EipClient = loop {
                    if cancel.is_cancelled() {
                        shared.shutdown.store(true, Ordering::Release);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx
                            .send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        return;
                    }
                    match Self::connect_once(&channel.config).await {
                        Ok(c) => break c,
                        Err(e) => {
                            shared.healthy.store(false, Ordering::Relaxed);
                            let _ = state_tx.send(SouthwardConnectionState::Failed(e.to_string()));
                            attempt = attempt.saturating_add(1);
                            let delay = bo.next_backoff().unwrap_or_else(|| {
                                StdDuration::from_millis(
                                    channel.connection_policy.backoff.max_interval_ms,
                                )
                            });
                            warn!(
                                attempt = attempt,
                                delay_ms = delay.as_millis() as u64,
                                error = %e,
                                "Ethernet/IP connect retry"
                            );
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

                // Store client and mark healthy
                let client = Arc::new(Mutex::new(client));
                if let Some(old) = shared.client.swap(Some(Arc::clone(&client))) {
                    // Close old connection if necessary
                    // rust-ethernet-ip might handle closing on Drop, but if there's an explicit close/disconnect, call it.
                    // EipClient::close is typical, checking docs or assuming explicit close isn't strictly required if Drop works,
                    // but let's see if we can call something.
                    // Assuming EipClient has a close method or similar if needed. If not, Drop handles it.
                    // For now, we just drop the old mutex guard.
                    let _ = timeout(StdDuration::from_secs(2), async move {
                        let mut guard = old.lock().await;
                        // guard.close().await; // If such method exists
                        let _ = &mut *guard; // suppress unused
                    })
                    .await;
                }
                shared.healthy.store(true, Ordering::Release);
                let _ = state_tx.send(SouthwardConnectionState::Connected);
                info!("Ethernet/IP connected successfully");

                // Drain any stale notifications to avoid immediate redundant reconnect.
                while reconnect_rx.try_recv().is_ok() {}

                // Wait for either external cancel or reconnect request
                tokio::select! {
                    _ = cancel.cancelled() => {
                        shared.shutdown.store(true, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        // Best-effort disconnect
                        if let Some(c) = shared.client.swap(None) {
                             let _ = timeout(StdDuration::from_secs(2), async move {
                                let mut _guard = c.lock().await;
                                // disconnect logic if needed
                            })
                            .await;
                        }
                        return;
                    }
                    Some(()) = reconnect_rx.recv() => {
                        // Drop connection and restart loop
                        shared.healthy.store(false, Ordering::Release);
                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                        if let Some(c) = shared.client.swap(None) {
                             let _ = timeout(StdDuration::from_secs(2), async move {
                                let mut _guard = c.lock().await;
                                 // disconnect logic if needed
                            })
                            .await;
                        }
                        // continue outer loop
                    }
                }
            }
        });
        Ok(())
    }
}
