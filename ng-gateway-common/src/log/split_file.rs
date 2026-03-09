//! Split file logging layer that routes logs to different files based on `source`.
//!
//! This module implements a custom tracing layer that:
//! - Routes host logs (no driver_type) to `host.log`
//! - Routes driver logs (`source=driver` + `driver_type`) to `driver_<driver_type>.log`
//! - Routes plugin logs (`source=plugin` + `plugin_type`) to `plugin_<plugin_type>.log`
//! - Uses rolling file appenders for each log file
//! - Maintains thread-safe writer handles for each route key

use super::span_ext::ChannelIdExt;
use dashmap::{DashMap, Entry};
use ng_gateway_models::settings::{LoggingFileRotation, LoggingFormat, RotationMode, TimeRotation};
use ng_gateway_sdk::log::fields as log_fields;
use serde_json;
use std::{
    fmt::{Debug, Display, Write as _},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    subscriber::Interest,
    Event, Level, Metadata, Subscriber,
};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

/// Sanitize a log label into a safe file stem component.
///
/// # Security
/// `driver_type` / `plugin_type` can originate from external `cdylib`s. We must prevent:
/// - path traversal (`../`)
/// - path separators (`/`, `\`)
/// - control characters
///
/// # Rules
/// - Keep only ASCII `[a-z0-9_-]` (lowercased)
/// - Replace other characters with `_`
/// - Trim `_` on both ends
/// - Trim to a reasonable length to avoid filesystem/path issues
#[inline]
fn sanitize_file_stem_label(raw: &str) -> Arc<str> {
    const MAX_LEN: usize = 64;
    let mut out = String::with_capacity(raw.len().min(MAX_LEN));
    for ch in raw.chars() {
        if out.len() >= MAX_LEN {
            break;
        }
        let c = ch.to_ascii_lowercase();
        if matches!(c, 'a'..='z' | '0'..='9' | '-' | '_') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        Arc::<str>::from("unknown")
    } else {
        Arc::<str>::from(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogSource {
    Driver,
    Plugin,
}

/// Extension to store routing info in span extensions for efficient lookup.
#[derive(Debug, Clone, Default)]
struct LogRouteExt {
    source: Option<LogSource>,
    driver_type: Option<Arc<str>>,
    plugin_type: Option<Arc<str>>,
}

/// Visitor to extract `source` + (`driver_type`/`plugin_type`) from span/event fields.
#[derive(Default)]
struct LogRouteVisitor {
    source: Option<LogSource>,
    driver_type: Option<Arc<str>>,
    plugin_type: Option<Arc<str>>,
}

impl Visit for LogRouteVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            log_fields::DRIVER_TYPE => {
                self.driver_type = Some(sanitize_file_stem_label(value));
            }
            log_fields::PLUGIN_TYPE => {
                self.plugin_type = Some(sanitize_file_stem_label(value));
            }
            log_fields::SOURCE => {
                if value == log_fields::SOURCE_DRIVER {
                    self.source = Some(LogSource::Driver);
                } else if value == log_fields::SOURCE_PLUGIN {
                    self.source = Some(LogSource::Plugin);
                }
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        // Best-effort: capture string-ish debug values. This makes the extractor resilient
        // even if callers log `driver_type = %...` instead of `driver_type = "..."`.
        match field.name() {
            log_fields::DRIVER_TYPE => {
                let mut s = format!("{value:?}");
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    s = s[1..s.len() - 1].to_string();
                }
                if !s.is_empty() {
                    self.driver_type = Some(sanitize_file_stem_label(&s));
                }
            }
            log_fields::PLUGIN_TYPE => {
                let mut s = format!("{value:?}");
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    s = s[1..s.len() - 1].to_string();
                }
                if !s.is_empty() {
                    self.plugin_type = Some(sanitize_file_stem_label(&s));
                }
            }
            log_fields::SOURCE => {
                let mut s = format!("{value:?}");
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    s = s[1..s.len() - 1].to_string();
                }
                if s == log_fields::SOURCE_DRIVER {
                    self.source = Some(LogSource::Driver);
                } else if s == log_fields::SOURCE_PLUGIN {
                    self.source = Some(LogSource::Plugin);
                }
            }
            _ => {}
        }
    }
}

