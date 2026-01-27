//! Driver <-> host log bridge utilities.
//!
//! This module provides:
//! - A stable C ABI `LogSinkV1` so the host can register a callback sink into a `cdylib` driver.
//! - A driver-side `tracing_subscriber::Layer` that captures events and flushes them to the sink
//!   asynchronously in batches (to avoid blocking hot paths).
//! - A host-side sink implementation that ingests JSON/JSONL payloads and re-emits them as host
//!   `tracing` events, so they naturally flow into the unified host logger.
//!
//! # Safety contract (FFI)
//! - The driver MUST treat sink callbacks as "best-effort, non-blocking".
//! - The host sink callbacks MUST return quickly: copy + enqueue only.

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::VecDeque,
    ffi::c_void,
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
};
use tokio::{runtime::Handle, sync::Notify};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Level, Subscriber,
};
use tracing_log::LogTracer;
use tracing_subscriber::{
    layer::Context,
    layer::SubscriberExt,
    registry::{LookupSpan, SpanRef},
    Layer,
};

/// Shared log field keys used by the driver<->host bridge.
///
/// Keep these as a single source of truth so both sides agree on the schema.
pub mod fields {
    use serde_json::{Map, Value};

    /// Field key for channel attribution.
    pub const CHANNEL_ID: &str = "channel_id";
    /// Synthetic field key used by `tracing` for event body.
    pub const MESSAGE: &str = "message";
    /// Field key that marks a log as originating from a driver.
    ///
    /// Used by the host to apply driver-specific routing and processing.
    pub const SOURCE: &str = "source";
    /// The `SOURCE` value used by the driver->host bridge.
    pub const SOURCE_DRIVER: &str = "driver";
    /// Field key for driver type attribution (e.g. "modbus", "opcua", "s7").
    pub const DRIVER_TYPE: &str = "driver_type";

    /// Stable tracing target for driver logs re-emitted by the host.
    pub const TARGET_DRIVER: &str = "driver";
    /// Stable span name used by the host-side driver ingest bridge.
    pub const SPAN_DRIVER_LOG: &str = "driver-log";

    /// Extract an `i32` from a JSON map field.
    #[inline]
    pub fn map_i32(map: &Map<String, Value>, key: &str) -> Option<i32> {
        map.get(key)
            .and_then(|v| v.as_i64())
            .and_then(|v| i32::try_from(v).ok())
    }
}

/// ABI version for `LogSinkV1`.
pub const LOG_SINK_ABI_V1: u32 = 1;

/// Log sink emit function signature.
pub type LogEmitFn = extern "C" fn(user_data: *mut c_void, ptr: *const u8, len: usize);

/// Optional sink flush function signature.
pub type LogFlushFn = extern "C" fn(user_data: *mut c_void);

/// A stable log sink ABI for driver -> host log streaming.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LogSinkV1 {
    pub abi_version: u32,
    pub user_data: *mut c_void,
    pub emit_json: LogEmitFn,
    pub emit_batch_json: Option<LogEmitFn>,
    pub flush: Option<LogFlushFn>,
}

// SAFETY:
// `LogSinkV1` is a plain C-ABI struct containing raw pointers and function pointers.
// The host/driver contract ensures `user_data` remains valid for the duration of usage.
unsafe impl Send for LogSinkV1 {}
unsafe impl Sync for LogSinkV1 {}

/// Driver-side log bridge configuration.
#[derive(Debug, Clone, Copy)]
pub struct DriverLogBridgeConfig {
    /// Max number of queued events before dropping oldest.
    pub queue_capacity: usize,
    /// Max bytes per log message (truncate beyond).
    pub event_max_bytes: usize,
    /// Max events per flush batch.
    pub batch_max_events: usize,
    /// Max bytes per flush batch.
    pub batch_max_bytes: usize,
}

impl Default for DriverLogBridgeConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 10_000,
            event_max_bytes: 8 * 1024,
            batch_max_events: 256,
            batch_max_bytes: 256 * 1024,
        }
    }
}

/// Internal driver-side shared state.
struct DriverLogState {
    sink: Mutex<Option<LogSinkV1>>,
    max_level: AtomicU8,
    cfg: DriverLogBridgeConfig,
    queue: Mutex<VecDeque<DriverWireEvent>>,
    notify: Notify,
    flush_started: AtomicBool,
}

