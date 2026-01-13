mod state;
pub use state::{SessionConfig, SessionEvent, SessionLifecycleState};

use super::{
    codec::Codec,
    error::{Error as McError, Result as McResult},
    frame::{
        addr::McLogicalAddress,
        builder::build_3e_req_header,
        builder::{
            build_device_batch_read_message, build_device_batch_write_message,
            build_device_random_read_message, build_device_random_write_message,
        },
        command::McCommandKind,
        device::McDeviceType,
        header::McHeader,
        pdu::{McBody, McPdu, McRequestBody},
        McAppBody, McMessage,
    },
    planner::{
        merge_read_raw, plan_random_read_batches, plan_random_write_batches, plan_read_batches,
        plan_write_batches, McReadItemRaw, PlannerConfig, PointReadSpec, RandomReadEntry,
        RandomWriteEntry, ReadBatchResult, WriteEntry,
    },
};
use arc_swap::ArcSwapOption;
use bytes::{BufMut, Bytes, BytesMut};
use futures::{SinkExt, Stream, StreamExt};
use std::{io, sync::Arc, time::Duration};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore},
    time,
};
use tokio_util::{codec::Framed, sync::CancellationToken};

/// Request message for MC session.
#[allow(unused)]
#[derive(Debug)]
pub struct SessionRequest {
    /// Fully constructed MC request message (header + payload).
    pub message: McMessage,
    /// Operation timeout.
    pub timeout: Duration,
    /// Response channel that will receive the decoded `McMessage`.
    pub response_tx: oneshot::Sender<std::io::Result<McMessage>>,
    /// Concurrency permit carried across the lifetime of this request.
    pub permit: OwnedSemaphorePermit,
}

/// MC session runtime state and IO entry point.
///
/// This struct intentionally mirrors the responsibilities of the S7 `Session`
/// type but with a simplified transport stack: MC is directly framed by
/// `McCodec` over `TcpStream`/`UdpSocket` without additional layers.
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
    /// Semaphore for controlling concurrent requests
    request_semaphore: Arc<Semaphore>,
}

/// Internal entry mapping a single logical bit point into a batch payload.
#[derive(Debug, Clone)]
struct BitBatchEntry {
    /// Original point index in caller-provided list.
    index: usize,
    /// Offset (in bits) from the head bit of this batch.
    offset_bits: u16,
}

/// Internal bit read batch specification for MC bit devices.
#[derive(Debug, Clone)]
struct BitReadBatchSpec {
    /// Device type for this batch (bit-addressed).
    device: McDeviceType,
    /// Encoded device code used on the wire.
    device_code: u16,
    /// Head logical bit address of the batch.
    head: u32,
    /// Total logical points (bits) within this batch.
    points: u16,
    /// Entries describing individual points within the batch.
    entries: Vec<BitBatchEntry>,
}

/// Raw multi-address read specification used by the low-level Session API.
///
/// Each item describes a single logical address; higher layers are
/// responsible for attaching typing semantics (for example, `DataType`).
#[derive(Debug, Clone)]
pub struct MultiAddressReadSpec {
    /// Caller-supplied index to preserve input ordering in results.
    pub index: usize,
    /// Logical MC address to read from.
    pub addr: McLogicalAddress,
}

/// Raw multi-address write specification used by the low-level Session API.
///
/// Values are expected to be pre-encoded into MC byte representation by
/// higher layers (such as the `typed_api` facade).
#[derive(Debug, Clone)]
pub struct MultiAddressWriteSpec {
    /// Logical MC address to write to.
    pub addr: McLogicalAddress,
    /// Pre-encoded MC payload bytes for this logical address.
    pub data: Bytes,
}

/// Session event loop facade providing a stream of `SessionEvent`
#[derive(Debug)]
pub struct SessionEventLoop {
    session: Arc<Session>,
    inner_cancel: CancellationToken,
    config: Arc<SessionConfig>,
    pre_connected: Option<TcpStream>,
}