/// Layer that extracts `source` and routing labels from spans and caches them.
#[derive(Default)]
pub struct LogRouteExtractorLayer;

impl<S> Layer<S> for LogRouteExtractorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = LogRouteVisitor::default();
        attrs.record(&mut visitor);
        if visitor.source.is_some()
            || visitor.driver_type.is_some()
            || visitor.plugin_type.is_some()
        {
            span.extensions_mut().insert(LogRouteExt {
                source: visitor.source,
                driver_type: visitor.driver_type,
                plugin_type: visitor.plugin_type,
            });
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = LogRouteVisitor::default();
        values.record(&mut visitor);
        if visitor.source.is_some()
            || visitor.driver_type.is_some()
            || visitor.plugin_type.is_some()
        {
            let mut exts = span.extensions_mut();
            if let Some(ext) = exts.get_mut::<LogRouteExt>() {
                if visitor.source.is_some() {
                    ext.source = visitor.source;
                }
                if visitor.driver_type.is_some() {
                    ext.driver_type = visitor.driver_type;
                }
                if visitor.plugin_type.is_some() {
                    ext.plugin_type = visitor.plugin_type;
                }
            } else {
                exts.insert(LogRouteExt {
                    source: visitor.source,
                    driver_type: visitor.driver_type,
                    plugin_type: visitor.plugin_type,
                });
            }
        }
    }
}

/// Thread-safe registry of file writers for different log routes.
pub struct SplitFileRegistry {
    writers: DashMap<Arc<str>, NonBlocking>,
    guards: DashMap<Arc<str>, WorkerGuard>,
    log_dir: PathBuf,
    rotation: LoggingFileRotation,
}

impl SplitFileRegistry {
    fn new(log_dir: PathBuf, rotation: LoggingFileRotation) -> Self {
        // Ensure log directory exists (best-effort). This avoids surprising runtime failures
        // when the logger initializes before runtime directories are created.
        let _ = fs::create_dir_all(&log_dir);
        Self {
            writers: DashMap::new(),
            guards: DashMap::new(),
            log_dir,
            rotation,
        }
    }

    #[inline]
    fn get_or_create_writer(&self, key: Arc<str>, file_name: String) -> NonBlocking {
        match self.writers.entry(Arc::clone(&key)) {
            Entry::Occupied(o) => o.get().clone(),
            Entry::Vacant(v) => {
                // Create new writer exactly once per route.
                let appender = RotatingFileAppender::new(
                    self.log_dir.clone(),
                    file_name,
                    self.rotation.clone(),
                );
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);

                // Keep guard alive to avoid dropping the worker thread.
                self.guards.insert(Arc::clone(v.key()), guard);
                v.insert(non_blocking.clone());
                non_blocking
            }
        }
    }

    /// Get or create a writer for a specific driver type.
    fn get_or_create_driver_writer(&self, driver_type: Arc<str>) -> NonBlocking {
        let file = format!("driver_{}.log", &*driver_type);
        let key: Arc<str> = Arc::<str>::from(format!("driver::{driver_type}"));
        self.get_or_create_writer(key, file)
    }

    /// Get or create a writer for a specific plugin type.
    fn get_or_create_plugin_writer(&self, plugin_type: Arc<str>) -> NonBlocking {
        let file = format!("plugin_{}.log", &*plugin_type);
        let key: Arc<str> = Arc::<str>::from(format!("plugin::{plugin_type}"));
        self.get_or_create_writer(key, file)
    }

    /// Get the host log writer (host.log).
    fn get_host_writer(&self) -> NonBlocking {
        const HOST_KEY: &str = "__host__";

        match self.writers.entry(Arc::<str>::from(HOST_KEY)) {
            Entry::Occupied(o) => o.get().clone(),
            Entry::Vacant(v) => {
                let appender = RotatingFileAppender::new(
                    self.log_dir.clone(),
                    "host.log".to_string(),
                    self.rotation.clone(),
                );
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);
                self.guards.insert(Arc::clone(v.key()), guard);
                v.insert(non_blocking.clone());
                non_blocking
            }
        }
    }

    /// Get all active log file names (for API listing).
    pub fn list_log_files(&self) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.log_dir) {
            for entry in entries.flatten() {
                let Ok(ft) = entry.file_type() else { continue };
                if !ft.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.is_empty() || name.starts_with('.') {
                    continue;
                }
                // Daily naming uses "<stem>.log.<date>", so we only require ".log" to be present.
                if !name.contains(".log") {
                    continue;
                }
                files.push(name);
            }
        }
        files.sort();
        files.dedup();
        files
    }
}

