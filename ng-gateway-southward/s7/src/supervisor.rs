use super::{
    protocol::{
        frame::tsap::default_tsap_pair,
        session::{
            create, Session, SessionConfig, SessionEvent, SessionEventLoop, SessionLifecycleState,
        },
    },
    types::{S7Channel, TsapConfig},
};
use arc_swap::ArcSwapOption;
use backoff::backoff::Backoff;
use futures::{pin_mut, StreamExt};
use ng_gateway_sdk::{
    build_exponential_backoff, DriverError, DriverResult, SouthwardConnectionState,
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
    time::{sleep, Duration},
};
use tokio_util::sync::CancellationToken;

#[allow(unused)]
/// Shared session entry for S7
pub(super) struct SessionEntry {
    /// Current active session if any
    pub session: ArcSwapOption<Session>,
    /// Health indicator for fast-path checks
    pub healthy: AtomicBool,
    /// Shutdown flag set when supervisor stops
    pub shutdown: AtomicBool,
    /// Last error message for observability
    pub last_error: Mutex<Option<String>>,
    // metrics counters for visibility (kept aligned with IEC104 fields)
    pub window_full_drops: AtomicU64,
    pub memory_budget_drops: AtomicU64,
    pub frame_too_large_drops: AtomicU64,
    pub t1_timeouts: AtomicU64,
    /// Watch channel to notify consumers when the active session changes
    pub session_watch_tx: watch::Sender<Option<Arc<Session>>>,
    /// Primary receiver used to clone additional receivers for subscribers
    pub session_watch_rx: Mutex<watch::Receiver<Option<Arc<Session>>>>,
}

impl SessionEntry {
    #[inline]
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
    /// Subscribe to session changes. The returned receiver always contains
    /// the latest value (None if no active session). Consumers should call
    /// `changed().await` to await updates.
    pub async fn subscribe_session(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.session_watch_rx.lock().await.clone()
    }
}

pub(super) type SharedSession = Arc<SessionEntry>;

/// S7 high-availability supervisor that owns the protocol session lifecycle
/// and performs reconnect with exponential backoff. Design aligned with IEC104 supervisor.
///
pub(super) struct S7Supervisor {
    cancel: CancellationToken,
    started: AtomicBool,
    /// Shared lock-free session state and health, for data-plane usage
    shared: SharedSession,
    /// Connection state watch sender maintained by supervisor
    state_tx: watch::Sender<SouthwardConnectionState>,
}

impl S7Supervisor {
    /// Create a new supervisor with provided shared session and options
    pub fn new(
        shared: SharedSession,
        cancel: CancellationToken,
        state_tx: watch::Sender<SouthwardConnectionState>,
    ) -> Self {
        Self {
            cancel,
            shared,
            started: AtomicBool::new(false),
            state_tx,
        }
    }