static DRIVER_LOG_STATE: OnceCell<Arc<DriverLogState>> = OnceCell::new();

/// Initialize driver-side log bridge state (idempotent).
fn driver_state(cfg: DriverLogBridgeConfig) -> Arc<DriverLogState> {
    DRIVER_LOG_STATE
        .get_or_init(|| {
            Arc::new(DriverLogState {
                sink: Mutex::new(None),
                // Default to INFO.
                max_level: AtomicU8::new(level_to_u8(&tracing::Level::INFO)),
                cfg,
                queue: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
                flush_started: AtomicBool::new(false),
            })
        })
        .clone()
}

/// Set the driver log sink (host registers this after loading the library).
///
/// # Returns
/// - 0: ok
/// - 1: abi version mismatch
pub fn set_log_sink(sink: LogSinkV1) -> u32 {
    if sink.abi_version != LOG_SINK_ABI_V1 {
        return 1;
    }
    let st = driver_state(DriverLogBridgeConfig::default());
    let mut guard = st.sink.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(sink);
    0
}

/// Set the max log level for this driver (dynamic).
///
/// Level mapping:
/// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
pub fn set_max_level(level: u8) -> u32 {
    let st = driver_state(DriverLogBridgeConfig::default());
    st.max_level.store(level.min(4), Ordering::Relaxed);
    0
}

/// Get the current driver max level (dynamic).
pub fn get_max_level() -> u8 {
    let st = driver_state(DriverLogBridgeConfig::default());
    st.max_level.load(Ordering::Relaxed)
}