/// A file appender that supports time/size/both rotation.
///
/// This is designed to be used under `tracing_appender::non_blocking`, so all writes and
/// rotations happen on a single worker thread (no extra locking required).
struct RotatingFileAppender {
    dir: PathBuf,
    stem: String,
    rotation: LoggingFileRotation,
    current_name: String,
    current_size: u64,
    file: Box<dyn Write + Send>,
}

impl RotatingFileAppender {
    fn new(dir: PathBuf, stem: String, rotation: LoggingFileRotation) -> Self {
        let _ = fs::create_dir_all(&dir);
        let now = chrono::Utc::now();
        let current_name = compute_active_name(&stem, &rotation, now);
        let (file, size) = open_for_append(&dir, &current_name);
        Self {
            dir,
            stem,
            rotation,
            current_name,
            current_size: size,
            file,
        }
    }

    fn maybe_rotate(&mut self, now: chrono::DateTime<chrono::Utc>, incoming_len: usize) {
        let desired = compute_active_name(&self.stem, &self.rotation, now);
        if desired != self.current_name {
            self.current_name = desired;
            let (file, size) = open_for_append(&self.dir, &self.current_name);
            self.file = file;
            self.current_size = size;
        }

        if !rotation_has_size(&self.rotation.mode) {
            return;
        }
        let size_mb = self.rotation.size_mb.max(1);
        let limit = size_mb.saturating_mul(1024 * 1024);
        let incoming = incoming_len as u64;
        if self.current_size.saturating_add(incoming) <= limit {
            return;
        }

        self.roll_by_size();
    }

    fn roll_by_size(&mut self) {
        let keep_total = self.rotation.max_files.max(1);
        let keep_rotated = keep_total.saturating_sub(1);
        let base_path = self.dir.join(&self.current_name);

        // If we cannot keep rotated files, just truncate the active file.
        if keep_rotated == 0 {
            let (file, _) = open_for_truncate(&self.dir, &self.current_name);
            self.file = file;
            self.current_size = 0;
            return;
        }

        // Close the file handle before renaming.
        let _ = self.file.flush();

        // Shift rotated suffixes: `.N-1` -> `.N`
        for i in (1..keep_rotated).rev() {
            let src = rotated_path(&self.dir, &self.current_name, i);
            let dst = rotated_path(&self.dir, &self.current_name, i + 1);
            let _ = fs::remove_file(&dst);
            let _ = fs::rename(&src, &dst);
        }

        // Move current base to `.1`
        let first = rotated_path(&self.dir, &self.current_name, 1);
        let _ = fs::remove_file(&first);
        let _ = fs::rename(&base_path, &first);

        // Open a new active file.
        let (file, _) = open_for_truncate(&self.dir, &self.current_name);
        self.file = file;
        self.current_size = 0;
    }
}

impl Write for RotatingFileAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let now = chrono::Utc::now();
        self.maybe_rotate(now, buf.len());
        let n = self.file.write(buf)?;
        self.current_size = self.current_size.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[inline]
fn rotation_has_size(mode: &RotationMode) -> bool {
    matches!(mode, RotationMode::Size | RotationMode::Both)
}

#[inline]
fn rotation_has_time(mode: &RotationMode) -> bool {
    matches!(mode, RotationMode::Time | RotationMode::Both)
}

fn compute_active_name(
    stem: &str,
    rotation: &LoggingFileRotation,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    if rotation_has_time(&rotation.mode) {
        let suffix = match rotation.time {
            TimeRotation::Hourly => now.format("%Y-%m-%d-%H").to_string(),
            TimeRotation::Daily => now.format("%Y-%m-%d").to_string(),
        };
        format!("{stem}.{suffix}")
    } else {
        stem.to_string()
    }
}

#[inline]
fn rotated_path(dir: &Path, base_name: &str, idx: usize) -> PathBuf {
    dir.join(format!("{base_name}.{idx}"))
}

