mod handshake;
mod state;
use ng_gateway_sdk::{WireDecode, WireEncode};
pub use state::{SessionConfig, SessionEvent, SessionLifecycleState};

use super::{
    codec::Codec,
    error::{Error, Result},
    frame::{
        addr::S7Address, build_cotp_data_message, build_read_var, build_userdata_read_szl_request,
        build_write_var, Cotp, S7AckDataPayloadRef, S7AppBody, S7DataValue, S7Message,
        S7PayloadRef, S7Pdu, S7VarSpec,
    },
    planner::{PlannerConfig, ReadItemMerged, ReadMerged, ReadMergedTyped, S7Planner},
};
use arc_swap::ArcSwapOption;
use bytes::{Bytes, BytesMut};
use futures::{pin_mut, Stream};
use futures_util::{stream::SplitStream, SinkExt, StreamExt};
use std::{
    cmp::{max, min},
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    result::Result as StdResult,
    sync::{
        atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    select,
    sync::{broadcast, mpsc, oneshot, watch, Mutex, OwnedSemaphorePermit, Semaphore},
    time::sleep,
};
use tokio_util::{codec::Framed, sync::CancellationToken};

/// Request message for the session
#[derive(Debug)]
pub struct SessionRequest {
    /// Request payload
    pub payload: Bytes,
    /// Operation timeout
    pub timeout: Duration,
    /// Response channel
    pub response_tx: oneshot::Sender<Result<S7Pdu>>,
    /// Concurrency permit carried across the lifetime of this request
    pub permit: OwnedSemaphorePermit,
}

/// Entry stored for each inflight request.
///
/// Holding an instance of this struct implies that one concurrency slot
/// has been acquired from the session-level `Semaphore`. The slot is
/// represented by the `_permit` field. When this struct is dropped
/// (on response, timeout, IO error, or connection teardown), the permit
/// is released automatically via RAII, ensuring precise back-pressure
/// without scattered manual releases.
#[derive(Debug)]
struct InflightEntry {
    /// Response channel bound to the specific PDU reference
    tx: oneshot::Sender<Result<S7Pdu>>,
    /// RAII guard for the concurrency slot acquired at API boundary
    _permit: OwnedSemaphorePermit,
}

/// S7 session runtime state and IO with state machine
#[allow(unused)]
#[derive(Debug)]
pub struct Session {
    /// Session configuration
    config: Arc<SessionConfig>,
    /// Request channel for incoming requests
    request_tx: Arc<ArcSwapOption<mpsc::Sender<SessionRequest>>>,
    /// Cancellation token for cooperative shutdown
    cancel: CancellationToken,
    /// Events broadcaster
    events_tx: broadcast::Sender<SessionEvent>,
    /// Lifecycle watch channel (tx side)
    lifecycle_tx: watch::Sender<SessionLifecycleState>,
    /// Lifecycle watch channel (rx side)
    lifecycle_rx: watch::Receiver<SessionLifecycleState>,
    /// Semaphore for controlling concurrent requests (set after handshake)
    request_semaphore: Arc<ArcSwapOption<Semaphore>>,
    /// Metrics: current inflight requests gauge
    inflight_gauge: Arc<AtomicUsize>,
    /// Metrics: total request timeouts observed
    timeouts_total: Arc<AtomicU64>,
    /// Monotonic PDU reference generator with wrap-around in [1..=65535]
    pdu_ref_counter: AtomicU16,
    /// Negotiated planner configuration after handshake
    planner_config: Arc<Mutex<Option<PlannerConfig>>>,
}

/// Session event loop facade providing a stream of `SessionPollResult` and helpers
#[derive(Debug)]
pub struct SessionEventLoop {
    session: Arc<Session>,
    inner_cancel: CancellationToken,
    config: Arc<SessionConfig>,
    pre_connected: Option<TcpStream>,
}

impl SessionEventLoop {
    /// Enter and get a stream of poll results (events)
    pub fn enter(self) -> impl Stream<Item = SessionEvent> {
        let session = Arc::clone(&self.session);
        let events_rx = session.subscribe_events();

        // spawn IO driver
        let cancel = self.inner_cancel.child_token();
        let config = Arc::clone(&self.config);
        let pre = self.pre_connected;
        tokio::spawn(async move {
            if let Some(stream) = pre {
                run_connection_with_stream(session, stream, config, cancel).await;
            } else {
                run_connection(session, config, cancel).await;
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

    /// Drain the event stream (utility)
    pub async fn run(self) {
        let s = self.enter();
        pin_mut!(s);
        while let Some(_ev) = s.next().await {}
    }

    /// Spawn a background task to drain the event stream
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run())
    }

    /// Cancel the connection
    pub fn cancel(&self) {
        self.inner_cancel.cancel();
    }
}

impl Session {
    /// Create a new session (returns Arc<Self>)
    pub fn new(config: Arc<SessionConfig>, cancel: CancellationToken) -> Arc<Self> {
        let request_tx: Arc<ArcSwapOption<mpsc::Sender<SessionRequest>>> =
            Arc::new(ArcSwapOption::from(None));
        let (events_tx, _rx_unused) = broadcast::channel::<SessionEvent>(1024);
        let (lifecycle_tx, lifecycle_rx) = watch::channel(SessionLifecycleState::Idle);

        let request_semaphore = Arc::new(ArcSwapOption::from(None));
        let inflight_gauge = Arc::new(AtomicUsize::new(0));
        let timeouts_total = Arc::new(AtomicU64::new(0));
        let planner_config = Arc::new(Mutex::new(None));

        Arc::new(Session {
            config,
            request_tx,
            cancel: cancel.clone(),
            events_tx,
            lifecycle_tx,
            lifecycle_rx,
            request_semaphore,
            inflight_gauge,
            timeouts_total,
            pdu_ref_counter: AtomicU16::new(0),
            planner_config,
        })
    }

    /// Subscribe to session events.
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events_tx.subscribe()
    }

    /// Get a lifecycle watch receiver clone.
    pub fn lifecycle(&self) -> watch::Receiver<SessionLifecycleState> {
        self.lifecycle_rx.clone()
    }

    /// Get current lifecycle state.
    #[inline]
    pub fn current_lifecycle(&self) -> SessionLifecycleState {
        *self.lifecycle_rx.borrow()
    }

    /// Whether lifecycle is currently Active (fast path, no await).
    #[inline]
    pub fn is_active(&self) -> bool {
        matches!(self.current_lifecycle(), SessionLifecycleState::Active)
    }

    /// Graceful shutdown
    pub async fn shutdown(&self) {
        // Trigger cooperative cancellation for the session state machine
        self.cancel.cancel();
        // Close semaphore to wake any pending acquirers
        if let Some(sem) = self.request_semaphore.load_full() {
            sem.close();
        }
    }

    /// Wait until lifecycle reaches the expected state. Returns false if the
    /// lifecycle channel is closed before reaching it.
    pub async fn wait_for_state(&self, expected: SessionLifecycleState) -> bool {
        if self.current_lifecycle() == expected {
            return true;
        }
        let mut rx = self.lifecycle();
        let ok = rx.wait_for(|s| *s == expected).await.is_ok();
        ok
    }

    /// Wait until the transport is connected and session enters at least
    /// Handshaking/Active. Returns false if lifecycle goes to Closed/Failed.
    pub async fn wait_for_active(&self) -> bool {
        if self.is_active() {
            return true;
        }
        let mut rx = self.lifecycle();
        rx.wait_for(|s| {
            matches!(
                *s,
                SessionLifecycleState::Active
                    | SessionLifecycleState::Closed
                    | SessionLifecycleState::Failed
            )
        })
        .await
        .map(|s| match *s {
            SessionLifecycleState::Active => true,
            SessionLifecycleState::Closed | SessionLifecycleState::Failed => false,
            _ => false,
        })
        .unwrap_or(false)
    }

    /// Generate next PDU reference in range [1..=65535] with lock-free wrap-around.
    #[inline]
    fn next_pdu_ref(&self) -> u16 {
        loop {
            let cur = self.pdu_ref_counter.load(Ordering::Relaxed);
            let mut next = cur.wrapping_add(1);
            if next == 0 {
                next = 1;
            }
            if self
                .pdu_ref_counter
                .compare_exchange(cur, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return next;
            }
        }
    }
}

/// Session external API
impl Session {
    /// Send a raw S7 payload (pre-encoded S7 PDU bytes to be wrapped by COTP-D) and await its response.
    ///
    /// This API will:
    /// - ensure lifecycle is Active
    /// - acquire the request semaphore (back pressure)
    /// - enqueue the request through `request_tx`
    /// - await the paired response
    pub async fn send_raw(&self, payload: Bytes, timeout: Duration) -> Result<S7Pdu> {
        // Fast-path lifecycle check
        if !self.is_active() {
            return Err(Error::ErrNotActive);
        }

        // Acquire concurrency permit (back pressure at API boundary)
        let permit = if let Some(sem) = self.request_semaphore.load_full() {
            match sem.acquire_owned().await {
                Ok(p) => p,
                Err(_e) => return Err(Error::ErrUseClosedConnection),
            }
        } else {
            // Handshake may not have completed properly
            return Err(Error::ErrUseClosedConnection);
        };

        let (tx, rx) = oneshot::channel();
        let req = SessionRequest {
            payload,
            timeout,
            response_tx: tx,
            permit,
        };

        // Send to request channel
        if let Some(sender) = self.request_tx.load_full() {
            sender
                .send(req)
                .await
                .map_err(|_| Error::ErrUseClosedConnection)?;
        } else {
            return Err(Error::ErrUseClosedConnection);
        }

        // Await paired response (router will timeout/close on our behalf)
        match rx.await {
            Ok(res) => res,
            Err(_) => Err(Error::ErrUseClosedConnection),
        }
    }

    /// ReadVar (optimized): plan under PDU cap, split-merge fragments, return merged data.
    async fn read_var(&self, specs: &[S7VarSpec]) -> Result<ReadMerged> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Use negotiated planner config; fall back to default PDU=960 if not yet set
        let planner = {
            let guard = self.planner_config.lock().await;
            guard.unwrap_or(PlannerConfig::new(960))
        };
        let plan = S7Planner::plan_read(&planner, specs);

        // Send each batch as a ReadVar job, await each response, collect payloads
        let mut responses: Vec<Bytes> = Vec::with_capacity(plan.batches.len());
        for batch in plan.batches.iter() {
            // Build request S7 Job bytes and wrap
            // Generate a per-session monotonic pdu_ref with wrap-around
            let pdu_ref = self.next_pdu_ref();
            let s7 = build_read_var(pdu_ref, batch);
            let resp = self.send_raw(s7, self.config.read_timeout).await?;
            resp.validate_response()?;
            let view = resp.as_ref_view()?;
            // Extract only payload bytes for merge helper
            if let S7PayloadRef::AckData(S7AckDataPayloadRef::ReadVarResponse(r)) = view.payload {
                responses.push(Bytes::copy_from_slice(r.raw_tail));
            } else {
                return Err(Error::ErrUnexpectedPdu);
            }
        }

        Ok(plan.merge(&responses))
    }

    /// WriteVar (optimized): plan under PDU cap and send sequential batches.
    async fn write_var(&self, items: &[(S7VarSpec, Bytes)]) -> Result<Vec<S7Pdu>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        let planner = {
            let guard = self.planner_config.lock().await;
            guard.unwrap_or(PlannerConfig::new(960))
        };
        let plan = S7Planner::plan_write(&planner, items);

        let mut acks: Vec<S7Pdu> = Vec::with_capacity(plan.batches.len());
        for batch in plan.batches.iter() {
            let pdu_ref = self.next_pdu_ref();
            // Transform to (spec, &[u8]) temporary view for builder
            let temp = batch
                .iter()
                .map(|(s, d)| (*s, d.as_ref()))
                .collect::<Vec<_>>();
            let s7 = build_write_var(pdu_ref, &temp);
            let resp = self.send_raw(s7, self.config.write_timeout).await?;
            resp.validate_response()?;
            acks.push(resp);
        }
        Ok(acks)
    }

    /// Convenience: CPU Functions Read SZL request via UserData PDU.
    pub async fn read_szl(&self, param_extra: Bytes) -> Result<S7Pdu> {
        if !self.is_active() {
            return Err(Error::ErrNotActive);
        }
        let pdu_ref = self.next_pdu_ref();
        let pdu = build_userdata_read_szl_request(pdu_ref, param_extra);
        let raw = pdu.into_bytes();
        let resp = self.send_raw(raw, self.config.read_timeout).await?;
        resp.validate_response()?;
        Ok(resp)
    }

    /// Read a single address expressed as a string (e.g. "DB1.DBD0", "I0.0").
    ///
    /// This helper parses the textual address using the robust parser and
    /// performs an optimized ReadVar with `count=1`. The merged result for the
    /// single item is returned. For bit addresses, data length is 1 byte.
    pub async fn read_address(&self, address: S7Address) -> Result<ReadItemMerged> {
        let mut merged = self.read_addresses(&[address]).await?;
        if merged.len() == 1 {
            Ok(merged.remove(0))
        } else {
            Err(Error::ErrUnexpectedPdu)
        }
    }

    /// Batch read a list of string addresses. Each address is parsed with `count=1`.
    ///
    /// Returns a merged result aligned with the input order. For bit addresses,
    /// each item's data has length 1 byte.
    pub async fn read_addresses(&self, addresses: &[S7Address]) -> Result<ReadMerged> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let specs: Vec<S7VarSpec> = addresses
            .iter()
            .map(S7VarSpec::try_from)
            .collect::<StdResult<_, _>>()?;
        self.read_var(&specs).await
    }

    /// Write a single string address with the provided data bytes.
    ///
    /// The address is parsed with `count=1`. For Bit addresses, provide a 1-byte buffer.
    /// Returns the AckData PDU for the write request.
    pub async fn write_address(&self, address: S7Address, data: Bytes) -> Result<S7Pdu> {
        let mut acks = self.write_addresses(&[(address, data)]).await?;
        if acks.len() == 1 {
            Ok(acks.remove(0))
        } else {
            Err(Error::ErrUnexpectedPdu)
        }
    }

    /// Write a single string address with a data slice convenience overload.
    #[inline]
    pub async fn write_address_slice(&self, address: S7Address, data: &[u8]) -> Result<S7Pdu> {
        self.write_address(address, Bytes::copy_from_slice(data))
            .await
    }

    /// Batch write a list of (address, data) where address is a string and data is Bytes.
    ///
    /// Each address is parsed with `count=1`. For Bit addresses, each data buffer should be 1 byte.
    /// Returns AckData PDUs in order of planned batches.
    pub async fn write_addresses(&self, items: &[(S7Address, Bytes)]) -> Result<Vec<S7Pdu>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut specs_and_data: Vec<(S7VarSpec, Bytes)> = Vec::with_capacity(items.len());
        for (addr, data) in items.iter() {
            let spec = S7VarSpec::try_from(addr)?;
            specs_and_data.push((spec, data.clone()));
        }
        self.write_var(&specs_and_data).await
    }

    /// Batch write convenience overload where data is passed as slices.
    pub async fn write_addresses_slices(&self, items: &[(S7Address, &[u8])]) -> Result<Vec<S7Pdu>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut specs_and_data: Vec<(S7VarSpec, Bytes)> = Vec::with_capacity(items.len());
        for (addr, data) in items.iter() {
            let spec = S7VarSpec::try_from(addr)?;
            specs_and_data.push((spec, Bytes::copy_from_slice(data)));
        }
        self.write_var(&specs_and_data).await
    }

    /// ReadVar (typed): plan under PDU cap, then decode into typed values using planner's merge_typed.
    ///
    /// Returns a vector of typed results aligned with the `specs` order. Each item contains
    /// the return code and an optional `S7DataValue` when successful.
    pub async fn read_var_typed(&self, specs: &[S7VarSpec]) -> Result<ReadMergedTyped> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Use negotiated planner config; fall back to default PDU=960 if not yet set
        let planner_cfg = {
            let guard = self.planner_config.lock().await;
            guard.unwrap_or(PlannerConfig::new(960))
        };
        let plan = S7Planner::plan_read(&planner_cfg, specs);

        // Send each batch and capture raw payload bytes for merge
        let mut responses = Vec::with_capacity(plan.batches.len());
        for batch in plan.batches.iter() {
            let pdu_ref = self.next_pdu_ref();
            let s7 = build_read_var(pdu_ref, batch);
            let resp = self.send_raw(s7, self.config.read_timeout).await?;
            resp.validate_response()?;
            let view = resp.as_ref_view()?;
            if let S7PayloadRef::AckData(S7AckDataPayloadRef::ReadVarResponse(r)) = view.payload {
                responses.push(Bytes::copy_from_slice(r.raw_tail));
            } else {
                return Err(Error::ErrUnexpectedPdu);
            }
        }

        plan.merge_typed(&responses)
    }

    /// WriteVar (typed): encode provided typed values per `S7VarSpec` and send sequential batches.
    ///
    /// Only scalar writes are supported (spec.count must be 1). For Bit writes, the boolean value
    /// is encoded as a single byte 0x00/0x01; the target bit is specified by `spec.bit_index`.
    pub async fn write_var_typed(&self, items: &[(S7VarSpec, S7DataValue)]) -> Result<Vec<S7Pdu>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }

        // Encode each typed value using S7DataValue::WireEncode; caller is responsible for
        // providing a matching `S7VarSpec.transport_size`. We only enforce scalar writes here.
        let mut encoded = Vec::with_capacity(items.len());
        for (spec, val) in items.iter() {
            if spec.count != 1 {
                return Err(Error::UnsupportedFeature {
                    feature: "typed write requires spec.count == 1",
                });
            }
            let mut buf = BytesMut::with_capacity(val.encoded_len(&()));
            let _ = val.encode_to(&mut buf, &());
            encoded.push((*spec, buf.freeze()));
        }
        self.write_var(&encoded).await
    }

    /// Read a single `S7Address` and decode it as a typed value.
    pub async fn read_address_typed(&self, address: &S7Address) -> Result<S7DataValue> {
        let mut results = self.read_addresses_typed(&[address]).await?;
        if results.len() == 1 {
            let it = results.remove(0);
            match it.value {
                Some(v) => Ok(v),
                None => Err(Error::ErrUnexpectedPdu),
            }
        } else {
            Err(Error::ErrUnexpectedPdu)
        }
    }

    /// Batch read a list of `S7Address` and decode them as typed values.
    pub async fn read_addresses_typed(&self, addresses: &[&S7Address]) -> Result<ReadMergedTyped> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let specs: Vec<S7VarSpec> = addresses
            .iter()
            .map(|address| S7VarSpec::try_from(*address))
            .collect::<StdResult<_, _>>()?;
        self.read_var_typed(&specs).await
    }

    /// Write a single `S7Address` with a typed value. Only scalar writes are supported.
    pub async fn write_address_typed(
        &self,
        address: &S7Address,
        value: S7DataValue,
    ) -> Result<S7Pdu> {
        let spec = S7VarSpec::try_from(address)?;
        let mut acks = self.write_var_typed(&[(spec, value)]).await?;
        if acks.len() == 1 {
            Ok(acks.remove(0))
        } else {
            Err(Error::ErrUnexpectedPdu)
        }
    }

    /// Batch write a list of (`S7Address`, typed value). Only scalar writes are supported.
    pub async fn write_addresses_typed(
        &self,
        items: &[(&S7Address, S7DataValue)],
    ) -> Result<Vec<S7Pdu>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut specs_and_vals = Vec::with_capacity(items.len());
        for (addr, v) in items.iter() {
            let spec = S7VarSpec::try_from(*addr)?;
            specs_and_vals.push((spec, v.clone()));
        }
        self.write_var_typed(&specs_and_vals).await
    }
}

