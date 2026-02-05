//! Driver -> host log bridge (host side).
//!
//! This module implements the host-side log sink for dynamically loaded `cdylib` drivers:
//! - FFI callback functions that **only copy + enqueue** (never block)
//! - A background ingest loop that parses JSON/JSONL and re-emits as host `tracing` events
//!
//! # Why this lives in `ng-gateway-common::log`
//! Driver logs are part of the gateway's single authoritative logging system:
//! they must go through the same console/file pipeline as host logs.

use crate::log::control;
use arc_swap::ArcSwap;
use dashmap::DashMap;
use ng_gateway_sdk::log::{fields as log_fields, LogSinkV1, LOG_SINK_ABI_V1};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Notify;
use tracing::{Instrument, Span};

/// Host-side sink handle that keeps the callback context alive.
pub struct HostLogSinkHandle {
    ctx: Box<HostSinkContext>,
}

impl HostLogSinkHandle {
    /// Build a `LogSinkV1` struct to register into a driver.
    pub fn sink(&self) -> LogSinkV1 {
        LogSinkV1 {
            abi_version: LOG_SINK_ABI_V1,
            user_data: (&*self.ctx) as *const HostSinkContext as *mut c_void,
            emit_json: host_emit_json,
            emit_batch_json: Some(host_emit_batch_json),
            flush: None,
        }
    }

    /// Update the driver type label (best-effort).
    ///
    /// This enables the loader to register the log sink **before** probing metadata,
    /// while still ending up with a correct `driver_type` label for re-emitted events.
    pub fn set_driver_type(&self, driver_type: String) {
        // Update label atomically and clear cached spans so the next event rebuilds spans
        // with the updated `driver_type` field.
        self.ctx.driver_type.store(Arc::new(driver_type));
        self.ctx.span_cache.clear();
    }
}

#[derive(Clone)]
struct HostSinkContext {
    driver_id: i32,
    /// Driver type label stored in an `ArcSwap<String>` for lock-free reads.
    ///
    /// We intentionally store `String` (Sized) rather than `str` to keep `arc-swap`
    /// trait bounds simple and portable.
    driver_type: Arc<ArcSwap<String>>,
    /// Cached spans keyed by channel id (and a sentinel for no-channel events).
    span_cache: Arc<DashMap<i32, Span>>,
}

struct HostIngestItem {
    ctx: HostSinkContext,
    bytes: Vec<u8>,
}

struct HostBridge {
    queue: Mutex<VecDeque<HostIngestItem>>,
    notify: Notify,
    started: AtomicBool,
}

static HOST_BRIDGE: OnceCell<Arc<HostBridge>> = OnceCell::new();

/// Resolve current driver ingest queue capacity from runtime settings.
///
/// # Semantics
/// - If log-control runtime is not initialized, falls back to a safe default.
/// - Always returns a value >= 1.
#[inline]
fn current_queue_capacity() -> usize {
    control::global()
        .map(|rt| rt.settings().ingest_queue_capacity)
        .unwrap_or(10_000)
        .max(1)
}

fn host_bridge() -> Arc<HostBridge> {
    HOST_BRIDGE
        .get_or_init(|| {
            Arc::new(HostBridge {
                queue: Mutex::new(VecDeque::new()),
                notify: Notify::new(),
                started: AtomicBool::new(false),
            })
        })
        .clone()
}

/// Ensure the host ingest loop is started (idempotent).
///
/// # Important
/// Must be called from within a Tokio runtime context.
pub fn ensure_ingest_started() {
    let bridge = host_bridge();
    if bridge.started.swap(true, Ordering::SeqCst) {
        return;
    }
    let b = Arc::clone(&bridge);
    tokio::spawn(async move { host_ingest_loop(b).await }.in_current_span());
}

/// Create a host log sink handle for a specific driver.
pub fn create_sink(driver_id: i32, driver_type: String) -> HostLogSinkHandle {
    let _ = host_bridge();
    HostLogSinkHandle {
        ctx: Box::new(HostSinkContext {
            driver_id,
            driver_type: Arc::new(ArcSwap::from(Arc::new(driver_type))),
            span_cache: Arc::new(DashMap::new()),
        }),
    }
}

extern "C" fn host_emit_json(user_data: *mut c_void, ptr: *const u8, len: usize) {
    host_enqueue(user_data, ptr, len);
}

extern "C" fn host_emit_batch_json(user_data: *mut c_void, ptr: *const u8, len: usize) {
    host_enqueue(user_data, ptr, len);
}