    /// Start the underlying session loop with reconnect semantics.
    /// Safe to call multiple times; subsequent calls are no-ops.
    pub async fn run(&self, channel: Arc<S7Channel>) -> DriverResult<()> {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        let shared = Arc::clone(&self.shared);
        // Reset shutdown flag at the beginning of a supervisor lifecycle
        shared.shutdown.store(false, Ordering::Release);

        let (src, dst) = match channel.config.tsap {
            TsapConfig::RackSlot { rack, slot } => {
                let pair = default_tsap_pair(channel.config.cpu, rack, slot)
                    .map_err(|e| DriverError::ConfigurationError(format!("Invalid TSAP: {}", e)))?;
                (pair.local.into(), pair.remote.into())
            }
            TsapConfig::Tsap { src, dst } => (src, dst),
        };

        // Map driver config to protocol session options
        let socket_addr = format!("{}:{}", channel.config.host, channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| {
                DriverError::ConfigurationError(format!("Invalid socket address: {}", e))
            })?;
        let options = Arc::new(SessionConfig {
            // use defaults for CPU/rack/slot unless provided via TSAP overrides
            socket_addr,
            cpu: channel.config.cpu,
            tsap_src: src,
            tsap_dst: dst,
            preferred_pdu_size: channel.config.preferred_pdu_size,
            preferred_amq_caller: channel.config.preferred_amq_caller,
            preferred_amq_callee: channel.config.preferred_amq_callee,
            connect_timeout: Duration::from_millis(channel.connection_policy.connect_timeout_ms),
            read_timeout: Duration::from_millis(channel.connection_policy.read_timeout_ms),
            write_timeout: Duration::from_millis(channel.connection_policy.write_timeout_ms),
            ..Default::default()
        });

        let cancel_outer = self.cancel.child_token();
        let state_tx = self.state_tx.clone();

        tokio::spawn(async move {
            // initial connect
            let (mut session, mut ev) = create(Arc::clone(&options));
            // reconnect backoff state; reset on a successful active session
            let mut bo = build_exponential_backoff(&channel.connection_policy.backoff);
            let mut attempt: u32 = 0;
            loop {
                let child = cancel_outer.child_token();
                // proactively report Connecting for this attempt
                // run one attempt's event loop (spawns IO internally)
                let mut task = tokio::spawn(Self::run_event_loop(
                    shared.clone(),
                    session.clone(),
                    ev,
                    child.clone(),
                    state_tx.clone(),
                ));
                tokio::select! {
                    _ = cancel_outer.cancelled() => {
                        child.cancel();
                        let _ = task.await;
                        shared.session.store(None);
                        shared.healthy.store(false, Ordering::Release);
                        shared.shutdown.store(true, Ordering::Release);
                        let _ = shared.session_watch_tx.send(None);
                        let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                        {
                            let mut e = shared.last_error.lock().await;
                            *e = Some("supervisor cancelled".to_string());
                        }
                        break;
                    }
                    res = &mut task => {
                        // attempt finished, clear state then backoff and retry
                        shared.session.store(None);
                        shared.healthy.store(false, Ordering::Release);
                        let _ = shared.session_watch_tx.send(None);
                        if res.unwrap_or_default() {
                            bo.reset();
                            attempt = 0;
                        }

                        match bo.next_backoff() {
                            Some(dur) => {
                                attempt = attempt.saturating_add(1);
                                tracing::warn!(attempt = attempt, delay_ms = dur.as_millis() as u64, "S7 connect retry");
                                tokio::select! {
                                    _ = cancel_outer.cancelled() => { break; }
                                    _ = sleep(dur) => {
                                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                    }
                                }
                            }
                            None => {
                                let _ = state_tx.send(SouthwardConnectionState::Failed("backoff exhausted".to_string()));
                                break;
                            }
                        }
                        (session, ev) = create(Arc::clone(&options));
                    }
                }
            }
        });
        Ok(())
    }

    /// Drive the S7 SessionEventLoop stream until cancelled or ends.
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
                    let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                    return seen_active;
                }
                maybe_item = stream.next() => {
                    match maybe_item {
                        Some(ev) => {
                            match ev {
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Connecting) => {
                                    shared.healthy.store(false, Ordering::Release);
                                    let _ = state_tx.send(SouthwardConnectionState::Connecting);
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Handshaking) => {
                                    shared.healthy.store(false, Ordering::Release);
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Active) => {
                                    shared.session.store(Some(session.clone()));
                                    shared.healthy.store(true, Ordering::Release);
                                    let _ = shared.session_watch_tx.send(Some(session.clone()));
                                    let _ = state_tx.send(SouthwardConnectionState::Connected);
                                    seen_active = true;
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Closing) => {
                                    shared.session.store(None);
                                    shared.healthy.store(false, Ordering::Release);
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Closed) => {
                                    shared.healthy.store(false, Ordering::Release);
                                    {
                                        let mut e = shared.last_error.lock().await;
                                        *e = Some("session closed".to_string());
                                    }
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
                                    return seen_active;
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Failed) => {
                                    shared.healthy.store(false, Ordering::Release);
                                    {
                                        let mut e = shared.last_error.lock().await;
                                        *e = Some("session failed".to_string());
                                    }
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Failed("event loop failed".to_string()));
                                    return seen_active;
                                }
                                SessionEvent::TransportError => {
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("transport error".to_string());
                                }
                                SessionEvent::FragmentReassemblyDrop => {
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("fragment reassembly drop".to_string());
                                }
                                _ => {}
                            }
                        }
                        None => { return seen_active; }
                    }
                }
            }
        }
    }
}