/// Initialize tracing in the driver `cdylib` and install the bridge layer.
///
/// # Important
/// This function must be called by the host loader. It should not block.
pub fn init_driver_tracing(handle: Handle, debug: bool) {
    let st = driver_state(DriverLogBridgeConfig::default());
    // Initialize max level according to debug flag (best-effort).
    let init_level = if debug {
        tracing::Level::DEBUG
    } else {
        tracing::Level::INFO
    };
    st.max_level
        .store(level_to_u8(&init_level), Ordering::Relaxed);

    // Bridge `log` crate into `tracing`.
    let _ = LogTracer::init();

    // Install a lightweight subscriber that only captures and bridges events.
    let layer = DriverBridgeLayer {
        st: Arc::clone(&st),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Start the flush task once.
    if !st.flush_started.swap(true, Ordering::SeqCst) {
        handle.spawn(driver_flush_loop(st));
    }
}

/// Driver bridge layer: capture events and enqueue into an internal bounded queue.
struct DriverBridgeLayer {
    st: Arc<DriverLogState>,
}

/// Span extension: cached `channel_id` for host-side filtering.
#[derive(Debug, Clone, Copy, Default)]
struct ChannelIdExt(Option<i32>);

#[derive(Default)]
struct ChannelIdVisitor {
    channel_id: Option<i32>,
}

impl Visit for ChannelIdVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == fields::CHANNEL_ID {
            self.channel_id = Some(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == fields::CHANNEL_ID {
            self.channel_id = Some((value.min(i32::MAX as u64)) as i32);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverWireSpan {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DriverWireEvent {
    ts: i64,
    /// Backward compatible string level (older hosts).
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    /// Compact numeric level for zero-allocation encoding.
    ///
    /// Mapping: 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
    #[serde(skip_serializing_if = "Option::is_none")]
    level_u8: Option<u8>,
    target: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span: Option<DriverWireSpan>,
}

impl<S> Layer<S> for DriverBridgeLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        let max = self.st.max_level.load(Ordering::Relaxed);
        level_to_u8(metadata.level()) <= max
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = ChannelIdVisitor::default();
        attrs.record(&mut v);

        // Inherit `channel_id` from ancestors so host-side per-channel filtering stays reliable
        // even when dependencies create nested spans that don't repeat the field.
        if v.channel_id.is_none() {
            let mut p = span.parent();
            while let Some(ps) = p {
                if let Some(ext) = ps.extensions().get::<ChannelIdExt>() {
                    if ext.0.is_some() {
                        v.channel_id = ext.0;
                        break;
                    }
                }
                p = ps.parent();
            }
        }

        span.extensions_mut().insert(ChannelIdExt(v.channel_id));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = ChannelIdVisitor::default();
        values.record(&mut v);
        if v.channel_id.is_none() {
            return;
        }
        let mut exts = span.extensions_mut();
        if let Some(ext) = exts.get_mut::<ChannelIdExt>() {
            ext.0 = v.channel_id;
        } else {
            exts.insert(ChannelIdExt(v.channel_id));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Fast-path filter (in case enabled() wasn't checked by registry for some callsites).
        let meta = event.metadata();
        let max = self.st.max_level.load(Ordering::Relaxed);
        if level_to_u8(meta.level()) > max {
            return;
        }

        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);

        // Best practice: `message` is the event body, it MUST NOT be duplicated inside `fields`.
        // We extract it and remove it from the structured map to keep payload small and semantics clear.
        let message = visitor
            .fields
            .remove(fields::MESSAGE)
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let message = truncate_utf8(&message, self.st.cfg.event_max_bytes);

        let current_span: Option<SpanRef<'_, S>> = ctx.lookup_current();
        let span = current_span.as_ref().map(|s| {
            let channel_id = s.extensions().get::<ChannelIdExt>().and_then(|e| e.0);
            DriverWireSpan {
                name: s.metadata().name().to_string(),
                channel_id,
                fields: None,
            }
        });

        let fields = if visitor.fields.is_empty() {
            None
        } else {
            Some(visitor.fields)
        };

        let wire = DriverWireEvent {
            ts: chrono::Utc::now().timestamp_millis(),
            level: None,
            level_u8: Some(level_to_u8(meta.level())),
            target: meta.target().to_string(),
            message,
            fields,
            span,
        };

        // Enqueue (drop-old-keep-new).
        let mut q = self.st.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(wire);
        while q.len() > self.st.cfg.queue_capacity {
            q.pop_front();
        }
        drop(q);
        self.st.notify.notify_one();
    }
}

/// Background flush loop in driver runtime.
async fn driver_flush_loop(st: Arc<DriverLogState>) {
    loop {
        // Wait for new logs (or periodic wakeup).
        tokio::select! {
            _ = st.notify.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }

        let sink = {
            let guard = st.sink.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let Some(sink) = sink else { continue };

        // Drain queue into a batch.
        let mut batch: Vec<DriverWireEvent> = Vec::new();
        let mut bytes_budget = st.cfg.batch_max_bytes;
        {
            let mut q = st.queue.lock().unwrap_or_else(|e| e.into_inner());
            while batch.len() < st.cfg.batch_max_events {
                let Some(ev) = q.pop_front() else { break };
                // Rough byte budget: message + target + overhead
                let approx = ev
                    .message
                    .len()
                    .saturating_add(ev.target.len())
                    .saturating_add(256);
                if approx > bytes_budget && !batch.is_empty() {
                    // Put back and flush current batch.
                    q.push_front(ev);
                    break;
                }
                bytes_budget = bytes_budget.saturating_sub(approx);
                batch.push(ev);
            }
        }

        if batch.is_empty() {
            continue;
        }

        if let Some(emit_batch) = sink.emit_batch_json {
            // JSON Lines batch.
            let mut buf: Vec<u8> = Vec::with_capacity(st.cfg.batch_max_bytes.min(1024 * 1024));
            for ev in batch.iter() {
                let _ = serde_json::to_writer(&mut buf, ev);
                buf.push(b'\n');
            }
            if !buf.is_empty() {
                emit_batch(sink.user_data, buf.as_ptr(), buf.len());
            }
        } else {
            // Fallback: emit per event.
            for ev in batch.iter() {
                let mut buf: Vec<u8> = Vec::with_capacity(512);
                let _ = serde_json::to_writer(&mut buf, ev);
                (sink.emit_json)(sink.user_data, buf.as_ptr(), buf.len());
            }
        }
    }
}

#[derive(Default)]
struct JsonVisitor {
    fields: Map<String, Value>,
}

impl Visit for JsonVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), Value::from(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .insert(field.name().to_string(), Value::from(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), Value::from(format!("{value:?}")));
    }
}

#[inline]
fn level_to_u8(level: &Level) -> u8 {
    if *level == Level::ERROR {
        0
    } else if *level == Level::WARN {
        1
    } else if *level == Level::INFO {
        2
    } else if *level == Level::DEBUG {
        3
    } else {
        4
    }
}

#[inline]
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_string();
    out.push('…');
    out
}