fn open_for_append(dir: &Path, name: &str) -> (Box<dyn Write + Send>, u64) {
    let path = dir.join(name);
    let file = OpenOptions::new().create(true).append(true).open(&path);
    if let Ok(f) = file {
        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
        return (Box::new(f), size);
    }
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path);
    if let Ok(f) = file {
        return (Box::new(f), 0);
    }
    (Box::new(io::sink()), 0)
}

fn open_for_truncate(dir: &Path, name: &str) -> (Box<dyn Write + Send>, u64) {
    let path = dir.join(name);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path);
    if let Ok(f) = file {
        return (Box::new(f), 0);
    }
    let file = OpenOptions::new().create(true).append(true).open(&path);
    if let Ok(f) = file {
        return (Box::new(f), 0);
    }
    (Box::new(io::sink()), 0)
}

/// Visitor that extracts `message` and formats all other fields.
///
/// # Why this exists
/// Many `tracing` events (e.g. `sqlx::query`) put the important payload into event fields
/// (like `query = "SELECT ..."`), not into the formatted `message`. The console formatter
/// prints fields by default, but our split-file layer must do it explicitly.
#[derive(Default)]
struct EventFieldsFormatter {
    /// Best-effort extracted message. May be empty if the event doesn't carry a message.
    message: String,
    /// Formatted `key=value` pairs for all non-message fields.
    ///
    /// We keep this as a single `String` to avoid per-field heap allocations.
    fields: String,
    /// Whether at least one non-message field was recorded.
    has_fields: bool,
}

impl EventFieldsFormatter {
    #[inline]
    fn push_kv(&mut self, key: &str, value: impl Display) {
        if key == log_fields::MESSAGE {
            // `message` is handled separately to keep output stable.
            return;
        }

        if !self.has_fields {
            self.has_fields = true;
        } else {
            let _ = self.fields.write_str(", ");
        }

        let _ = write!(&mut self.fields, "{}={}", key, value);
    }
}