/// Create a new S7 session and event loop (IEC104-aligned signature)
pub fn create(config: Arc<SessionConfig>) -> (Arc<Session>, SessionEventLoop) {
    let cancel = CancellationToken::new();
    let config = Arc::new(config);
    let session = Session::new(Arc::clone(&config), cancel);
    let ev = SessionEventLoop {
        session: Arc::clone(&session),
        inner_cancel: CancellationToken::new(),
        config: Arc::clone(&config),
        pre_connected: None,
    };
    (session, ev)
}

/// Create a new S7 session and event loop with a pre-connected TcpStream (IEC104-aligned)
pub fn create_with_stream(
    config: Arc<SessionConfig>,
    stream: TcpStream,
) -> (Arc<Session>, SessionEventLoop) {
    let cancel = CancellationToken::new();
    let config = Arc::new(config);
    let session = Session::new(Arc::clone(&config), cancel);
    let ev = SessionEventLoop {
        session: Arc::clone(&session),
        inner_cancel: CancellationToken::new(),
        config: Arc::clone(&config),
        pre_connected: Some(stream),
    };
    (session, ev)
}

/// Main connection driver for S7 session (IEC104-aligned): establish transport internally
async fn run_connection(
    session: Arc<Session>,
    config: Arc<SessionConfig>,
    cancel: CancellationToken,
) {
    // establish transport here (align with IEC104)
    publish_lifecycle(
        &session.events_tx,
        &session.lifecycle_tx,
        SessionLifecycleState::Connecting,
    );
    let stream = match tokio::time::timeout(
        config.connect_timeout,
        TcpStream::connect(config.socket_addr),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => {
            publish_lifecycle(
                &session.events_tx,
                &session.lifecycle_tx,
                SessionLifecycleState::Failed,
            );
            return;
        }
    };
    let _ = stream.set_nodelay(config.tcp_nodelay);
    run_connection_with_stream(session, stream, config, cancel).await;
}

