use super::{
    protocol::{
        create, session::Session, Cause, CauseOfTransmission, Error, ObjectQCC, ObjectQOI,
        SessionConfig, SessionEvent, SessionEventLoop, SessionLifecycleState,
    },
    types::Iec104Channel,
};
use anyhow::anyhow;
use arc_swap::ArcSwapOption;
use backoff::{backoff::Backoff, ExponentialBackoff};
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
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// Startup action item describing which interrogation(s) to trigger when session becomes Active.
#[derive(Clone)]
pub enum StartupAction {
    /// Trigger Counter Interrogation with given QCC then ActivationTerm after a delay (ms)
    CounterInterrogation { ca: u16, qcc: u8 },
    /// Trigger General Interrogation with given QOI then ActivationTerm after a delay (ms)
    GeneralInterrogation { ca: u16, qoi: u8 },
}

/// Shared session entry for IEC104, aligned with OPC UA's lock-free access pattern
pub(super) struct SessionEntry {
    pub session: ArcSwapOption<Session>,
    pub healthy: AtomicBool,
    pub shutdown: AtomicBool,
    pub last_error: Mutex<Option<String>>,
    // metrics counters for visibility
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

    /// Subscribe to session changes. The returned receiver always contains
    /// the latest value (None if no active session). Consumers should call
    /// `changed().await` to await updates.
    pub async fn subscribe_session(&self) -> watch::Receiver<Option<Arc<Session>>> {
        self.session_watch_rx.lock().await.clone()
    }
}

pub(super) type SharedSession = Arc<SessionEntry>;

/// High-availability supervisor for a single IEC104 TCP session.
///
/// Responsibilities:
/// - Create and own the protocol Session/Loop using ClientBuilder
/// - Drive run_forever with backoff parameters to achieve auto-reconnect
/// - Expose cancellation and JoinHandle for clean shutdown
/// - Observe lifecycle and trigger configured startup actions when becoming Active
pub(super) struct Iec104Supervisor {
    cancel: CancellationToken,
    started: AtomicBool,
    /// Shared lock-free session state and health, for data-plane usage
    shared: SharedSession,
    /// Connection state watch sender maintained by supervisor
    state_tx: watch::Sender<SouthwardConnectionState>,
}

