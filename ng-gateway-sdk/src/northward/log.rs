//! Northward plugin <-> host log bridge utilities.
//!
//! This is the northward-plugin counterpart to `southward::log`:
//! - A plugin-side `tracing_subscriber::Layer` that captures events and flushes them to a host sink
//! - A stable C ABI `LogSinkV1` (re-used from `crate::log`) registered by the host loader
//!
//! # Design notes
//! - The sink is **per dynamic library**, not per plugin instance.
//! - We MUST propagate `app_id` via span inheritance so app-level overrides work reliably.

use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::VecDeque,
    error, fmt,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::runtime::Handle;
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

use crate::log::{fields, LogSinkV1, LOG_SINK_ABI_V1};

/// Plugin-side log bridge configuration.
#[derive(Debug, Clone, Copy)]
pub struct PluginLogBridgeConfig {
    /// Max number of queued events before dropping oldest.
    pub queue_capacity: usize,
    /// Max bytes per log message (truncate beyond).
    pub event_max_bytes: usize,
    /// Max events per flush batch.
    pub batch_max_events: usize,
    /// Max bytes per flush batch.
    pub batch_max_bytes: usize,
}

impl Default for PluginLogBridgeConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 10_000,
            event_max_bytes: 8 * 1024,
            batch_max_events: 256,
            batch_max_bytes: 256 * 1024,
        }
    }
}

struct PluginLogState {
    sink: Mutex<Option<LogSinkV1>>,
    max_level: AtomicU8,
    cfg: PluginLogBridgeConfig,
    queue: Mutex<VecDeque<PluginWireEvent>>,
    notify: tokio::sync::Notify,
    flush_started: AtomicBool,
}

static PLUGIN_LOG_STATE: OnceCell<Arc<PluginLogState>> = OnceCell::new();

fn plugin_state(cfg: PluginLogBridgeConfig) -> Arc<PluginLogState> {
    PLUGIN_LOG_STATE
        .get_or_init(|| {
            Arc::new(PluginLogState {
                sink: Mutex::new(None),
                // Default to INFO.
                max_level: AtomicU8::new(level_to_u8(&Level::INFO)),
                cfg,
                queue: Mutex::new(VecDeque::new()),
                notify: tokio::sync::Notify::new(),
                flush_started: AtomicBool::new(false),
            })
        })
        .clone()
}

/// Set the plugin log sink (host registers this after loading the library).
///
/// # Returns
/// - 0: ok
/// - 1: abi version mismatch
pub fn set_log_sink(sink: LogSinkV1) -> u32 {
    if sink.abi_version != LOG_SINK_ABI_V1 {
        return 1;
    }
    let st = plugin_state(PluginLogBridgeConfig::default());
    let mut guard = st.sink.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(sink);
    0
}

/// Set the max log level for this plugin library (dynamic).
///
/// Level mapping:
/// - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
pub fn set_max_level(level: u8) -> u32 {
    let st = plugin_state(PluginLogBridgeConfig::default());
    st.max_level.store(level.min(4), Ordering::Relaxed);
    0
}

/// Get the current plugin max level (dynamic).
pub fn get_max_level() -> u8 {
    let st = plugin_state(PluginLogBridgeConfig::default());
    st.max_level.load(Ordering::Relaxed)
}

/// Initialize tracing in the plugin `cdylib` and install the bridge layer.
///
/// # Important
/// This function is called by the host loader. It should not block.
pub fn init_plugin_tracing(handle: Handle, debug: bool) {
    let st = plugin_state(PluginLogBridgeConfig::default());

    let init_level = if debug { Level::DEBUG } else { Level::INFO };
    st.max_level
        .store(level_to_u8(&init_level), Ordering::Relaxed);

    // Bridge `log` crate into `tracing`.
    let _ = LogTracer::init();

    // Install a lightweight subscriber that only captures and bridges events.
    let layer = PluginBridgeLayer { st: st.clone() };
    let subscriber = tracing_subscriber::registry().with(layer);
    let _ = tracing::subscriber::set_global_default(subscriber);

    // Start the flush task once.
    if !st.flush_started.swap(true, Ordering::SeqCst) {
        handle.spawn(plugin_flush_loop(st));
    }
}