/// Main connection driver using a pre-connected TcpStream (IEC104-aligned)
async fn run_connection_with_stream(
    session: Arc<Session>,
    stream: TcpStream,
    config: Arc<SessionConfig>,
    cancel: CancellationToken,
) {
    // Prepare request channel for the session
    let (request_tx, mut request_rx) = mpsc::channel(config.send_queue_capacity);
    session.request_tx.store(Some(Arc::new(request_tx)));

    // Local bindings
    let events_tx = session.events_tx.clone();
    let lifecycle_tx = session.lifecycle_tx.clone();
    let inflight_gauge = Arc::clone(&session.inflight_gauge);
    let timeouts_total = Arc::clone(&session.timeouts_total);
    let planner_config = Arc::clone(&session.planner_config);

    // Move to Handshaking
    publish_lifecycle(
        &events_tx,
        &lifecycle_tx,
        SessionLifecycleState::Handshaking,
    );

    // Handshake: COTP CR/CC then S7 SetupCommunication
    let mut framed = Framed::new(stream, Codec);
    if handshake::iso_connect(&mut framed, Arc::clone(&config))
        .await
        .is_err()
    {
        let _ = events_tx.send(SessionEvent::TransportError);
        publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Failed);
        return;
    }
    let negotiated = match handshake::negotiation(&mut framed, Arc::clone(&config), 0).await {
        Ok(v) => v,
        Err(_e) => {
            let _ = events_tx.send(SessionEvent::TransportError);
            publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Failed);
            return;
        }
    };

    let amq_callee = negotiated.amq_callee as usize;
    {
        let mut guard = planner_config.lock().await;
        *guard = Some(PlannerConfig::new(negotiated.pdu_len));
    }

    // Initialize concurrency gate based on negotiated limits
    let effective_max = min(amq_callee, config.max_concurrent_requests);
    session
        .request_semaphore
        .store(Some(Arc::new(Semaphore::new(effective_max))));

    // Split framed and wrap in Option to ease borrow across select!
    let (sink, stream) = framed.split();
    let mut sink_opt = Some(sink);
    let mut stream_opt = Some(stream);
    let mut reasm: Option<BytesMut> = None;
    let mut inflight: HashMap<u16, InflightEntry> = HashMap::with_capacity(64);
    let mut timeouts = BTreeMap::<Instant, Vec<u16>>::new();
    // Reusable sleep future to avoid per-iteration allocation and jitter
    let mut deadline_sleep = Box::pin(sleep(Duration::from_millis(3_600_000)));

    // Move to Established/Active
    publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Active);

    loop {
        // Reset reusable sleep to the nearest deadline (or a far future)
        if let Some(dl) = timeouts.keys().next().cloned() {
            deadline_sleep
                .as_mut()
                .reset(tokio::time::Instant::from_std(dl));
        } else {
            let far = tokio::time::Instant::now() + Duration::from_millis(3_600_000);
            deadline_sleep.as_mut().reset(far);
        }
        select! {
            _ = cancel.cancelled() => {
                publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Closing);
            }
            req = request_rx.recv() => {
                match req {
                    Some(request) => {
                        if let Some((pdu_ref, payload)) = register_inflight_request(
                            request,
                            &mut inflight,
                            &inflight_gauge,
                            &mut timeouts,
                        ) {
                            let msg = build_cotp_data_message(payload);
                            if let Some(s) = sink_opt.as_mut() {
                                if let Err(_e) = s.send(msg).await {
                                    handle_send_failure(pdu_ref, &mut inflight, &inflight_gauge);
                                }
                            }
                        }
                    }
                    None => break,
                }
            }
            pkt_res = poll_next_msg(&mut stream_opt) => {
                match pkt_res {
                    Some(Ok(msg)) => {
                        handle_incoming_s7_message(
                            msg,
                            negotiated.pdu_len,
                            &mut reasm,
                            &mut inflight,
                            &inflight_gauge,
                            &events_tx,
                        );
                    }
                    Some(Err(_e)) => {
                        let _ = events_tx.send(SessionEvent::TransportError);
                    }
                    None => {
                        let _ = events_tx.send(SessionEvent::TransportError);
                        break;
                    }
                }
            }
            _ = &mut deadline_sleep => {
                handle_request_timeouts(
                    &mut timeouts,
                    &mut inflight,
                    &inflight_gauge,
                    &timeouts_total,
                );
            }
        }
    }

    publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Closed);
}

