pub mod state;
pub use state::{SessionConfig, SessionEvent, SessionLifecycleState, SessionState};

use super::{
    codec::Codec,
    frame::{
        apci::{
            iframe_wire_size_for_asdu, new_iframe, new_sframe, new_uframe, ApciKind, SApci, UApci,
            APDU_SIZE_MAX, U_STARTDT_ACTIVE, U_STARTDT_CONFIRM, U_STOPDT_ACTIVE, U_STOPDT_CONFIRM,
            U_TESTFR_ACTIVE, U_TESTFR_CONFIRM,
        },
        asdu::{Asdu, CauseOfTransmission, CommonAddr, TypeID},
        cproc::{
            bits_string32_cmd, double_cmd, set_point_cmd_float, set_point_cmd_normal,
            set_point_cmd_scaled, single_cmd, step_cmd, BitsString32CommandInfo, DoubleCommandInfo,
            SetPointCommandFloatInfo, SetPointCommandNormalInfo, SetPointCommandScaledInfo,
            SingleCommandInfo, StepCommandInfo,
        },
        csys::{counter_interrogation_cmd, interrogation_cmd, ObjectQCC, ObjectQOI},
    },
    Error,
};
use arc_swap::ArcSwapOption;
use chrono::{DateTime, Utc};
use futures::{pin_mut, Stream, StreamExt};
use futures_util::SinkExt;
use std::{collections::VecDeque, time::Duration};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};
use tokio::{
    net::TcpStream,
    select,
    sync::{broadcast, mpsc, watch, Mutex},
    task::JoinHandle,
    time::{interval, timeout},
};
use tokio_util::{
    codec::Decoder,
    codec::Encoder,
    sync::{CancellationToken as _CancellationTokenAlias, CancellationToken as _},
};
use tokio_util::{codec::Framed, sync::CancellationToken};

#[derive(Debug, Clone)]
pub enum Priority {
    High,
    Low,
}

#[derive(Debug, Clone)]
pub enum Request {
    I(Asdu, Priority),
    U(UApci),
    S(SApci),
}

pub struct Session {
    sender: Arc<ArcSwapOption<mpsc::Sender<Request>>>,
    events_tx: broadcast::Sender<SessionEvent>,
    asdu_tx: mpsc::Sender<Asdu>,
    asdu_rx: Mutex<Option<mpsc::Receiver<Asdu>>>,
    lifecycle_tx: watch::Sender<SessionLifecycleState>,
    lifecycle_rx: watch::Receiver<SessionLifecycleState>,
}