#[allow(unused)]
impl SessionEventLoop {
    /// Enter and get a stream of session events
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
        futures::pin_mut!(s);
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

#[allow(unused)]
impl Session {
    /// Create a new MC session (returns `Arc<Self>`)
    pub fn new(config: Arc<SessionConfig>, cancel: CancellationToken) -> Arc<Self> {
        let request_tx: Arc<ArcSwapOption<mpsc::Sender<SessionRequest>>> =
            Arc::new(ArcSwapOption::from(None));
        let (events_tx, _rx_unused) = broadcast::channel::<SessionEvent>(1024);
        let (lifecycle_tx, lifecycle_rx) = watch::channel(SessionLifecycleState::Idle);

        // For now we enforce strict request/response semantics at the protocol
        // layer and derive the semaphore permits from `max_concurrent_requests`, which is
        // configured at channel level with a safe default of 1.
        let permits = config.max_concurrent_requests.max(1);
        let request_semaphore = Arc::new(Semaphore::new(permits));

        Arc::new(Session {
            config,
            request_tx,
            cancel: cancel.clone(),
            events_tx,
            lifecycle_tx,
            lifecycle_rx,
            request_semaphore,
        })
    }

    /// Subscribe to session events
    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.events_tx.subscribe()
    }

    /// Get a lifecycle watch receiver clone
    pub fn lifecycle(&self) -> watch::Receiver<SessionLifecycleState> {
        self.lifecycle_rx.clone()
    }

    /// Get current lifecycle state
    #[inline]
    pub fn current_lifecycle(&self) -> SessionLifecycleState {
        *self.lifecycle_rx.borrow()
    }

    /// Whether lifecycle is currently Active (fast path, no await)
    #[inline]
    pub fn is_active(&self) -> bool {
        matches!(self.current_lifecycle(), SessionLifecycleState::Active)
    }