/// Register a new incoming request as inflight and compute its timeout deadline.
///
/// This helper validates the payload, extracts the PDU reference, tracks the
/// associated oneshot channel and concurrency permit, updates the inflight gauge,
/// and stores the timeout deadline in the `timeouts` map. When the payload is
/// invalid, the error is sent back immediately and `None` is returned.
#[inline]
fn register_inflight_request(
    request: SessionRequest,
    inflight: &mut HashMap<u16, InflightEntry>,
    inflight_gauge: &AtomicUsize,
    timeouts: &mut BTreeMap<Instant, Vec<u16>>,
) -> Option<(u16, Bytes)> {
    if request.payload.len() < 10 {
        let _ = request.response_tx.send(Err(Error::ErrInvalidFrame));
        return None;
    }

    let payload = request.payload;
    let pdu_ref = u16::from_be_bytes([payload[4], payload[5]]);
    let response_tx = request.response_tx;
    let permit = request.permit;
    let timeout = request.timeout;

    if inflight
        .insert(
            pdu_ref,
            InflightEntry {
                tx: response_tx,
                _permit: permit,
            },
        )
        .is_none()
    {
        inflight_gauge.fetch_add(1, Ordering::Relaxed);
    }

    let deadline = Instant::now() + timeout;
    timeouts.entry(deadline).or_default().push(pdu_ref);

    Some((pdu_ref, payload))
}

