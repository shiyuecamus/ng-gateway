use super::{
    protocol::session::{
        create_with_stream, Session, SessionConfig, SessionEvent, SessionEventLoop,
        SessionLifecycleState,
    },
    types::McChannel,
};
use arc_swap::ArcSwapOption;
use futures::{pin_mut, StreamExt};
use ng_gateway_sdk::{
    connect_tcp_metered_with_timeout, DriverError, DriverResult, RetryController, RetryDecision,
    SouthwardConnectionState, SouthwardTransportMeter,
};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{
    sync::{watch, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[allow(unused)]
/// Shared MC session entry with lock-free handle access and health flags.
pub(super) struct SessionEntry {
    /// Current active protocol session if any.
    pub session: ArcSwapOption<Session>,
    /// Health indicator for fast-path checks.
    pub healthy: AtomicBool,
    /// Shutdown flag set when supervisor stops.
    pub shutdown: AtomicBool,
    /// Last error message for observability.
    pub last_error: Mutex<Option<String>>,
    /// Metrics counters for visibility (aligned with IEC104 fields).
    pub window_full_drops: AtomicU64,
    pub memory_budget_drops: AtomicU64,
    pub frame_too_large_drops: AtomicU64,
    pub t1_timeouts: AtomicU64,
    /// Watch channel to notify consumers when the active session changes.
    pub session_watch_tx: watch::Sender<Option<Arc<Session>>>,
    /// Primary receiver used to clone additional receivers for subscribers.
    pub session_watch_rx: Mutex<watch::Receiver<Option<Arc<Session>>>>,
}

impl SessionEntry {
    /// Create an empty shared entry with no active IO handle.
    pub fn new_empty() -> Self {
        let (tx, rx) = watch::channel::<Option<Arc<Session>>>(None);
        Self {
            session: ArcSwapOption::from(None),
            healthy: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            last_error: Mutex::new(None),
            window_full_drops: AtomicU64::new(0),
            memory_budget_drops: AtomicU64::new(0),
            frame_too_large_drops: AtomicU64::new(0),
            t1_timeouts: AtomicU64::new(0),
            session_watch_tx: tx,
            session_watch_rx: Mutex::new(rx),
        }
    }

    #[allow(unused)]
    /// Subscribe to session changes. Returned receiver always contains latest value.
    pub async fn subscribe_session(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.session_watch_rx.lock().await.clone()
    }
}

/// Shared pointer type for session entry.
pub(super) type SharedSession = Arc<SessionEntry>;

/// High-availability supervisor for a single MC TCP/UDP connection.
///
/// Responsibilities:
/// - Establish and own a single MC transport (TCP/UDP) based on channel config
/// - Perform exponential backoff reconnect on connection failures
/// - Maintain `SharedSession` health flags and IO handle
/// - Broadcast `SouthwardConnectionState` updates for observability
pub(super) struct McSupervisor {
    cancel: CancellationToken,
    started: AtomicBool,
    /// Shared lock-free IO state and health, for data-plane usage.
    pub(super) shared: SharedSession,
    /// Connection state watch sender maintained by supervisor.
    state_tx: watch::Sender<SouthwardConnectionState>,
    transport_meter: Arc<dyn SouthwardTransportMeter>,
}

impl McSupervisor {
    /// Create a new supervisor with provided shared session and options.
    pub fn new(
        shared: SharedSession,
        cancel: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
        transport_meter: Arc<dyn SouthwardTransportMeter>,
    ) -> Self {
        Self {
            cancel,
            shared,
            started: AtomicBool::new(false),
            state_tx,
            transport_meter,
        }
    }

    /// Start the supervisor outer loop with reconnect semantics.
    /// Safe to call multiple times; subsequent calls are no-ops.
    pub async fn run(&self, channel: Arc<McChannel>) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let shared = Arc::clone(&self.shared);
        // Reset shutdown flag at the beginning of a supervisor lifecycle.
        shared.shutdown.store(false, Ordering::Release);

        // Map driver config to protocol session options
        let socket_addr = format!("{}:{}", channel.config.host, channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| {
                DriverError::ConfigurationError(format!("Invalid socket address: {}", e))
            })?;

        let options = Arc::new(SessionConfig {
            socket_addr,
            series: channel.config.series,
            frame_variant: channel.config.series.frame_variant(),
            connect_timeout: Duration::from_millis(channel.connection_policy.connect_timeout_ms),
            read_timeout: Duration::from_millis(channel.connection_policy.read_timeout_ms),
            write_timeout: Duration::from_millis(channel.connection_policy.write_timeout_ms),
            send_queue_capacity: 256,
            max_concurrent_requests: channel.config.concurrent_requests.unwrap_or(1).max(1)
                as usize,
            tcp_nodelay: true,
        });

        let cancel_outer = self.cancel.child_token();
        let state_tx = self.state_tx.clone();
        let transport_meter = Arc::clone(&self.transport_meter);

        tokio::spawn(async move {
            let mut retry = RetryController::new(&channel.connection_policy.backoff);

            loop {
                if cancel_outer.is_cancelled() {
                    shared.shutdown.store(true, Ordering::Release);
                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                    let mut last = shared.last_error.lock().await;
                    *last = Some("supervisor cancelled".to_string());
                    return;
                }

                let _ = state_tx.send(SouthwardConnectionState::Connecting);

                let stream = match connect_tcp_metered_with_timeout(
                    options.socket_addr,
                    Arc::clone(&transport_meter),
                    channel.connection_policy.connect_timeout_ms,
                )
                .await
                {
                    Ok(s) => s,
                    Err(e) => {
                        let msg = format!("mc tcp connect failed: {e}");
                        {
                            let mut last = shared.last_error.lock().await;
                            *last = Some(msg.clone());
                        }
                        shared.healthy.store(false, Ordering::Relaxed);
                        // failure immediately visible + reconnect process visible
                        let _ = state_tx.send(SouthwardConnectionState::Failed(msg.clone()));
                        match retry.on_failure() {
                            RetryDecision::RetryAfter(delay) => {
                                let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                tokio::select! {
                                    _ = cancel_outer.cancelled() => return,
                                    _ = tokio::time::sleep(delay) => {}
                                }
                                continue;
                            }
                            RetryDecision::Exhausted => return,
                        }
                    }
                };
                let _ = stream.inner_ref().set_nodelay(options.tcp_nodelay);

                let (session, ev) = create_with_stream(Arc::clone(&options), stream);
                shared.session.store(Some(session.clone()));
                let _ = shared.session_watch_tx.send(Some(session.clone()));

                let child = cancel_outer.child_token();
                let seen_active = Self::run_event_loop(
                    Arc::clone(&shared),
                    Arc::clone(&session),
                    ev,
                    child,
                    state_tx.clone(),
                )
                .await;

                shared.session.store(None);
                shared.healthy.store(false, Ordering::Release);
                let _ = shared.session_watch_tx.send(None);

                if seen_active {
                    retry.reset();
                }

                match retry.on_failure() {
                    RetryDecision::RetryAfter(delay) => {
                        tracing::warn!(
                            delay_ms = delay.as_millis() as u64,
                            "MC session reconnect retry"
                        );
                        tokio::select! {
                            _ = cancel_outer.cancelled() => return,
                            _ = tokio::time::sleep(delay) => {
                                let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                            }
                        }
                    }
                    RetryDecision::Exhausted => {
                        let mut last = shared.last_error.lock().await;
                        *last = Some("retry budget exhausted".to_string());
                        let _ = state_tx.send(SouthwardConnectionState::Failed(
                            "retry budget exhausted".to_string(),
                        ));
                        return;
                    }
                }
            }
        });

        Ok(())
    }

    /// Drive the MC SessionEventLoop stream until cancelled or ends.
    /// Updates shared health and broadcasts connection state.
    async fn run_event_loop(
        shared: SharedSession,
        session: Arc<Session>,
        ev: SessionEventLoop,
        cancel: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
    ) -> bool {
        let stream = ev.enter();
        pin_mut!(stream);
        let mut seen_active = false;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    shared.healthy.store(false, Ordering::Release);
                    let _ = shared.session_watch_tx.send(None);
                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                    return seen_active;
                }
                maybe_ev = stream.next() => {
                    match maybe_ev {
                        Some(SessionEvent::LifecycleChanged(state)) => {
                            match state {
                                SessionLifecycleState::Connecting => {
                                    shared.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Connecting);
                                }
                                SessionLifecycleState::Active => {
                                    shared.session.store(Some(Arc::clone(&session)));
                                    shared.healthy.store(true, Ordering::Release);
                                    let _ = shared.session_watch_tx.send(Some(Arc::clone(&session)));
                                    let _ = state_tx.send(SouthwardConnectionState::Connected);
                                    seen_active = true;
                                }
                                SessionLifecycleState::Closing => {
                                    shared.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                }
                                SessionLifecycleState::Closed => {
                                    shared.healthy.store(false, Ordering::Release);
                                    {
                                        let mut last = shared.last_error.lock().await;
                                        *last = Some("session closed".to_string());
                                    }
                                    shared.session.store(None);
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                    return seen_active;
                                }
                                SessionLifecycleState::Failed => {
                                    shared.healthy.store(false, Ordering::Release);
                                    {
                                        let mut last = shared.last_error.lock().await;
                                        *last = Some("session failed".to_string());
                                    }
                                    shared.session.store(None);
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                    return seen_active;
                                }
                                SessionLifecycleState::Idle => {}
                            }
                        }
                        Some(SessionEvent::TransportError) => {
                            shared.healthy.store(false, Ordering::Release);
                            {
                                let mut last = shared.last_error.lock().await;
                                *last = Some("transport error".to_string());
                            }
                            // Treat transport error as immediate reconnect trigger.
                            let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                            return seen_active;
                        }
                        None => {
                            // Event stream ended.
                            shared.healthy.store(false, Ordering::Release);
                            shared.session.store(None);
                            let _ = shared.session_watch_tx.send(None);
                            let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                            return seen_active;
                        }
                    }
                }
            }
        }
    }
}