    /// Graceful shutdown
    pub async fn shutdown(&self) {
        // Trigger cooperative cancellation for the session state machine
        self.cancel.cancel();
        // Close semaphore to wake any pending acquirers
        self.request_semaphore.close();
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

    /// Wait until the transport is connected and session enters at least Active.
    /// Returns false if lifecycle goes to Closed/Failed.
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

    /// Submit a raw MC request frame and wait for a single `McMessage` response.
    ///
    /// This method enforces strict request/response semantics and delegates
    /// the actual I/O to the background connection task, which uses
    /// `Framed<TcpStream, Codec>` for encode/decode.
    pub async fn request_message(&self, message: McMessage) -> std::io::Result<McMessage> {
        // Acquire one concurrency slot
        let permit = match self.request_semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "MC session semaphore closed",
                ))
            }
        };

        // Fast path: ensure we have a request sender
        let sender = self.request_tx.load_full().ok_or(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "MC session not active",
        ))?;

        let (tx, rx) = oneshot::channel();
        let req = SessionRequest {
            message,
            timeout: self.config.read_timeout,
            response_tx: tx,
            permit,
        };

        if sender.send(req).await.is_err() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "MC session request channel closed",
            ));
        }

        match rx.await {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MC session request timeout",
            )),
        }
    }

    /// Raw batch read helper for MC logical items.
    ///
    /// This API accepts a list of logical point specifications, uses the MC
    /// planner to build contiguous batches and executes each batch as a single
    /// MC request. Successful values are decoded via `McValueCodec` and
    /// returned as `McReadItemTyped` results, aligned with the input order
    /// through the `index` field on `PointReadSpec`.
    pub async fn read_points_raw(
        &self,
        planner_cfg: &PlannerConfig,
        specs: Vec<PointReadSpec>,
    ) -> McResult<Vec<McReadItemRaw>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Split into bit devices and non-bit devices so that we can handle
        // bit-addressed points with a dedicated batching and decode path.
        let mut bit_specs: Vec<PointReadSpec> = Vec::new();
        let mut word_specs: Vec<PointReadSpec> = Vec::new();
        for spec in specs.into_iter() {
            if spec.addr.device.is_bit() {
                bit_specs.push(spec);
            } else {
                word_specs.push(spec);
            }
        }

        // Determine maximum index to size result vector; indices are provided
        // by the caller in `PointReadSpec::index`.
        let mut max_index = 0usize;
        for s in word_specs.iter().chain(bit_specs.iter()) {
            if s.index > max_index {
                max_index = s.index;
            }
        }
        let total = max_index + 1;
        let mut results: Vec<McReadItemRaw> = (0..total)
            .map(|idx| McReadItemRaw {
                index: idx,
                end_code: 0,
                payload: None,
            })
            .collect();

        // Handle non-bit devices using the existing word/dword planner and
        // raw merge helper.
        if !word_specs.is_empty() {
            let batches = plan_read_batches(planner_cfg, word_specs);
            let mut batch_results: Vec<ReadBatchResult> = Vec::with_capacity(batches.len());

            for batch in batches.iter() {
                let monitoring_ms = self.config.read_timeout.as_millis().max(250) as u32;
                let message = build_device_batch_read_message(
                    self.config.series,
                    self.config.frame_variant,
                    monitoring_ms,
                    batch.head,
                    batch.points,
                    batch.device_code,
                );

                let resp = match self.request_message(message).await {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        return Err(McError::RequestTimeout);
                    }
                    Err(e) => return Err(McError::Io(e)),
                };

                let (end_code, payload) = match resp {
                    McMessage {
                        header: McHeader::Ack3E(h),
                        body: Some(McAppBody::Raw(b)),
                    } => (h.end_code, b),
                    _ => {
                        return Err(McError::ProtocolViolation {
                            context:
                                "unexpected MC response header for read_points_typed (word/dword)",
                        });
                    }
                };

                batch_results.push(ReadBatchResult {
                    end_code,
                    payload: Some(payload),
                });
            }

            let merged = merge_read_raw(&batches, &batch_results);
            for item in merged.into_iter() {
                let idx = item.index;
                if idx < results.len() {
                    results[idx] = item;
                }
            }
        }

        // Handle bit-addressed devices with a specialised bit batching and
        // raw bit payload path.
        if !bit_specs.is_empty() {
            let bit_items = self.read_points_raw_bits(planner_cfg, bit_specs).await?;
            for item in bit_items.into_iter() {
                let idx = item.index;
                if idx < results.len() {
                    results[idx] = item;
                }
            }
        }

        Ok(results)
    }

    /// Raw batch read helper specialised for MC bit devices.
    ///
    /// This helper:
    /// - Packs bit-addressed points into contiguous batches under planner
    ///   capacity constraints;
    /// - Executes each batch as a single MC request;
    /// - Decodes the nibble-encoded bit payload into boolean JSON values;
    /// - Returns a list of `McReadItemTyped` aligned by the original `index`.
    async fn read_points_raw_bits(
        &self,
        planner_cfg: &PlannerConfig,
        mut specs: Vec<PointReadSpec>,
    ) -> McResult<Vec<McReadItemRaw>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Ensure all specs target bit devices; type semantics are handled
        // in higher layers (for example, `typed_api`).
        for s in specs.iter() {
            if !s.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "read_points_typed_bits expects only bit-addressed devices",
                });
            }
        }

        // Sort by device and head to build contiguous batches in bit space.
        specs.sort_by_key(|s| (s.addr.device as u8, s.addr.head));

        let mut batches: Vec<BitReadBatchSpec> = Vec::new();
        let mut cur: Option<BitReadBatchSpec> = None;

        for spec in specs.into_iter() {
            let len_bits = spec.word_len;

            match &mut cur {
                None => {
                    let mut entries = Vec::with_capacity(4);
                    entries.push(BitBatchEntry {
                        index: spec.index,
                        offset_bits: 0,
                    });
                    cur = Some(BitReadBatchSpec {
                        device: spec.addr.device,
                        device_code: spec.device_code,
                        head: spec.addr.head,
                        points: len_bits,
                        entries,
                    });
                }
                Some(batch) => {
                    if batch.device != spec.addr.device || batch.device_code != spec.device_code {
                        batches.push(BitReadBatchSpec {
                            device: batch.device,
                            device_code: batch.device_code,
                            head: batch.head,
                            points: batch.points,
                            entries: batch.entries.clone(),
                        });
                        let mut entries = Vec::with_capacity(4);
                        entries.push(BitBatchEntry {
                            index: spec.index,
                            offset_bits: 0,
                        });
                        *batch = BitReadBatchSpec {
                            device: spec.addr.device,
                            device_code: spec.device_code,
                            head: spec.addr.head,
                            points: len_bits,
                            entries,
                        };
                        continue;
                    }

                    let expected_head = batch.head.saturating_add(batch.points as u32);
                    let contiguous = spec.addr.head == expected_head;

                    let next_points = batch.points.saturating_add(len_bits);
                    let within_capacity =
                        next_points as u32 <= planner_cfg.max_points_per_batch as u32;

                    if !contiguous || !within_capacity {
                        batches.push(BitReadBatchSpec {
                            device: batch.device,
                            device_code: batch.device_code,
                            head: batch.head,
                            points: batch.points,
                            entries: batch.entries.clone(),
                        });
                        let mut entries = Vec::with_capacity(4);
                        entries.push(BitBatchEntry {
                            index: spec.index,
                            offset_bits: 0,
                        });
                        *batch = BitReadBatchSpec {
                            device: spec.addr.device,
                            device_code: spec.device_code,
                            head: spec.addr.head,
                            points: len_bits,
                            entries,
                        };
                    } else {
                        let offset_bits = batch.points;
                        batch.entries.push(BitBatchEntry {
                            index: spec.index,
                            offset_bits,
                        });
                        batch.points = next_points;
                    }
                }
            }
        }

        if let Some(b) = cur {
            batches.push(b);
        }

        let mut out: Vec<McReadItemRaw> = Vec::new();

        for batch in batches.into_iter() {
            let monitoring_ms = self.config.read_timeout.as_millis().max(250) as u32;
            let message = build_device_batch_read_message(
                self.config.series,
                self.config.frame_variant,
                monitoring_ms,
                batch.head,
                batch.points,
                batch.device_code,
            );

            let resp = match self.request_message(message).await {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(McError::RequestTimeout);
                }
                Err(e) => return Err(McError::Io(e)),
            };

            let (end_code, payload) = match resp {
                McMessage {
                    header: McHeader::Ack3E(h),
                    body: Some(McAppBody::Raw(b)),
                } => (h.end_code, b),
                _ => {
                    return Err(McError::ProtocolViolation {
                        context: "unexpected MC response header for read_points_typed_bits",
                    });
                }
            };

            if end_code != 0 {
                for entry in batch.entries.iter() {
                    out.push(McReadItemRaw {
                        index: entry.index,
                        end_code,
                        payload: None,
                    });
                }
                continue;
            }

            let bits = decode_mc_bits(payload.as_ref(), batch.points);

            for entry in batch.entries.iter() {
                let off = entry.offset_bits as usize;
                if off >= bits.len() {
                    out.push(McReadItemRaw {
                        index: entry.index,
                        end_code: 0,
                        payload: None,
                    });
                    continue;
                }
                // Represent each bit as a single-byte payload (0/1) so that
                // higher layers can reuse generic value decoders.
                let b: u8 = if bits[off] { 1 } else { 0 };
                out.push(McReadItemRaw {
                    index: entry.index,
                    end_code: 0,
                    payload: Some(Bytes::from(vec![b])),
                });
            }
        }

        Ok(out)
    }

    /// Typed batch write helper specialised for MC bit devices.
    ///
    /// This helper:
    /// - Packs bit-addressed write entries into contiguous batches under the
    ///   planner capacity constraints (using max_points_per_batch as bit limit);
    /// - Encodes boolean values into MC nibble-based bit representation;
    /// - Executes each batch as a single MC write request and fails fast on any
    ///   protocol-level error.
    async fn write_points_typed_bits(
        &self,
        planner_cfg: &PlannerConfig,
        mut entries: Vec<WriteEntry>,
    ) -> McResult<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Ensure all entries target bit devices and carry a single boolean
        // value encoded as one byte (0 or 1).
        for e in entries.iter() {
            if !e.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "write_points_typed_bits expects only bit-addressed devices",
                });
            }
            if e.data.is_empty() {
                return Err(McError::Encode {
                    context: "MC bit write entry has empty payload",
                });
            }
        }

        // Sort by (device, head) to build contiguous batches in bit space.
        entries.sort_by_key(|e| (e.addr.device as u8, e.addr.head));

        #[derive(Debug, Clone)]
        struct BitWriteBatch {
            device: McDeviceType,
            device_code: u16,
            head: u32,
            points: u16,
            /// Logical write entries in this batch, ordered by head address.
            entries: Vec<WriteEntry>,
        }

        let mut batches: Vec<BitWriteBatch> = Vec::new();
        let mut cur: Option<BitWriteBatch> = None;

        for e in entries.into_iter() {
            let len_bits = e.word_len;

            match &mut cur {
                None => {
                    cur = Some(BitWriteBatch {
                        device: e.addr.device,
                        device_code: e.device_code,
                        head: e.addr.head,
                        points: len_bits,
                        entries: vec![e],
                    });
                }
                Some(batch) => {
                    if batch.device != e.addr.device || batch.device_code != e.device_code {
                        batches.push(BitWriteBatch {
                            device: batch.device,
                            device_code: batch.device_code,
                            head: batch.head,
                            points: batch.points,
                            entries: batch.entries.clone(),
                        });
                        *batch = BitWriteBatch {
                            device: e.addr.device,
                            device_code: e.device_code,
                            head: e.addr.head,
                            points: len_bits,
                            entries: vec![e],
                        };
                        continue;
                    }

                    let expected_head = batch.head.saturating_add(batch.points as u32);
                    let contiguous = e.addr.head == expected_head;
                    let next_points = batch.points.saturating_add(len_bits);
                    let within_capacity =
                        next_points as u32 <= planner_cfg.max_points_per_batch as u32;

                    if !contiguous || !within_capacity {
                        batches.push(BitWriteBatch {
                            device: batch.device,
                            device_code: batch.device_code,
                            head: batch.head,
                            points: batch.points,
                            entries: batch.entries.clone(),
                        });
                        *batch = BitWriteBatch {
                            device: e.addr.device,
                            device_code: e.device_code,
                            head: e.addr.head,
                            points: len_bits,
                            entries: vec![e],
                        };
                    } else {
                        batch.points = next_points;
                        batch.entries.push(e);
                    }
                }
            }
        }

        if let Some(b) = cur {
            batches.push(b);
        }

        for batch in batches.into_iter() {
            // Build a flat boolean array ordered by address.
            let mut bools = Vec::with_capacity(batch.points as usize);
            for e in batch.entries.iter() {
                // Each write entry is expected to represent a single boolean
                // encoded as 0/1 in the first byte.
                let v = e.data.first().map(|f| *f != 0).unwrap_or(false);
                bools.push(v);
            }

            let data = encode_mc_bits(&bools);

            let pdu = McPdu::new(
                self.config.series,
                McCommandKind::DeviceAccessBatchWriteUnits,
                McBody::Request(McRequestBody::DeviceAccessBatchWriteUnits {
                    head: batch.head,
                    points: batch.points,
                    device_code: batch.device_code,
                    data,
                }),
            );

            let monitoring_ms = self.config.write_timeout.as_millis().max(250) as u32;
            let header = build_3e_req_header(self.config.frame_variant, monitoring_ms);
            let message = McMessage {
                header: McHeader::Req3E(header),
                body: Some(McAppBody::Parsed(pdu)),
            };

            let resp = match self.request_message(message).await {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(McError::RequestTimeout);
                }
                Err(e) => return Err(McError::Io(e)),
            };

            let end_code = match resp.header {
                McHeader::Ack3E(h) => h.end_code,
                _ => {
                    return Err(McError::ProtocolViolation {
                        context: "unexpected MC response header for write_points_typed_bits",
                    });
                }
            };

            if end_code != 0 {
                return Err(McError::EndCode { code: end_code });
            }
        }

        Ok(())
    }

    /// Raw batch write helper for MC logical items.
    ///
    /// This API accepts a list of logical write entries, uses the MC planner to
    /// build contiguous batches and executes each batch as a single MC write
    /// request. Any protocol-level failure (non-zero end_code or unexpected
    /// header) aborts the whole action and returns an error.
    pub async fn write_points_raw(
        &self,
        planner_cfg: &PlannerConfig,
        entries: Vec<WriteEntry>,
    ) -> McResult<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Split into bit and non-bit entries so that bit-addressed writes can
        // use a dedicated packing path.
        let mut bit_entries: Vec<WriteEntry> = Vec::new();
        let mut word_entries: Vec<WriteEntry> = Vec::new();
        for e in entries.into_iter() {
            if e.addr.device.is_bit() {
                bit_entries.push(e);
            } else {
                word_entries.push(e);
            }
        }

        // Handle non-bit entries via existing planner and write path.
        if !word_entries.is_empty() {
            let batches = plan_write_batches(planner_cfg, word_entries);

            for batch in batches.into_iter() {
                let monitoring_ms = self.config.write_timeout.as_millis().max(250) as u32;
                let message = build_device_batch_write_message(
                    self.config.series,
                    self.config.frame_variant,
                    monitoring_ms,
                    batch.head,
                    batch.points,
                    batch.device_code,
                    batch.data.freeze(),
                );

                let resp = match self.request_message(message).await {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                        return Err(McError::RequestTimeout);
                    }
                    Err(e) => return Err(McError::Io(e)),
                };

                let end_code = match resp.header {
                    McHeader::Ack3E(h) => h.end_code,
                    _ => {
                        return Err(McError::ProtocolViolation {
                            context: "unexpected MC response header for write_points_typed",
                        });
                    }
                };

                if end_code != 0 {
                    return Err(McError::EndCode { code: end_code });
                }
            }
        }

        // Handle bit-addressed entries with a specialised bit packing and
        // write path.
        if !bit_entries.is_empty() {
            self.write_points_typed_bits(planner_cfg, bit_entries)
                .await?;
        }

        Ok(())
    }

    /// Random-read helper for MC logical items using word/dword units (raw).
    ///
    /// This API mirrors the Java `McNetwork.readDeviceRandomInWord` behaviour:
    /// it accepts a list of random-read entries, splits them into word/dword
    /// groups, plans batches under the series limits and executes MC
    /// `DeviceAccessRandomReadUnits` commands to fetch the data.
    pub async fn read_random_points_raw(
        &self,
        mut entries: Vec<RandomReadEntry>,
    ) -> McResult<Vec<McReadItemRaw>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }

        // Validate and split into word/dword groups based on device type.
        let mut word_entries = Vec::new();
        let mut dword_entries = Vec::new();
        let mut max_index = 0usize;

        for e in entries.drain(..) {
            if e.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "read_random_points_typed does not support bit devices",
                });
            }
            if !e.addr.device.is_word() && !e.addr.device.is_dword() {
                return Err(McError::ProtocolViolation {
                    context: "read_random_points_typed expects word or dword devices",
                });
            }
            max_index = max_index.max(e.index);
            if e.addr.device.is_dword() {
                dword_entries.push(e);
            } else {
                word_entries.push(e);
            }
        }

        // Preallocate result slots aligned with entry indices.
        let total = max_index + 1;
        let mut results: Vec<McReadItemRaw> = (0..total)
            .map(|idx| McReadItemRaw {
                index: idx,
                end_code: 0,
                payload: None,
            })
            .collect();

        // Plan random-read batches under series-specific limits.
        let batches = plan_random_read_batches(self.config.series, word_entries, dword_entries);

        for batch in batches.into_iter() {
            let monitoring_ms = self.config.read_timeout.as_millis().max(250) as u32;
            let word_addrs: Vec<(u32, u16)> = batch
                .word_entries
                .iter()
                .map(|e| (e.addr.head, e.device_code))
                .collect();
            let dword_addrs: Vec<(u32, u16)> = batch
                .dword_entries
                .iter()
                .map(|e| (e.addr.head, e.device_code))
                .collect();
            let message = build_device_random_read_message(
                self.config.series,
                self.config.frame_variant,
                monitoring_ms,
                &word_addrs,
                &dword_addrs,
            );

            let resp = match self.request_message(message).await {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(McError::RequestTimeout);
                }
                Err(e) => return Err(McError::Io(e)),
            };

            let (end_code, payload) = match resp {
                McMessage {
                    header: McHeader::Ack3E(h),
                    body: Some(McAppBody::Raw(b)),
                } => (h.end_code, b),
                _ => {
                    return Err(McError::ProtocolViolation {
                        context: "unexpected MC response header for read_random_points_typed",
                    });
                }
            };

            if end_code != 0 {
                for e in batch.word_entries.iter().chain(batch.dword_entries.iter()) {
                    if e.index < results.len() {
                        results[e.index].end_code = end_code;
                        results[e.index].payload = None;
                    }
                }
                continue;
            }

            let mut offset = 0usize;
            let word_size = 2usize;
            let dword_size = 4usize;

            // Attach word entries (2 bytes each) as raw payload slices.
            for e in batch.word_entries.iter() {
                if offset + word_size > payload.len() {
                    return Err(McError::ProtocolViolation {
                        context: "MC random read payload too short for word entries",
                    });
                }
                let slice = payload.slice(offset..offset + word_size);
                offset += word_size;

                if e.index >= results.len() {
                    continue;
                }

                results[e.index].end_code = 0;
                results[e.index].payload = Some(slice);
            }

            // Attach dword entries (4 bytes each) as raw payload slices.
            for e in batch.dword_entries.iter() {
                if offset + dword_size > payload.len() {
                    return Err(McError::ProtocolViolation {
                        context: "MC random read payload too short for dword entries",
                    });
                }
                let slice = payload.slice(offset..offset + dword_size);
                offset += dword_size;

                if e.index >= results.len() {
                    continue;
                }

                results[e.index].end_code = 0;
                results[e.index].payload = Some(slice);
            }
        }

        Ok(results)
    }

    /// Random-write helper for MC logical items using word/dword units (raw).
    ///
    /// This API mirrors the Java `McNetwork.writeDeviceRandomInWord` behaviour:
    /// it accepts a list of random-write entries, splits them into word/dword
    /// groups, plans batches under the series limits and executes MC
    /// `DeviceAccessRandomWriteUnits` commands to perform the writes.
    pub async fn write_random_points_raw(
        &self,
        mut entries: Vec<RandomWriteEntry>,
    ) -> McResult<()> {
        if entries.is_empty() {
            return Ok(());
        }

        // Validate and split into word/dword groups based on device type.
        let mut word_entries = Vec::new();
        let mut dword_entries = Vec::new();

        for e in entries.drain(..) {
            if e.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "write_random_points_typed does not support bit devices",
                });
            }
            if !e.addr.device.is_word() && !e.addr.device.is_dword() {
                return Err(McError::ProtocolViolation {
                    context: "write_random_points_typed expects word or dword devices",
                });
            }

            if e.addr.device.is_dword() {
                dword_entries.push(e);
            } else {
                word_entries.push(e);
            }
        }

        let batches = plan_random_write_batches(self.config.series, word_entries, dword_entries);

        for batch in batches.into_iter() {
            let monitoring_ms = self.config.write_timeout.as_millis().max(250) as u32;
            let word_items: Vec<(u32, u16, Bytes)> = batch
                .word_entries
                .iter()
                .map(|e| (e.addr.head, e.device_code, e.data.clone()))
                .collect();
            let dword_items: Vec<(u32, u16, Bytes)> = batch
                .dword_entries
                .iter()
                .map(|e| (e.addr.head, e.device_code, e.data.clone()))
                .collect();
            let message = build_device_random_write_message(
                self.config.series,
                self.config.frame_variant,
                monitoring_ms,
                &word_items,
                &dword_items,
            );

            let resp = match self.request_message(message).await {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                    return Err(McError::RequestTimeout);
                }
                Err(e) => return Err(McError::Io(e)),
            };

            let end_code = match resp.header {
                McHeader::Ack3E(h) => h.end_code,
                _ => {
                    return Err(McError::ProtocolViolation {
                        context: "unexpected MC response header for write_random_points_typed",
                    });
                }
            };

            if end_code != 0 {
                return Err(McError::EndCode { code: end_code });
            }
        }

        Ok(())
    }

    /// High-level multi-address read API built on top of MC random-read units
    /// returning raw payload slices.
    ///
    /// This API:
    /// - Infers word/dword grouping from `McLogicalAddress.device`;
    /// - Builds `RandomReadEntry` lists and delegates to
    ///   `Session::read_random_points_raw`;
    /// - Returns `McReadItemRaw` results aligned with the caller-provided
    ///   `index` field.
    pub async fn read_multi_address_raw(
        &self,
        mut specs: Vec<MultiAddressReadSpec>,
    ) -> McResult<Vec<McReadItemRaw>> {
        if specs.is_empty() {
            return Ok(Vec::new());
        }

        // Build RandomReadEntry list, validating device capabilities.
        let mut entries: Vec<RandomReadEntry> = Vec::with_capacity(specs.len());
        for spec in specs.drain(..) {
            if spec.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "read_multi_address_typed does not support bit devices",
                });
            }
            if !spec.addr.device.is_word() && !spec.addr.device.is_dword() {
                return Err(McError::ProtocolViolation {
                    context: "read_multi_address_typed expects word or dword devices",
                });
            }
            let device_code =
                spec.addr
                    .device
                    .device_code_3e()
                    .ok_or(McError::ProtocolViolation {
                        context: "unsupported MC device type for random read",
                    })?;

            entries.push(RandomReadEntry {
                index: spec.index,
                addr: spec.addr,
                device_code,
            });
        }

        self.read_random_points_raw(entries).await
    }

    /// High-level multi-address write API built on top of MC random-write units
    /// using pre-encoded payloads.
    ///
    /// This API:
    /// - Infers word/dword grouping from `McLogicalAddress.device`;
    /// - Builds `RandomWriteEntry` lists and delegates to
    ///   `Session::write_random_points_raw`.
    pub async fn write_multi_address_raw(
        &self,
        mut specs: Vec<MultiAddressWriteSpec>,
    ) -> McResult<()> {
        if specs.is_empty() {
            return Ok(());
        }

        let mut entries: Vec<RandomWriteEntry> = Vec::with_capacity(specs.len());
        for spec in specs.drain(..) {
            if spec.addr.device.is_bit() {
                return Err(McError::ProtocolViolation {
                    context: "write_multi_address_typed does not support bit devices",
                });
            }
            if !spec.addr.device.is_word() && !spec.addr.device.is_dword() {
                return Err(McError::ProtocolViolation {
                    context: "write_multi_address_typed expects word or dword devices",
                });
            }

            let device_code =
                spec.addr
                    .device
                    .device_code_3e()
                    .ok_or(McError::ProtocolViolation {
                        context: "unsupported MC device type for random write",
                    })?;

            entries.push(RandomWriteEntry {
                addr: spec.addr,
                device_code,
                word_len: 1,
                data: spec.data,
            });
        }

        self.write_random_points_raw(entries).await
    }
}