impl Visit for EventFieldsFormatter {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == log_fields::MESSAGE {
            self.message.clear();
            self.message.push_str(value);
            return;
        }
        // Quote string-ish fields for readability and to match common tracing output style.
        self.push_kv(field.name(), format_args!("{value:?}"));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push_kv(field.name(), value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push_kv(field.name(), value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push_kv(field.name(), value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == log_fields::MESSAGE {
            self.message = format!("{value:?}");
            return;
        }
        self.push_kv(field.name(), format_args!("{value:?}"));
    }
}

/// Split file layer that routes logs to different files based on driver_type.
pub struct SplitFileLayer {
    registry: Arc<SplitFileRegistry>,
    format: LoggingFormat,
    include_span_fields: bool,
}

impl SplitFileLayer {
    /// Create a new split file layer.
    pub fn new(log_dir: PathBuf, rotation: LoggingFileRotation) -> Self {
        Self {
            registry: Arc::new(SplitFileRegistry::new(log_dir, rotation)),
            format: LoggingFormat::Text,
            include_span_fields: true,
        }
    }

    #[inline]
    pub fn set_format(&mut self, format: LoggingFormat) {
        self.format = format;
    }

    #[inline]
    pub fn set_include_span_fields(&mut self, enabled: bool) {
        self.include_span_fields = enabled;
    }

    /// Get the registry for listing log files.
    pub fn registry(&self) -> Arc<SplitFileRegistry> {
        Arc::clone(&self.registry)
    }
}

impl<S> Layer<S> for SplitFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Determine which file to write to.
        //
        // Hot path: routing info is cached in span extensions.
        let route_ext: Option<LogRouteExt> = ctx
            .lookup_current()
            .and_then(|span| span.extensions().get::<LogRouteExt>().cloned())
            .or(None);

        let mut source: Option<LogSource> = route_ext.as_ref().and_then(|e| e.source);
        let mut driver_type: Option<Arc<str>> = route_ext
            .as_ref()
            .and_then(|e| e.driver_type.as_ref().map(Arc::clone));
        let mut plugin_type: Option<Arc<str>> = route_ext
            .as_ref()
            .and_then(|e| e.plugin_type.as_ref().map(Arc::clone));

        // Fallback: if extensions are missing/incomplete, parse event fields directly.
        if source.is_none() || (driver_type.is_none() && plugin_type.is_none()) {
            let mut v = LogRouteVisitor::default();
            event.record(&mut v);
            if source.is_none() {
                source = v.source;
            }
            if driver_type.is_none() {
                driver_type = v.driver_type;
            }
            if plugin_type.is_none() {
                plugin_type = v.plugin_type;
            }
        }

        // Get appropriate writer
        let mut writer = match (source, driver_type, plugin_type) {
            (Some(LogSource::Driver), Some(dt), _) => self.registry.get_or_create_driver_writer(dt),
            (Some(LogSource::Plugin), _, Some(pt)) => self.registry.get_or_create_plugin_writer(pt),
            _ => self.registry.get_host_writer(),
        };

        // Format and write the event
        let now = chrono::Utc::now();
        let level = event.metadata().level();
        let target = event.metadata().target();

        // Collect message + fields (best-effort).
        let mut ev_visitor = EventFieldsFormatter::default();
        event.record(&mut ev_visitor);

        // Optional span fields (best-effort): channel_id + span stack names.
        let (channel_id, spans) = if self.include_span_fields {
            let mut names: Vec<&'static str> = Vec::new();
            let mut chan: Option<i32> = None;
            if let Some(span) = ctx.lookup_current() {
                // Walk from current to root.
                let mut cur = Some(span);
                while let Some(s) = cur {
                    names.push(s.metadata().name());
                    if chan.is_none() {
                        let exts = s.extensions();
                        chan = exts.get::<ChannelIdExt>().and_then(|e| e.0);
                    }
                    cur = s.parent();
                }
            }
            names.reverse();
            (chan, Some(names))
        } else {
            (None, None)
        };

        let line = match self.format {
            LoggingFormat::Text => {
                // Keep close to console output:
                // - `target` (e.g. `sqlx::query`)
                // - `message` (if present)
                // - event fields (if present)
                // - optional span context
                let span_part = if let Some(spans) = &spans {
                    if spans.is_empty() && channel_id.is_none() {
                        String::new()
                    } else {
                        format!(
                            " [spans={}]{}",
                            spans.join("->"),
                            channel_id
                                .map(|v| format!(" channel_id={v}"))
                                .unwrap_or_default()
                        )
                    }
                } else {
                    String::new()
                };

                if ev_visitor.has_fields {
                    format!(
                        "{} [{}] {}: {} {{{}}}{}\n",
                        now.format("%Y-%m-%d %H:%M:%S%.3f"),
                        level.as_str(),
                        target,
                        ev_visitor.message,
                        ev_visitor.fields,
                        span_part
                    )
                } else {
                    format!(
                        "{} [{}] {}: {}{}\n",
                        now.format("%Y-%m-%d %H:%M:%S%.3f"),
                        level.as_str(),
                        target,
                        ev_visitor.message,
                        span_part
                    )
                }
            }
            LoggingFormat::Json => {
                // Low-cardinality JSON line (best-effort field encoding).
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "ts".to_string(),
                    serde_json::Value::String(
                        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    ),
                );
                obj.insert(
                    "level".to_string(),
                    serde_json::Value::String(level.as_str().to_string()),
                );
                obj.insert(
                    "target".to_string(),
                    serde_json::Value::String(target.to_string()),
                );
                obj.insert(
                    "message".to_string(),
                    serde_json::Value::String(ev_visitor.message.clone()),
                );
                if ev_visitor.has_fields {
                    obj.insert(
                        "fields".to_string(),
                        serde_json::Value::String(ev_visitor.fields.clone()),
                    );
                }
                if let Some(v) = channel_id {
                    obj.insert(
                        "channel_id".to_string(),
                        serde_json::Value::Number(v.into()),
                    );
                }
                if let Some(spans) = spans {
                    obj.insert(
                        "spans".to_string(),
                        serde_json::Value::Array(
                            spans
                                .into_iter()
                                .map(|s| serde_json::Value::String(s.to_string()))
                                .collect(),
                        ),
                    );
                }
                serde_json::Value::Object(obj).to_string() + "\n"
            }
        };

        // Write to the non-blocking writer
        // NonBlocking implements Write and is thread-safe (uses internal channel)
        let _ = write!(writer, "{}", line);
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        // Always enabled (filtering is done by the filter layer)
        metadata.level() <= &Level::TRACE
    }

    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }
}
