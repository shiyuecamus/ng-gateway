//! In-memory LogHub (tail + follow) for realtime logs.
//!
//! # Design goals
//! - Explicit capacity limits to avoid unbounded memory usage
//! - Simple APIs for tail (ring buffers) and follow (broadcast)
//! - Reasonable performance: single lock for in-memory buffers, plus Tokio broadcast

use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::settings::RealtimeLogs as RealtimeLogsSettings;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroUsize,
};
use tokio::sync::broadcast;

/// A serializable log level for websocket payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<&tracing::Level> for LogLevel {
    #[inline]
    fn from(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::ERROR => Self::Error,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::INFO => Self::Info,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::TRACE => Self::Trace,
        }
    }
}

impl From<tracing::Level> for LogLevel {
    #[inline]
    fn from(level: tracing::Level) -> Self {
        Self::from(&level)
    }
}

impl From<LogLevel> for tracing::Level {
    #[inline]
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => tracing::Level::ERROR,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Trace => tracing::Level::TRACE,
        }
    }
}

impl From<&LogLevel> for tracing::Level {
    #[inline]
    fn from(level: &LogLevel) -> Self {
        (*level).into()
    }
}

/// Event source (host or driver).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    Host,
    Driver,
}

/// Optional span info captured with a log event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSpan {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Map<String, Value>>,
}

/// A unified log event payload for WebSocket streaming.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub ts: i64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
    pub source: LogSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<LogSpan>,
}

/// LogHub configuration.
#[derive(Debug, Clone)]
pub struct LogHubConfig {
    pub global_capacity: usize,
    pub per_channel_capacity: usize,
    pub max_channels: usize,
    pub broadcast_capacity: usize,
}

impl From<&RealtimeLogsSettings> for LogHubConfig {
    #[inline]
    fn from(s: &RealtimeLogsSettings) -> Self {
        Self {
            global_capacity: s.global_capacity.max(1),
            per_channel_capacity: s.per_channel_capacity.max(1),
            max_channels: s.max_channels.max(1),
            broadcast_capacity: s.broadcast_capacity.max(16),
        }
    }
}

struct ChannelBuf {
    buf: VecDeque<LogEvent>,
}

struct LogHubInner {
    global: VecDeque<LogEvent>,
    channels: HashMap<i32, ChannelBuf>,
    /// Simple LRU list for channels. Back = most recently accessed.
    lru: VecDeque<i32>,
}

/// In-memory log hub with bounded ring buffers and broadcast fan-out.
pub struct LogHub {
    cfg: LogHubConfig,
    inner: Mutex<LogHubInner>,
    tx: broadcast::Sender<LogEvent>,
}

impl LogHub {
    /// Create a new LogHub.
    pub fn new(cfg: LogHubConfig) -> NGResult<Self> {
        let cap = NonZeroUsize::new(cfg.broadcast_capacity)
            .ok_or_else(|| NGError::from("broadcast_capacity must be > 0"))?;
        let (tx, _rx) = broadcast::channel(cap.get());

        Ok(Self {
            cfg,
            inner: Mutex::new(LogHubInner {
                global: VecDeque::new(),
                channels: HashMap::new(),
                lru: VecDeque::new(),
            }),
            tx,
        })
    }

    /// Subscribe to realtime follow stream.
    #[inline]
    pub fn subscribe(&self) -> broadcast::Receiver<LogEvent> {
        self.tx.subscribe()
    }

    /// Push one log event into buffers and broadcast to followers.
    ///
    /// # Notes
    /// - This is intentionally non-async for use in `tracing` layers.
    /// - Broadcast send is best-effort; failures only happen when there are no receivers.
    pub fn push(&self, ev: LogEvent) {
        {
            let mut inner = self.inner.lock();

            inner.global.push_back(ev.clone());
            while inner.global.len() > self.cfg.global_capacity {
                inner.global.pop_front();
            }

            if let Some(channel_id) = ev.channel_id {
                self.touch_channel(&mut inner, channel_id);
                let buf = inner
                    .channels
                    .entry(channel_id)
                    .or_insert_with(|| ChannelBuf {
                        buf: VecDeque::new(),
                    });
                buf.buf.push_back(ev.clone());
                while buf.buf.len() > self.cfg.per_channel_capacity {
                    buf.buf.pop_front();
                }
                self.evict_if_needed(&mut inner);
            }
        }

        let _ = self.tx.send(ev);
    }

    /// Tail recent global logs (up to `n` items).
    pub fn tail_global(&self, n: usize) -> Vec<LogEvent> {
        let inner = self.inner.lock();
        let n = n.min(inner.global.len());
        inner
            .global
            .iter()
            .skip(inner.global.len().saturating_sub(n))
            .cloned()
            .collect()
    }

    /// Tail recent logs for a specific channel (up to `n` items).
    pub fn tail_channel(&self, channel_id: i32, n: usize) -> Vec<LogEvent> {
        let mut inner = self.inner.lock();
        self.touch_channel(&mut inner, channel_id);
        let Some(buf) = inner.channels.get(&channel_id) else {
            return Vec::new();
        };
        let n = n.min(buf.buf.len());
        buf.buf
            .iter()
            .skip(buf.buf.len().saturating_sub(n))
            .cloned()
            .collect()
    }

    fn touch_channel(&self, inner: &mut LogHubInner, channel_id: i32) {
        if let Some(pos) = inner.lru.iter().position(|&id| id == channel_id) {
            inner.lru.remove(pos);
        }
        inner.lru.push_back(channel_id);
    }

    fn evict_if_needed(&self, inner: &mut LogHubInner) {
        while inner.channels.len() > self.cfg.max_channels {
            let Some(victim) = inner.lru.pop_front() else {
                break;
            };
            inner.channels.remove(&victim);
        }
    }
}