/// Create a new MC session and event loop (S7-aligned signature)
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

/// Create a new MC session and event loop with a pre-connected TcpStream (S7-aligned)
#[allow(unused)]
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

/// Decode MC bit payload into a sequence of booleans using the same nibble
/// encoding as the Java reference implementation:
///
/// - High nibble 0x1? represents the first bit in the pair;
/// - Low nibble 0x?1 represents the second bit in the pair.
fn decode_mc_bits(payload: &[u8], points: u16) -> Vec<bool> {
    let mut res = Vec::with_capacity(points as usize);
    for &b in payload.iter() {
        let hi = (b & 0xF0) == 0x10;
        let lo = (b & 0x0F) == 0x01;
        if res.len() < points as usize {
            res.push(hi);
        }
        if res.len() < points as usize {
            res.push(lo);
        }
        if res.len() >= points as usize {
            break;
        }
    }
    res
}

/// Encode a slice of booleans into MC bit payload using the same nibble
/// encoding as `decode_mc_bits` and the Java reference implementation.
///
/// This implementation builds the payload into a `BytesMut` buffer to avoid
/// intermediate `Vec<u8>` allocations and then freezes it into an immutable
/// `Bytes` handle suitable for zero-copy sharing.
fn encode_mc_bits(bools: &[bool]) -> Bytes {
    let mut out = BytesMut::with_capacity(bools.len().div_ceil(2));
    let mut i = 0usize;
    while i < bools.len() {
        let mut b = 0u8;
        if bools[i] {
            b |= 0x10;
        }
        if i + 1 < bools.len() && bools[i + 1] {
            b |= 0x01;
        }
        out.put_u8(b);
        i += 2;
    }
    out.freeze()
}