fn host_enqueue(user_data: *mut c_void, ptr: *const u8, len: usize) {
    if user_data.is_null() || ptr.is_null() || len == 0 {
        return;
    }
    let ctx = unsafe { &*(user_data as *const HostSinkContext) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };

    // Copy immediately (FFI boundary).
    let mut payload = Vec::with_capacity(len.min(1024 * 1024));
    payload.extend_from_slice(bytes);

    let cap = current_queue_capacity();
    let bridge = host_bridge();
    let mut q = bridge.queue.lock().unwrap_or_else(|e| e.into_inner());
    q.push_back(HostIngestItem {
        ctx: ctx.clone(),
        bytes: payload,
    });
    while q.len() > cap {
        q.pop_front();
    }
    drop(q);
    bridge.notify.notify_one();
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostWireSpan {
    #[allow(unused)]
    name: String,
    #[serde(default)]
    channel_id: Option<i32>,
    #[serde(default)]
    fields: Map<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostWireEvent {
    #[allow(unused)]
    ts: i64,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    level_u8: Option<u8>,
    target: String,
    message: String,
    #[serde(default)]
    fields: Map<String, Value>,
    #[serde(default)]
    span: Option<HostWireSpan>,
}

async fn host_ingest_loop(bridge: Arc<HostBridge>) {
    loop {
        bridge.notify.notified().await;

        let mut items: Vec<HostIngestItem> = Vec::new();
        {
            let mut q = bridge.queue.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(it) = q.pop_front() {
                items.push(it);
                if items.len() >= 1024 {
                    break;
                }
            }
        }

        for item in items.into_iter() {
            ingest_one_payload(&item.ctx, &item.bytes);
        }
    }
}

fn ingest_one_payload(ctx: &HostSinkContext, bytes: &[u8]) {
    for mut line in bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(b'\r') = line.last() {
            line = &line[..line.len().saturating_sub(1)];
        }
        if line.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_slice::<HostWireEvent>(line) else {
            continue;
        };
        reemit_as_tracing(ctx, ev);
    }
}

fn reemit_as_tracing(ctx: &HostSinkContext, ev: HostWireEvent) {
    let level = ev
        .level_u8
        .and_then(parse_level_u8)
        .or(ev.level.as_deref().and_then(parse_level))
        .unwrap_or(tracing::Level::INFO);
    // Resolve channel attribution (best-effort):
    // 1) explicit `span.channel_id` (preferred)
    // 2) `channel_id` recorded into span fields
    // 3) `channel_id` recorded into event fields (some callers log it on the event, not span)
    let channel_id = ev
        .span
        .as_ref()
        .and_then(|s| {
            s.channel_id
                .or(log_fields::map_i32(&s.fields, log_fields::CHANNEL_ID))
        })
        .or(log_fields::map_i32(&ev.fields, log_fields::CHANNEL_ID));

    // Use a sentinel key to avoid span allocation for no-channel events.
    // Channel ids are expected to be positive; this sentinel is reserved for "no channel".
    const NO_CHANNEL_KEY: i32 = i32::MIN;
    let key = channel_id.unwrap_or(NO_CHANNEL_KEY);

    // Avoid allocating a new span per log event (hot path).
    let span = ctx
        .span_cache
        .get(&key)
        .map(|s| s.clone())
        .unwrap_or_else(|| {
            let dt = ctx.driver_type.load();
            let dt_str: &str = dt.as_str();

            let span = if key == NO_CHANNEL_KEY {
                tracing::info_span!(
                    log_fields::SPAN_DRIVER_LOG,
                    source = log_fields::SOURCE_DRIVER,
                    driver_id = ctx.driver_id,
                    driver_type = dt_str
                )
            } else {
                tracing::info_span!(
                    log_fields::SPAN_DRIVER_LOG,
                    source = log_fields::SOURCE_DRIVER,
                    driver_id = ctx.driver_id,
                    driver_type = dt_str,
                    // Use i64 so downstream span visitors can capture reliably.
                    channel_id = i64::from(key)
                )
            };
            ctx.span_cache.insert(key, span.clone());
            span
        });
    let _enter = span.enter();
    emit_driver_event(level, &ev, ctx);
}

fn emit_driver_event(level: tracing::Level, ev: &HostWireEvent, ctx: &HostSinkContext) {
    // Keep a stable callsite target and attach original driver target as a field.
    // Avoid pre-serializing JSON fields on the hot path; formatting only happens if enabled.
    match level {
        tracing::Level::ERROR => tracing::error!(
            target: log_fields::TARGET_DRIVER,
            source = log_fields::SOURCE_DRIVER,
            driver_id = ctx.driver_id,
            driver_target = %ev.target,
            driver_fields = ?ev.fields,
            "{}",
            ev.message
        ),
        tracing::Level::WARN => tracing::warn!(
            target: log_fields::TARGET_DRIVER,
            source = log_fields::SOURCE_DRIVER,
            driver_id = ctx.driver_id,
            driver_target = %ev.target,
            driver_fields = ?ev.fields,
            "{}",
            ev.message
        ),
        tracing::Level::INFO => tracing::info!(
            target: log_fields::TARGET_DRIVER,
            source = log_fields::SOURCE_DRIVER,
            driver_id = ctx.driver_id,
            driver_target = %ev.target,
            driver_fields = ?ev.fields,
            "{}",
            ev.message
        ),
        tracing::Level::DEBUG => tracing::debug!(
            target: log_fields::TARGET_DRIVER,
            source = log_fields::SOURCE_DRIVER,
            driver_id = ctx.driver_id,
            driver_target = %ev.target,
            driver_fields = ?ev.fields,
            "{}",
            ev.message
        ),
        tracing::Level::TRACE => tracing::trace!(
            target: log_fields::TARGET_DRIVER,
            source = log_fields::SOURCE_DRIVER,
            driver_id = ctx.driver_id,
            driver_target = %ev.target,
            driver_fields = ?ev.fields,
            "{}",
            ev.message
        ),
    }
}

#[inline]
fn parse_level(s: &str) -> Option<tracing::Level> {
    match s {
        "ERROR" => Some(tracing::Level::ERROR),
        "WARN" => Some(tracing::Level::WARN),
        "INFO" => Some(tracing::Level::INFO),
        "DEBUG" => Some(tracing::Level::DEBUG),
        "TRACE" => Some(tracing::Level::TRACE),
        _ => None,
    }
}

#[inline]
fn parse_level_u8(v: u8) -> Option<tracing::Level> {
    match v {
        0 => Some(tracing::Level::ERROR),
        1 => Some(tracing::Level::WARN),
        2 => Some(tracing::Level::INFO),
        3 => Some(tracing::Level::DEBUG),
        4 => Some(tracing::Level::TRACE),
        _ => None,
    }
}