impl Session {
    pub fn data_sender(&self) -> Arc<ArcSwapOption<mpsc::Sender<Request>>> {
        Arc::clone(&self.sender)
    }
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events_tx.subscribe()
    }

    pub async fn take_asdu_receiver(&self) -> Option<mpsc::Receiver<Asdu>> {
        let mut guard = self.asdu_rx.lock().await;
        guard.take()
    }

    pub fn lifecycle(&self) -> watch::Receiver<SessionLifecycleState> {
        self.lifecycle_rx.clone()
    }

    pub fn current_lifecycle(&self) -> SessionLifecycleState {
        self.lifecycle_rx.borrow().clone()
    }

    pub async fn is_connected(&self) -> bool {
        if let Some(sender) = self.sender.load().as_ref() {
            return !sender.is_closed();
        }
        false
    }

    pub async fn is_active(&self) -> bool {
        self.is_connected().await
            && matches!(
                self.lifecycle_rx.borrow().clone(),
                SessionLifecycleState::Active
            )
    }

    /// Wait until lifecycle reaches the expected state. Returns false if the
    /// lifecycle channel is closed before reaching it.
    pub async fn wait_for_state(&self, expected: SessionLifecycleState) -> bool {
        if *self.lifecycle_rx.borrow() == expected {
            return true;
        }
        let mut rx = self.lifecycle();
        let ok = rx.wait_for(|s| *s == expected).await.is_ok();
        ok
    }

    /// Wait until the transport is connected (i.e., sender is available) and
    /// the session has entered at least Handshaking/Inactive/Active.
    /// Returns false if the lifecycle goes to Closed/Failed before that.
    pub async fn wait_for_connection(&self) -> bool {
        if self.is_connected().await {
            return true;
        }
        let mut rx = self.lifecycle();
        rx.wait_for(|s| {
            matches!(
                *s,
                SessionLifecycleState::Handshaking
                    | SessionLifecycleState::Inactive
                    | SessionLifecycleState::Active
                    | SessionLifecycleState::Closed
                    | SessionLifecycleState::Failed(_)
            )
        })
        .await
        .map(|s| match *s {
            SessionLifecycleState::Handshaking
            | SessionLifecycleState::Inactive
            | SessionLifecycleState::Active => true,
            SessionLifecycleState::Closed | SessionLifecycleState::Failed(_) => false,
            _ => false,
        })
        .unwrap_or(false)
    }

    async fn send(&self, req: Request) -> Result<(), Error> {
        if let Some(sender) = self.sender.load_full() {
            sender
                .send(req)
                .await
                .map_err(Error::ErrSendRequest)
                .map(|_| ())
        } else {
            Err(Error::ErrUseClosedConnection)
        }
    }

    pub async fn send_asdu(&self, asdu: Asdu) -> Result<(), Error> {
        if !self.is_connected().await {
            return Err(Error::ErrUseClosedConnection);
        }
        if !self.is_active().await {
            return Err(Error::ErrNotActive);
        }
        self.send(Request::I(asdu, Priority::High)).await
    }

    pub async fn send_asdu_low_priority(&self, asdu: Asdu) -> Result<(), Error> {
        if !self.is_connected().await {
            return Err(Error::ErrUseClosedConnection);
        }
        if !self.is_active().await {
            return Err(Error::ErrNotActive);
        }
        self.send(Request::I(asdu, Priority::Low)).await
    }

    pub async fn send_start_dt(&self) -> Result<(), Error> {
        if !self.is_connected().await {
            return Err(Error::ErrUseClosedConnection);
        }
        self.send(Request::U(UApci {
            function: U_STARTDT_ACTIVE,
        }))
        .await
    }

    pub async fn send_stop_dt(&self) -> Result<(), Error> {
        if !self.is_connected().await {
            return Err(Error::ErrUseClosedConnection);
        }
        self.send(Request::U(UApci {
            function: U_STOPDT_ACTIVE,
        }))
        .await
    }

    pub async fn interrogation_cmd(
        &self,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        qoi: ObjectQOI,
    ) -> Result<(), Error> {
        self.send_asdu(interrogation_cmd(cot, ca, qoi)?).await
    }

    pub async fn counter_interrogation_cmd(
        &self,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        qcc: ObjectQCC,
    ) -> Result<(), Error> {
        self.send_asdu(counter_interrogation_cmd(cot, ca, qcc)?)
            .await
    }

    pub async fn single_cmd(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: SingleCommandInfo,
    ) -> Result<(), Error> {
        self.send_asdu(single_cmd(type_id, cot, ca, cmd)?).await
    }

    pub async fn double_cmd(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: DoubleCommandInfo,
    ) -> Result<(), Error> {
        self.send_asdu(double_cmd(type_id, cot, ca, cmd)?).await
    }

    pub async fn step_cmd(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: StepCommandInfo,
    ) -> Result<(), Error> {
        self.send_asdu(step_cmd(type_id, cot, ca, cmd)?).await
    }

    pub async fn set_point_cmd_normal(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: SetPointCommandNormalInfo,
    ) -> Result<(), Error> {
        self.send_asdu(set_point_cmd_normal(type_id, cot, ca, cmd)?)
            .await
    }

    pub async fn set_point_cmd_scaled(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: SetPointCommandScaledInfo,
    ) -> Result<(), Error> {
        self.send_asdu(set_point_cmd_scaled(type_id, cot, ca, cmd)?)
            .await
    }

    pub async fn set_point_cmd_float(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: SetPointCommandFloatInfo,
    ) -> Result<(), Error> {
        self.send_asdu(set_point_cmd_float(type_id, cot, ca, cmd)?)
            .await
    }

    pub async fn bits_string32_cmd(
        &self,
        type_id: TypeID,
        cot: CauseOfTransmission,
        ca: CommonAddr,
        cmd: BitsString32CommandInfo,
    ) -> Result<(), Error> {
        self.send_asdu(bits_string32_cmd(type_id, cot, ca, cmd)?)
            .await
    }
}

pub struct SessionEventLoop {
    session: Arc<Session>,
    inner_cancel: CancellationToken,
    socket_addr: SocketAddr,
    config: SessionConfig,
    pre_connected: Option<TcpStream>,
}