/// Main connection driver for MC session (S7-aligned): establish transport internally
async fn run_connection(
    session: Arc<Session>,
    config: Arc<SessionConfig>,
    cancel: CancellationToken,
) {
    // establish transport here (align with S7)
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

/// Main connection driver using a pre-connected TcpStream (S7-aligned)
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

    // MC protocol has no handshake, move directly to Active
    publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Active);

    // Setup framed codec. All encoding/decoding goes through this framed
    // transport using `Sink`/`Stream` semantics for `McMessage`.
    let mut framed = Framed::new(
        stream,
        Codec {
            frame_variant: config.frame_variant,
        },
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Closing);
                break;
            }
            req = request_rx.recv() => {
                if let Some(request) = req {
                    // Send full MC message via framed Sink (encodes with `Codec`).
                    if let Err(e) = framed.send(request.message).await {
                        let _ = request.response_tx.send(Err(std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            format!("MC write failed: {e}"),
                        )));
                        let _ = events_tx.send(SessionEvent::TransportError);
                        break;
                    }
                    // Wait for one response with a per-request timeout bound to
                    // the configured read timeout.
                    match time::timeout(request.timeout, framed.next()).await {
                        Ok(Some(Ok(msg))) => {
                            let _ = request.response_tx.send(Ok(msg));
                        }
                        Ok(Some(Err(e))) => {
                            let _ = request.response_tx.send(Err(e));
                            let _ = events_tx.send(SessionEvent::TransportError);
                            break;
                        }
                        Ok(None) => {
                            let _ = request.response_tx.send(Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "MC connection closed",
                            )));
                            let _ = events_tx.send(SessionEvent::TransportError);
                            break;
                        }
                        Err(_elapsed) => {
                            let _ = request.response_tx.send(Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "MC request timed out waiting for response",
                            )));
                            let _ = events_tx.send(SessionEvent::TransportError);
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
        }
    }

    publish_lifecycle(&events_tx, &lifecycle_tx, SessionLifecycleState::Closed);
}

/// Publish lifecycle change to both event bus and watch channel
#[inline]
fn publish_lifecycle(
    events_tx: &broadcast::Sender<SessionEvent>,
    lifecycle_tx: &watch::Sender<SessionLifecycleState>,
    state: SessionLifecycleState,
) {
    let _ = events_tx.send(SessionEvent::LifecycleChanged(state));
    let _ = lifecycle_tx.send(state);
}