impl Iec104Supervisor {
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
    pub async fn run(
        &self,
        channel: Arc<Iec104Channel>,
        startup_actions: Vec<StartupAction>,
    ) -> DriverResult<()> {
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
        let socket_addr = format!("{}:{}", channel.config.host, channel.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| {
                DriverError::ConfigurationError(format!("Invalid socket address: {}", e))
            })?;

        let cancel_outer = self.cancel.child_token();
        let state_tx = self.state_tx.clone();
        let options = SessionConfig {
            connection_timeout_ms: channel.connection_policy.connect_timeout_ms,
            t0_ms: channel.config.t0_ms as u64,
            t1_ms: channel.config.t1_ms as u64,
            t2_ms: channel.config.t2_ms as u64,
            t3_ms: channel.config.t3_ms as u64,
            k_window: channel.config.k_window,
            w_threshold: channel.config.w_threshold,
            send_queue_capacity: channel.config.send_queue_capacity,
            tcp_nodelay: channel.config.tcp_nodelay,
            max_pending_asdu_bytes: channel.config.max_pending_asdu_bytes,
            discard_low_priority_when_window_full: channel
                .config
                .discard_low_priority_when_window_full,
            merge_low_priority: channel.config.merge_low_priority,
            low_prio_flush_max_age_ms: channel.config.low_prio_flush_max_age_ms,
        };

        tokio::spawn(async move {
            // initial connect
            let (mut session, mut ev) = create(socket_addr, options);
            // reconnect backoff state; reset on a successful active session
            let mut bo: ExponentialBackoff =
                build_exponential_backoff(&channel.connection_policy.backoff);
            let mut attempt: u32 = 0;
            loop {
                let child = cancel_outer.child_token();
                // run one attempt's event loop (spawns IO internally)
                let mut task = tokio::spawn(Self::run_event_loop(
                    shared.clone(),
                    session.clone(),
                    ev,
                    child.clone(),
                    startup_actions.clone(),
                    options.t1_ms,
                    1, // max retries per startup command
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
                                tracing::warn!(attempt = attempt, delay_ms = dur.as_millis() as u64, "IEC104 connect retry");
                                tokio::select! {
                                    _ = cancel_outer.cancelled() => { break; }
                                    _ = tokio::time::sleep(dur) => {
                                        let _ = state_tx.send(SouthwardConnectionState::Reconnecting);
                                    }
                                }
                            }
                            None => {
                                let _ = state_tx.send(SouthwardConnectionState::Failed("cancelled".to_string()));
                                break;
                            }
                        }
                        (session, ev) = create(socket_addr, options);
                    }
                }
            }
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Drive the IEC104 SessionEventLoop stream until cancelled or ends.
    /// Updates shared health and, on Active, triggers startup commands inline.
    async fn run_event_loop(
        shared: SharedSession,
        session: Arc<Session>,
        ev: SessionEventLoop,
        cancel: CancellationToken,
        startup_actions: Vec<StartupAction>,
        cmd_timeout_ms: u64,
        cmd_max_retries: u8,
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
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Active) => {
                                    shared.session.store(Some(session.clone()));
                                    shared.healthy.store(true, Ordering::Release);
                                    let _ = shared.session_watch_tx.send(Some(session.clone()));
                                    let _ = state_tx.send(SouthwardConnectionState::Connected);
                                    seen_active = true;
                                    if let Err(e) = Self::send_startup_on_active(&session, &startup_actions, cmd_timeout_ms, cmd_max_retries, &cancel).await {
                                        warn!(error=%e, "startup actions failed");
                                    }
                                }
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Closing) => {
                                    shared.session.store(None);
                                    shared.healthy.store(false, Ordering::Release);
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Disconnected);
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
                                SessionEvent::LifecycleChanged(SessionLifecycleState::Failed(reason)) => {
                                    shared.healthy.store(false, Ordering::Release);
                                    {
                                        let mut e = shared.last_error.lock().await;
                                        *e = Some(reason.clone());
                                    }
                                    let _ = shared.session_watch_tx.send(None);
                                    let _ = state_tx.send(SouthwardConnectionState::Failed(reason));
                                    return seen_active;
                                }
                                SessionEvent::T1Timeout => {
                                    shared.t1_timeouts.fetch_add(1, Ordering::Relaxed);
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("t1 timeout".to_string());
                                }
                                SessionEvent::WindowFullDrop => {
                                    shared.window_full_drops.fetch_add(1, Ordering::Relaxed);
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("window full: dropped frame".to_string());
                                }
                                SessionEvent::MemoryBudgetDrop => {
                                    shared.memory_budget_drops.fetch_add(1, Ordering::Relaxed);
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("memory budget exceeded: drop".to_string());
                                }
                                SessionEvent::FrameTooLargeDrop => {
                                    shared.frame_too_large_drops.fetch_add(1, Ordering::Relaxed);
                                    let mut e = shared.last_error.lock().await;
                                    *e = Some("frame too large: drop".to_string());
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

    /// Send startup commands on Active lifecycle.
    /// For each configured CA, send Counter Interrogation and General Interrogation.
    async fn send_startup_on_active(
        session: &Arc<Session>,
        actions: &[StartupAction],
        cmd_timeout_ms: u64,
        cmd_max_retries: u8,
        cancel: &CancellationToken,
    ) -> Result<(), Error> {
        // strictly sequential startup actions, in configured order; no inter-command delay, no parallelism across CAs
        for cmd in actions.iter().cloned() {
            if cancel.is_cancelled() {
                break;
            }
            let _ = Iec104Supervisor::send_startup_cmd(
                Arc::clone(session),
                cmd,
                cmd_timeout_ms,
                cmd_max_retries,
                cancel.child_token(),
            )
            .await;
        }
        Ok(())
    }

    async fn send_startup_cmd(
        session: Arc<Session>,
        cmd: StartupAction,
        cmd_timeout_ms: u64,
        cmd_max_retries: u8,
        cancel: CancellationToken,
    ) -> Result<(), Error> {
        let mut delay_ms: u64 = 10;
        let max_attempts: u32 = (cmd_max_retries as u32).saturating_add(1);
        let deadline = Duration::from_millis(cmd_timeout_ms);
        for attempt in 0..max_attempts {
            if cancel.is_cancelled() {
                break;
            }
            let session_clone = Arc::clone(&session);
            let cmd_clone = cmd.clone();
            let fut = async move {
                match cmd_clone {
                    StartupAction::CounterInterrogation { ca, qcc } => {
                        session_clone
                            .counter_interrogation_cmd(
                                CauseOfTransmission::new(false, false, Cause::Activation),
                                ca,
                                ObjectQCC::new(qcc),
                            )
                            .await
                    }
                    StartupAction::GeneralInterrogation { ca, qoi } => {
                        session_clone
                            .interrogation_cmd(
                                CauseOfTransmission::new(false, false, Cause::Activation),
                                ca,
                                ObjectQOI::new(qoi),
                            )
                            .await
                    }
                }
            };

            match tokio::time::timeout(deadline, fut).await {
                Ok(Ok(())) => {
                    return Ok(());
                }
                Ok(Err(e)) => {
                    warn!(error=%e, "startup command failed");
                }
                Err(_) => {
                    warn!("startup command timed out after {} ms", cmd_timeout_ms);
                }
            }

            if attempt + 1 < max_attempts {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                delay_ms = (delay_ms.saturating_mul(2)).min(200);
            }
        }
        Err(Error::ErrAnyHow(anyhow!(
            "startup command exhausted retries"
        )))
    }
}