struct PluginBridgeLayer {
    st: Arc<PluginLogState>,
}

/// Span extension: cached `app_id` for host-side filtering.
#[derive(Debug, Clone, Copy, Default)]
struct AppIdExt(Option<i32>);

#[derive(Default)]
struct AppIdVisitor {
    app_id: Option<i32>,
}

impl Visit for AppIdVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == fields::APP_ID {
            self.app_id = Some(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == fields::APP_ID {
            self.app_id = Some((value.min(i32::MAX as u64)) as i32);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginWireSpan {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginWireEvent {
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
    span: Option<PluginWireSpan>,
}

impl<S> Layer<S> for PluginBridgeLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        let max = self.st.max_level.load(Ordering::Relaxed);
        level_to_u8(metadata.level()) <= max
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = AppIdVisitor::default();
        attrs.record(&mut v);

        // Inherit `app_id` from ancestors so host-side per-app filtering stays reliable
        // even when dependencies create nested spans that don't repeat the field.
        if v.app_id.is_none() {
            let mut p = span.parent();
            while let Some(ps) = p {
                if let Some(ext) = ps.extensions().get::<AppIdExt>() {
                    if ext.0.is_some() {
                        v.app_id = ext.0;
                        break;
                    }
                }
                p = ps.parent();
            }
        }

        span.extensions_mut().insert(AppIdExt(v.app_id));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = AppIdVisitor::default();
        values.record(&mut v);
        if v.app_id.is_none() {
            return;
        }
        let mut exts = span.extensions_mut();
        if let Some(ext) = exts.get_mut::<AppIdExt>() {
            ext.0 = v.app_id;
        } else {
            exts.insert(AppIdExt(v.app_id));
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let max = self.st.max_level.load(Ordering::Relaxed);
        if level_to_u8(meta.level()) > max {
            return;
        }

        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .fields
            .remove(fields::MESSAGE)
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let message = truncate_utf8(&message, self.st.cfg.event_max_bytes);

        let current_span: Option<SpanRef<'_, S>> = ctx.lookup_current();
        let span = current_span.as_ref().map(|s| {
            let app_id = s.extensions().get::<AppIdExt>().and_then(|e| e.0);
            PluginWireSpan {
                name: s.metadata().name().to_string(),
                app_id,
                fields: None,
            }
        });

        let fields = if visitor.fields.is_empty() {
            None
        } else {
            Some(visitor.fields)
        };

        let wire = PluginWireEvent {
            ts: chrono::Utc::now().timestamp_millis(),
            level: None,
            level_u8: Some(level_to_u8(meta.level())),
            target: meta.target().to_string(),
            message,
            fields,
            span,
        };

        let mut q = self.st.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(wire);
        while q.len() > self.st.cfg.queue_capacity {
            q.pop_front();
        }
        drop(q);
        self.st.notify.notify_one();
    }
}

async fn plugin_flush_loop(st: Arc<PluginLogState>) {
    loop {
        tokio::select! {
            _ = st.notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        let sink = {
            let guard = st.sink.lock().unwrap_or_else(|e| e.into_inner());
            *guard
        };
        let Some(sink) = sink else { continue };

        let mut batch: Vec<PluginWireEvent> = Vec::new();
        let mut bytes_budget = st.cfg.batch_max_bytes;
        {
            let mut q = st.queue.lock().unwrap_or_else(|e| e.into_inner());
            while batch.len() < st.cfg.batch_max_events {
                let Some(ev) = q.pop_front() else { break };
                let approx = ev
                    .message
                    .len()
                    .saturating_add(ev.target.len())
                    .saturating_add(256);
                if approx > bytes_budget && !batch.is_empty() {
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
            let mut buf: Vec<u8> = Vec::with_capacity(st.cfg.batch_max_bytes.min(1024 * 1024));
            for ev in batch.iter() {
                let _ = serde_json::to_writer(&mut buf, ev);
                buf.push(b'\n');
            }
            if !buf.is_empty() {
                emit_batch(sink.user_data, buf.as_ptr(), buf.len());
            }
        } else {
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
    fn record_error(&mut self, field: &Field, value: &(dyn error::Error + 'static)) {
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