/// Handle a send failure for an inflight PDU by completing its response channel.
///
/// When the underlying TCP sink returns an error, this helper removes the
/// corresponding inflight entry, notifies the caller with an IO error, and
/// decrements the inflight gauge to keep metrics consistent.
#[inline]
fn handle_send_failure(
    pdu_ref: u16,
    inflight: &mut HashMap<u16, InflightEntry>,
    inflight_gauge: &AtomicUsize,
) {
    if let Some(entry) = inflight.remove(&pdu_ref) {
        let _ = entry.tx.send(Err(Error::ErrUseClosedConnection));
        inflight_gauge.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Process a fully received S7 message and drive reassembly and response routing.
///
/// This helper handles segmentation reassembly, PDU parsing and lookup of inflight
/// requests. Successfully parsed responses are delivered back to the waiting
/// caller, while malformed frames trigger a `FragmentReassemblyDrop` event.
#[inline]
fn handle_incoming_s7_message(
    msg: S7Message,
    negotiated_pdu_len: u16,
    reasm: &mut Option<BytesMut>,
    inflight: &mut HashMap<u16, InflightEntry>,
    inflight_gauge: &AtomicUsize,
    events_tx: &broadcast::Sender<SessionEvent>,
) {
    if let Cotp::D(params) = msg.cotp {
        match msg.app {
            Some(S7AppBody::Segmented(pl)) => {
                if !params.eot {
                    let buf = reasm.get_or_insert_with(|| {
                        let hint = max(pl.len() * 2, negotiated_pdu_len as usize);
                        BytesMut::with_capacity(hint)
                    });
                    buf.extend_from_slice(&pl);
                } else if let Some(mut buf) = reasm.take() {
                    buf.extend_from_slice(&pl);
                    let all = buf.freeze();
                    if let Ok((_r, pdu)) = S7Pdu::parse(&all, &all, &()) {
                        let key = pdu.header.pdu_ref;
                        if let Some(entry) = inflight.remove(&key) {
                            let _ = entry.tx.send(Ok(pdu));
                            inflight_gauge.fetch_sub(1, Ordering::Relaxed);
                        }
                    } else {
                        let _ = events_tx.send(SessionEvent::FragmentReassemblyDrop);
                    }
                } else if let Ok((_r, pdu)) = S7Pdu::parse(&pl, &pl, &()) {
                    let key = pdu.header.pdu_ref;
                    if let Some(entry) = inflight.remove(&key) {
                        let _ = entry.tx.send(Ok(pdu));
                        inflight_gauge.fetch_sub(1, Ordering::Relaxed);
                    }
                } else {
                    let _ = events_tx.send(SessionEvent::FragmentReassemblyDrop);
                }
            }
            Some(S7AppBody::Parsed(pdu)) => {
                let key = pdu.header.pdu_ref;
                if let Some(entry) = inflight.remove(&key) {
                    let _ = entry.tx.send(Ok(pdu));
                    inflight_gauge.fetch_sub(1, Ordering::Relaxed);
                }
            }
            None => {}
        }
    }
}

/// Scan timeout wheel and fail any expired inflight requests.
///
/// This helper walks the `timeouts` map up to `now`, completes each expired
/// inflight entry with a timeout error, updates the inflight gauge and increments
/// the global timeout counter.
#[inline]
fn handle_request_timeouts(
    timeouts: &mut BTreeMap<Instant, Vec<u16>>,
    inflight: &mut HashMap<u16, InflightEntry>,
    inflight_gauge: &AtomicUsize,
    timeouts_total: &AtomicU64,
) {
    let now = Instant::now();
    let expired: Vec<Instant> = timeouts
        .keys()
        .take_while(|d| **d <= now)
        .cloned()
        .collect();

    for dl in expired {
        if let Some(keys) = timeouts.remove(&dl) {
            for key in keys {
                if let Some(entry) = inflight.remove(&key) {
                    let _ = entry.tx.send(Err(Error::ErrRequestTimeout));
                    inflight_gauge.fetch_sub(1, Ordering::Relaxed);
                    timeouts_total.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
}

#[inline]
fn publish_lifecycle(
    events_tx: &broadcast::Sender<SessionEvent>,
    lifecycle_tx: &watch::Sender<SessionLifecycleState>,
    state: SessionLifecycleState,
) {
    let _ = events_tx.send(SessionEvent::LifecycleChanged(state));
    let _ = lifecycle_tx.send(state);
}

#[inline]
async fn poll_next_msg(
    stream: &mut Option<SplitStream<Framed<TcpStream, Codec>>>,
) -> Option<StdResult<S7Message, Error>> {
    if let Some(st) = stream.as_mut() {
        match st.next().await {
            Some(Ok(msg)) => Some(Ok(msg)),
            Some(Err(_e)) => Some(Err(Error::ErrUseClosedConnection)),
            None => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::frame::{
            default_tsap_pair, parse_s7_address, CpuType, S7Area, S7ReturnCode, S7TransportSize,
            S7VarSpec,
        },
        super::planner::S7Planner,
        *,
    };
    use tracing::Level;

    async fn connect_it_session() -> Option<Arc<Session>> {
        let _ = tracing_subscriber::fmt()
            .with_max_level(Level::DEBUG) // 设置测试中希望看到的最高日志级别
            .with_target(false) // 可选：关闭目标模块路径显示，使输出更简洁
            .without_time() // 可选：关闭时间戳，测试输出更清晰
            .try_init();

        let socket_addr = format!("{}:{}", "54.241.198.186", 17260)
            .parse::<SocketAddr>()
            .unwrap();
        let pair = default_tsap_pair(CpuType::S71500, 0, 1).unwrap();
        let (session, ev) = create(Arc::new(SessionConfig {
            socket_addr,
            cpu: CpuType::S71500,
            tsap_src: pair.local.into(),
            tsap_dst: pair.remote.into(),
            connect_timeout: Duration::from_millis(10000), // Very short timeout
            ..Default::default()
        }));
        let _io = ev.spawn();
        if !session.wait_for_active().await {
            panic!("error: !session.wait_for_connection().await");
        }
        Some(session)
    }

    #[tokio::test]
    #[ignore]
    async fn test_address_read() {
        let session = match connect_it_session().await {
            Some(s) => s,
            None => return,
        };
        let start = Instant::now();
        match session
            .read_addresses_typed(&[
                &parse_s7_address("DB2.X2.5").unwrap(),
                &parse_s7_address("DB1.X320.2").unwrap(),
                &parse_s7_address("DB1.X321.2").unwrap(),
                &parse_s7_address("DB1.X321.3").unwrap(),
                &parse_s7_address("DB1.X321.4").unwrap(),
                &parse_s7_address("DB1.X321.5").unwrap(),
                &parse_s7_address("DB1.X320.0").unwrap(),
                &parse_s7_address("DB1.X320.1").unwrap(),
                &parse_s7_address("DB2.X2.4").unwrap(),
                &parse_s7_address("DB2.X2.0").unwrap(),
                &parse_s7_address("DB2.X2.1").unwrap(),
                &parse_s7_address("DB2.X6.4").unwrap(),
                &parse_s7_address("DB2.X6.5").unwrap(),
                &parse_s7_address("DB2.X3.3").unwrap(),
                &parse_s7_address("DB2.X0.4").unwrap(),
                &parse_s7_address("DB2.X0.3").unwrap(),
                &parse_s7_address("DB2.X0.0").unwrap(),
                &parse_s7_address("DB2.X0.1").unwrap(),
                &parse_s7_address("DB2.X0.2").unwrap(),
                &parse_s7_address("DB2.X1.4").unwrap(),
                &parse_s7_address("DB2.X1.5").unwrap(),
                &parse_s7_address("DB2.X1.1").unwrap(),
                &parse_s7_address("DB2.X1.0").unwrap(),
                &parse_s7_address("DB2.X0.5").unwrap(),
                &parse_s7_address("DB2.X0.6").unwrap(),
                &parse_s7_address("DB2.X0.7").unwrap(),
                &parse_s7_address("DB2.X1.6").unwrap(),
                &parse_s7_address("DB2.X1.7").unwrap(),
                // Additional real-type points in DB1
                &parse_s7_address("DB1.R72").unwrap(),
                &parse_s7_address("DB1.R64").unwrap(),
                &parse_s7_address("DB1.R112").unwrap(),
                &parse_s7_address("DB1.R68").unwrap(),
                &parse_s7_address("DB1.R108").unwrap(),
                &parse_s7_address("DB1.R100").unwrap(),
                &parse_s7_address("DB1.R104").unwrap(),
                &parse_s7_address("DB1.R116").unwrap(),
                &parse_s7_address("DB1.R120").unwrap(),
                &parse_s7_address("DB1.R48").unwrap(),
                &parse_s7_address("DB1.R56").unwrap(),
                &parse_s7_address("DB1.R52").unwrap(),
                &parse_s7_address("DB1.R60").unwrap(),
                &parse_s7_address("DB1.R176").unwrap(),
                &parse_s7_address("DB1.R172").unwrap(),
                &parse_s7_address("DB1.R16").unwrap(),
                &parse_s7_address("DB1.R12").unwrap(),
                &parse_s7_address("DB1.R0").unwrap(),
                &parse_s7_address("DB1.R4").unwrap(),
                &parse_s7_address("DB1.R8").unwrap(),
                &parse_s7_address("DB1.R156").unwrap(),
                &parse_s7_address("DB1.R136").unwrap(),
                &parse_s7_address("DB1.R140").unwrap(),
                &parse_s7_address("DB1.R144").unwrap(),
                &parse_s7_address("DB1.R148").unwrap(),
                &parse_s7_address("DB1.R152").unwrap(),
                &parse_s7_address("DB1.R124").unwrap(),
                &parse_s7_address("DB1.R128").unwrap(),
                &parse_s7_address("DB1.R132").unwrap(),
                &parse_s7_address("DB1.R232").unwrap(),
                &parse_s7_address("DB1.R228").unwrap(),
                &parse_s7_address("DB1.R36").unwrap(),
                &parse_s7_address("DB1.R32").unwrap(),
                &parse_s7_address("DB1.R20").unwrap(),
                &parse_s7_address("DB1.R24").unwrap(),
                &parse_s7_address("DB1.R28").unwrap(),
                &parse_s7_address("DB1.R212").unwrap(),
                &parse_s7_address("DB1.R192").unwrap(),
                &parse_s7_address("DB1.R196").unwrap(),
                &parse_s7_address("DB1.R200").unwrap(),
                &parse_s7_address("DB1.R204").unwrap(),
                &parse_s7_address("DB1.R208").unwrap(),
                &parse_s7_address("DB1.R180").unwrap(),
                &parse_s7_address("DB1.R184").unwrap(),
                &parse_s7_address("DB1.R188").unwrap(),
                &parse_s7_address("DB1.R304").unwrap(),
                &parse_s7_address("DB1.R308").unwrap(),
                &parse_s7_address("DB1.R312").unwrap(),
                &parse_s7_address("DB1.R292").unwrap(),
                &parse_s7_address("DB1.R296").unwrap(),
                &parse_s7_address("DB1.R300").unwrap(),
            ])
            .await
        {
            Ok(m) => {
                tracing::info!("time: {:?}", start.elapsed());
                for ele in m {
                    tracing::info!("ele: {:?}", ele);
                }
            }
            Err(e) => {
                panic!("error: {:?}", e);
            }
        }
    }

    /// Integration test: adjacent items coalescing with real PLC.
    ///
    /// Env controls (optional unless host provided):
    /// - NG_S7_IT_DB (default 1)
    /// - NG_S7_IT_BASE (default 0)
    /// - NG_S7_IT_ITEM_SIZE (default 16)
    /// - NG_S7_IT_ITEM_COUNT (default 3)
    #[tokio::test]
    #[ignore]
    async fn it_read_coalesce_adjacent() {
        let session = match connect_it_session().await {
            Some(s) => s,
            None => return,
        };

        let db = 1;
        let base = 1;
        let item_size = 16;
        let item_count: usize = 50;

        let mut specs: Vec<S7VarSpec> = Vec::with_capacity(item_count);
        let mut cursor = base;
        for _ in 0..item_count {
            specs.push(S7VarSpec {
                transport_size: S7TransportSize::Byte,
                count: item_size,
                db_number: db,
                area: S7Area::DB,
                byte_address: cursor,
                bit_index: 0,
            });
            cursor = cursor.saturating_add(item_size as u32);
        }

        // Compute expected plan and batch count under negotiated PDU
        let plan = S7Planner::plan_read(&PlannerConfig::new(960), &specs);
        assert!(!plan.batches.is_empty());

        let merged = match session.read_var(&specs).await {
            Ok(m) => m,
            Err(e) => {
                // Cannot proceed on error (permissions/DB size). Skip.
                panic!("error: {:?}", e);
            }
        };

        if merged.len() != item_count {
            panic!("error: merged.len() != item_count");
        }
        for it in merged.iter() {
            if !matches!(it.rc, S7ReturnCode::Success) {
                panic!("error: it.rc != S7ReturnCode::Success");
            }
        }
        for it in merged.iter() {
            if it.data.len() != item_size as usize {
                panic!("error: it.data.len() != item_size");
            }
        }
    }

    /// Integration test: large single item read forces split across multiple batches.
    ///
    /// Env controls (optional unless host provided):
    /// - NG_S7_IT_DB (default 1)
    /// - NG_S7_IT_BASE (default 0)
    /// - NG_S7_IT_LARGE_COUNT (default computed from negotiated PDU)
    #[tokio::test]
    #[ignore]
    async fn it_read_split_large_single_item() {
        let session = match connect_it_session().await {
            Some(s) => s,
            None => return,
        };

        let db = 1;
        let base = 0;

        // Choose a count that guarantees split under negotiated PDU length
        let default_large = 960_usize.saturating_mul(2).min(u16::MAX as usize) as u16;
        let count = default_large;

        let spec = S7VarSpec {
            transport_size: S7TransportSize::Byte,
            count,
            db_number: db,
            area: S7Area::DB,
            byte_address: base,
            bit_index: 0,
        };

        // Verify planner requires split
        let plan = S7Planner::plan_read(&PlannerConfig::new(960), &[spec]);
        if plan.batches.len() < 2 {
            panic!("error: plan.batches.len() < 2");
        }

        let merged = match session.read_var(&[spec]).await {
            Ok(m) => m,
            Err(_e) => panic!("error: session.read_var(&[spec]).await"),
        };
        if merged.is_empty() {
            panic!("error: merged.is_empty()");
        }
        let got = merged[0].data.len();
        // We cannot assert exact length due to PLC DB size uncertainty; ensure non-zero result.
        if got == 0 {
            panic!("error: got == 0");
        }
    }
}