impl SessionEventLoop {
    pub fn enter(self) -> impl Stream<Item = SessionEvent> {
        let session = Arc::clone(&self.session);
        let events_rx = session.subscribe_events();

        // spawn IO driver
        let cancel = self.inner_cancel.child_token();
        let socket_addr = self.socket_addr;
        let config = self.config;
        let pre = self.pre_connected;
        tokio::spawn(async move {
            if let Some(stream) = pre {
                run_connection_with_stream(session, stream, config, cancel).await;
            } else {
                run_connection(session, socket_addr, config, cancel).await;
            }
        });

        // Forward events as a stream
        futures::stream::unfold(events_rx, |mut rx| async move {
            match rx.recv().await {
                Ok(ev) => Some((ev, rx)),
                Err(_) => None,
            }
        })
    }

    pub async fn run(self) {
        let mut s = self.enter();
        pin_mut!(s);
        while let Some(_ev) = s.next().await {}
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    pub fn cancel(&self) {
        self.inner_cancel.cancel();
    }
}

pub fn create(socket_addr: SocketAddr, config: SessionConfig) -> (Arc<Session>, SessionEventLoop) {
    let (events_tx, _rx) = broadcast::channel::<SessionEvent>(1024);
    let (lifecycle_tx, lifecycle_rx) = watch::channel(SessionLifecycleState::Idle);
    let (asdu_tx, asdu_rx) = mpsc::channel::<Asdu>(1024);
    let sender: Arc<ArcSwapOption<mpsc::Sender<Request>>> = Arc::new(ArcSwapOption::from(None));

    let session = Arc::new(Session {
        sender,
        events_tx,
        asdu_tx,
        asdu_rx: Mutex::new(Some(asdu_rx)),
        lifecycle_tx: lifecycle_tx.clone(),
        lifecycle_rx,
    });
    let event_loop = SessionEventLoop {
        session: Arc::clone(&session),
        inner_cancel: CancellationToken::new(),
        socket_addr,
        config,
        pre_connected: None,
    };
    (session, event_loop)
}

pub fn create_with_stream(
    socket_addr: SocketAddr,
    config: SessionConfig,
    stream: TcpStream,
) -> (Arc<Session>, SessionEventLoop) {
    let (session, _ev_unused) = create(socket_addr, config);
    let event_loop = SessionEventLoop {
        session: Arc::clone(&session),
        inner_cancel: CancellationToken::new(),
        socket_addr,
        config,
        pre_connected: Some(stream),
    };
    (session, event_loop)
}

async fn run_connection(
    session: Arc<Session>,
    socket_addr: SocketAddr,
    config: SessionConfig,
    cancel: CancellationToken,
) {
    // establish a single transport (no auto reconnect here)
    let _ = session.lifecycle_tx.send(SessionLifecycleState::Connecting);
    let connect_fut = TcpStream::connect(socket_addr);
    // Use connect_timeout_ms for transport establishment; reserve t0_ms for control confirms
    let transport = match timeout(
        Duration::from_millis(config.connection_timeout_ms),
        connect_fut,
    )
    .await
    {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            let reason = format!("connect failed: {}", e);
            let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
                SessionLifecycleState::Failed(reason.clone()),
            ));
            let _ = session
                .lifecycle_tx
                .send(SessionLifecycleState::Failed(reason));
            return;
        }
        Err(_elapsed) => {
            let reason = format!("connect timeout after {} ms", config.connection_timeout_ms);
            let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
                SessionLifecycleState::Failed(reason.clone()),
            ));
            let _ = session
                .lifecycle_tx
                .send(SessionLifecycleState::Failed(reason));
            tracing::warn!(timeout_ms = config.connection_timeout_ms, "connect timeout");
            return;
        }
    };
    run_connection_with_stream(session, transport, config, cancel).await;
}

async fn run_connection_with_stream(
    session: Arc<Session>,
    transport: TcpStream,
    config: SessionConfig,
    cancel: CancellationToken,
) {
    if let Err(e) = transport.set_nodelay(config.tcp_nodelay) {
        tracing::warn!(error=%e, tcp_nodelay=config.tcp_nodelay, "set TCP_NODELAY failed");
    }
    let mut framed = Framed::new(transport, Codec);
    let (tx, mut rx) = mpsc::channel::<Request>(config.send_queue_capacity);
    session.sender.store(Some(Arc::new(tx.clone())));
    let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
        SessionLifecycleState::Handshaking,
    ));
    let _ = session
        .lifecycle_tx
        .send(SessionLifecycleState::Handshaking);

    let mut state = SessionState::new(Utc::now());
    let mut tick = interval(Duration::from_millis(100));

    // high-priority backlog when window is full; bounded VecDeque
    let mut hi_queue: VecDeque<(Asdu, usize)> = VecDeque::with_capacity(config.send_queue_capacity);
    let mut hi_queue_bytes: usize = 0;

    // timers reference moments
    let mut test_sent_since = state.test_sent_since;
    let mut start_sent_since = state.start_sent_since;
    let mut stop_sent_since = state.stop_sent_since;

    // default to inactive, require STARTDT confirm
    let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
        SessionLifecycleState::Inactive,
    ));
    let _ = session.lifecycle_tx.send(SessionLifecycleState::Inactive);
    start_sent_since = Utc::now();
    if let Err(e) = framed.send(new_uframe(U_STARTDT_ACTIVE)).await {
        tracing::warn!(error=%e, "send STARTDT failed");
        let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
            SessionLifecycleState::Closing,
        ));
        let _ = session.lifecycle_tx.send(SessionLifecycleState::Closing);
        // best-effort close and cleanup
        let _ = framed.close().await;
        session.sender.store(None);
        let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
            SessionLifecycleState::Closed,
        ));
        let _ = session.lifecycle_tx.send(SessionLifecycleState::Closed);
        return;
    }

    loop {
        select! {
            _ = cancel.cancelled() => { break; }
            _ = tick.tick() => {
                let now = Utc::now();
                // t0/t1/t2/t3 checks
                // t1: pending front timeout warning and cleanup
                if let Some(drop_seq) = state.check_and_handle_t1_timeout(now, config.t1_ms) {
                    let _ = session.events_tx.send(SessionEvent::T1Timeout);
                    tracing::warn!(seq = drop_seq, count=state.consecutive_t1_timeouts, "t1 timeout; drop pending");
                    // if too many consecutive t1 timeouts, close session to trigger reconnect
                    if state.consecutive_t1_timeouts >= 5 {
                        tracing::error!(count=state.consecutive_t1_timeouts, "too many consecutive t1 timeouts; closing session");
                        break;
                    }
                }

                // t3: idle test frame
                if now.signed_duration_since(state.idle_since).num_milliseconds() as u64 >= config.t3_ms {
                    test_sent_since = now;
                    if let Err(e) = framed.send(new_uframe(U_TESTFR_ACTIVE)).await { tracing::warn!(error=%e, "send TESTFR failed"); continue; }
                    state.idle_since = now;
                }

                // t2/w: send S-ACK aggregation
                if state.should_send_s_ack(now, config.t2_ms, config.w_threshold) {
                    if let Err(e) = framed.send(new_sframe(state.rcv_sn)).await { tracing::warn!(error=%e, "send S-ACK failed"); }
                    state.mark_s_ack_sent();
                }

                // flush high-priority backlog first when window allows
                if matches!(session.lifecycle_rx.borrow().clone(), SessionLifecycleState::Active)
                    && state.window_has_capacity(config.k_window)
                {
                    let mut slots = state.window_available_slots(config.k_window);
                    while slots > 0 {
                        if let Some((asdu, size_bytes)) = hi_queue.pop_front() {
                            let apdu = new_iframe(asdu, state.send_sn, state.rcv_sn);
                            if let ApciKind::I(iapci) = ApciKind::from(apdu.apci) {
                                if let Err(e) = framed.send(apdu).await {
                                    tracing::warn!(error=%e, "send i-frame (backlog) failed");
                                    break;
                                }
                                state.push_pending(iapci.send_sn, now, size_bytes);
                                state.ack_rcvsn = state.rcv_sn;
                                state.send_sn = SessionState::seq_add(state.send_sn, 1);
                                hi_queue_bytes = hi_queue_bytes.saturating_sub(size_bytes);
                                slots -= 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                    // low-priority: flush by age or when empty window
                    if hi_queue.is_empty()
                        && config.low_prio_flush_max_age_ms > 0
                        && state.should_flush_low_prio(now, config.low_prio_flush_max_age_ms)
                    {
                        if let Some(asdu) = state.take_last_low_prio() {
                            let _ = tx.try_send(Request::I(asdu, Priority::Low));
                        }
                    }
                }

                // t0: START/STOP/TESTFR confirm timeout (only when timer is armed)
                let t0_exceeded = |since: DateTime<Utc>| {
                    let delta_ms = now.signed_duration_since(since).num_milliseconds();
                    delta_ms >= 0 && (delta_ms as u64) >= config.t0_ms
                };
                if t0_exceeded(test_sent_since)
                    || t0_exceeded(start_sent_since)
                    || t0_exceeded(stop_sent_since)
                {
                    tracing::error!("control confirm t0 timeout; closing session");
                    break;
                }
            }

            // app -> wire
            maybe_req = rx.recv() => {
                match maybe_req {
                    Some(Request::I(asdu, prio)) => {
                        if !matches!(session.lifecycle_rx.borrow().clone(), SessionLifecycleState::Active) { continue; }

                        let size_bytes = iframe_wire_size_for_asdu(&asdu);
                        if size_bytes > APDU_SIZE_MAX {
                            let _ = session.events_tx.send(SessionEvent::FrameTooLargeDrop);
                            continue;
                        }

                        if !state.window_has_capacity(config.k_window) {
                            match prio {
                                Priority::High => {
                                    if hi_queue.len() < config.send_queue_capacity
                                        && state
                                            .total_pending_bytes()
                                            .saturating_add(hi_queue_bytes)
                                            .saturating_add(size_bytes)
                                            <= config.max_pending_asdu_bytes
                                    {
                                        hi_queue.push_back((asdu, size_bytes));
                                        hi_queue_bytes = hi_queue_bytes.saturating_add(size_bytes);
                                    } else {
                                        let _ = session.events_tx.send(SessionEvent::WindowFullDrop);
                                    }
                                    continue;
                                }
                                Priority::Low => {
                                    if config.discard_low_priority_when_window_full {
                                        let _ = session.events_tx.send(SessionEvent::WindowFullDrop);
                                    } else if config.merge_low_priority {
                                        state.set_last_low_prio(asdu, Utc::now());
                                        let _ = session.events_tx.send(SessionEvent::LowPriorityMerged);
                                    } else if hi_queue.len() < config.send_queue_capacity
                                        && state
                                            .total_pending_bytes()
                                            .saturating_add(hi_queue_bytes)
                                            .saturating_add(size_bytes)
                                            <= config.max_pending_asdu_bytes
                                    {
                                        // best-effort queue into hi_queue tail if memory budget allows
                                        hi_queue.push_back((asdu, size_bytes));
                                        hi_queue_bytes = hi_queue_bytes.saturating_add(size_bytes);
                                    } else {
                                        let _ = session.events_tx.send(SessionEvent::WindowFullDrop);
                                    }
                                    continue;
                                }
                            }
                        }

                        if state
                            .total_pending_bytes()
                            .saturating_add(hi_queue_bytes)
                            .saturating_add(size_bytes)
                            > config.max_pending_asdu_bytes
                        {
                            match prio {
                                Priority::High => {
                                    let _ = session.events_tx.send(SessionEvent::MemoryBudgetDrop);
                                    continue;
                                }
                                Priority::Low => {
                                    if config.discard_low_priority_when_window_full {
                                        let _ = session.events_tx.send(SessionEvent::MemoryBudgetDrop);
                                    } else if config.merge_low_priority {
                                        state.set_last_low_prio(asdu, Utc::now());
                                        let _ = session.events_tx.send(SessionEvent::LowPriorityMerged);
                                    } else {
                                        let _ = session.events_tx.send(SessionEvent::MemoryBudgetDrop);
                                    }
                                    continue;
                                }
                            }
                        }

                        let apdu = new_iframe(asdu, state.send_sn, state.rcv_sn);
                        if let ApciKind::I(iapci) = ApciKind::from(apdu.apci) {
                            if let Err(e) = framed.send(apdu).await {
                                tracing::warn!(error=%e, "send i-frame failed");
                                break;
                            }
                            state.push_pending(iapci.send_sn, Utc::now(), size_bytes);
                            state.ack_rcvsn = state.rcv_sn;
                            state.send_sn = SessionState::seq_add(state.send_sn, 1);
                        }
                    }
                    Some(Request::U(u)) => {
                        match u.function {
                            U_STARTDT_ACTIVE => start_sent_since = Utc::now(),
                            U_STOPDT_ACTIVE => stop_sent_since = Utc::now(),
                            _ => {}
                        }
                        if let Err(e) = framed.send(new_uframe(u.function)).await {
                            tracing::warn!(error=%e, "send u-frame failed");
                            break;
                        }
                    }
                    Some(Request::S(s)) => {
                        if let Err(e) = framed.send(new_sframe(s.rcv_sn)).await {
                            tracing::warn!(error=%e, "send s-frame failed");
                            break;
                        }
                    }
                    None => { break; }
                }
            }

            // wire -> app
            maybe_apdu = framed.next() => {
                match maybe_apdu {
                    Some(Ok(apdu)) => {
                        state.idle_since = Utc::now();
                        match apdu.apci.into() {
                            ApciKind::I(iapci) => {
                                if !state.update_send_ack(iapci.rcv_sn) || !state.advance_receive_seq(iapci.send_sn) {
                                    tracing::error!("invalid ack or seq"); break;
                                }
                                state.mark_unacked_receive(Utc::now());

                                if let Some(asdu) = apdu.asdu {
                                    let _ = session.asdu_tx.try_send(asdu);
                                }
                            }
                            ApciKind::U(uapci) => {
                                match uapci.function {
                                    U_STARTDT_CONFIRM => {
                                        start_sent_since = DateTime::<Utc>::MAX_UTC;
                                        let _ = session.events_tx.send(SessionEvent::LifecycleChanged(SessionLifecycleState::Active));
                                        let _ = session.lifecycle_tx.send(SessionLifecycleState::Active);
                                    }
                                    U_STOPDT_CONFIRM => {
                                        stop_sent_since = DateTime::<Utc>::MAX_UTC;
                                        let _ = session.events_tx.send(SessionEvent::LifecycleChanged(SessionLifecycleState::Inactive));
                                        let _ = session.lifecycle_tx.send(SessionLifecycleState::Inactive);
                                    }
                                    U_TESTFR_CONFIRM => {
                                        test_sent_since = DateTime::<Utc>::MAX_UTC;
                                    }
                                    U_STARTDT_ACTIVE => {
                                        if let Err(e) = framed.send(new_uframe(U_STARTDT_CONFIRM)).await {
                                            tracing::warn!(error=%e, "send STARTDT_CONFIRM failed");
                                        } else {
                                            let _ = session.events_tx.send(SessionEvent::LifecycleChanged(SessionLifecycleState::Active));
                                            let _ = session.lifecycle_tx.send(SessionLifecycleState::Active);
                                        }
                                    }
                                    U_STOPDT_ACTIVE => {
                                        if let Err(e) = framed.send(new_uframe(U_STOPDT_CONFIRM)).await {
                                            tracing::warn!(error=%e, "send STOPDT_CONFIRM failed");
                                        } else {
                                            let _ = session.events_tx.send(SessionEvent::LifecycleChanged(SessionLifecycleState::Inactive));
                                            let _ = session.lifecycle_tx.send(SessionLifecycleState::Inactive);
                                        }
                                    }
                                    U_TESTFR_ACTIVE => {
                                         if let Err(e) = framed.send(new_uframe(U_TESTFR_CONFIRM)).await {
                                            tracing::warn!(error=%e, "send TESTFR_CONFIRM failed");
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            ApciKind::S(sapci) => {
                                if !state.update_send_ack(sapci.rcv_sn) { tracing::error!("invalid s-ack"); break; }
                            }
                        }
                    }
                    _ => { break; }
                }
            }
        }
    }
    // on exit, ensure lifecycle transitions to Closing -> Closed once
    // avoid duplicate Closing if already set
    match session.current_lifecycle() {
        SessionLifecycleState::Closing
        | SessionLifecycleState::Closed
        | SessionLifecycleState::Failed(_) => {}
        _ => {
            let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
                SessionLifecycleState::Closing,
            ));
            let _ = session.lifecycle_tx.send(SessionLifecycleState::Closing);
        }
    }
    // best-effort close sink and clear sender to reflect disconnected state
    let _ = framed.close().await;
    session.sender.store(None);
    let _ = session.events_tx.send(SessionEvent::LifecycleChanged(
        SessionLifecycleState::Closed,
    ));
    let _ = session.lifecycle_tx.send(SessionLifecycleState::Closed);
}
