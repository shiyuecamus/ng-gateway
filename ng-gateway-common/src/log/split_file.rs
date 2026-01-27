//! Split file logging layer that routes logs to different files based on driver_type.
//!
//! This module implements a custom tracing layer that:
//! - Routes host logs (no driver_type) to `host.log`
//! - Routes driver logs (with driver_type) to `{driver_type}.log`
//! - Uses rolling file appenders for each log file
//! - Maintains thread-safe writer handles for each driver type

use dashmap::{DashMap, Entry};
use ng_gateway_sdk::log::fields as log_fields;
use std::{fmt::Write as _, io::Write, path::PathBuf, sync::Arc};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    subscriber::Interest,
    Event, Level, Metadata, Subscriber,
};
use tracing_appender::{
    non_blocking::{NonBlocking, WorkerGuard},
    rolling,
};
use tracing_subscriber::{layer::Context, registry::LookupSpan, Layer};

/// Sanitize a driver type label into a safe file stem.
///
/// # Security
/// Driver type can originate from external plugins/drivers. We must prevent:
/// - path traversal (`../`)
/// - path separators (`/`, `\`)
/// - control characters
///
/// # Rules
/// - Keep only ASCII `[a-z0-9_-]` (lowercased)
/// - Replace other characters with `_`
/// - Trim to a reasonable length to avoid filesystem/path issues
#[inline]
fn sanitize_driver_type(raw: &str) -> Arc<str> {
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

/// Extension to store driver_type in span extensions for efficient lookup.
#[derive(Debug, Clone, Default)]
struct DriverTypeExt(Option<Arc<str>>);

/// Visitor to extract driver_type from event fields.
#[derive(Default)]
struct DriverTypeVisitor {
    driver_type: Option<Arc<str>>,
    is_driver_source: bool,
}

impl Visit for DriverTypeVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            log_fields::DRIVER_TYPE => {
                self.driver_type = Some(sanitize_driver_type(value));
            }
            log_fields::SOURCE => {
                self.is_driver_source = value == log_fields::SOURCE_DRIVER;
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        // Best-effort: capture string-ish debug values. This makes the extractor resilient
        // even if callers log `driver_type = %...` instead of `driver_type = "..."`.
        match field.name() {
            log_fields::DRIVER_TYPE => {
                let mut s = format!("{value:?}");
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    s = s[1..s.len() - 1].to_string();
                }
                if !s.is_empty() {
                    self.driver_type = Some(sanitize_driver_type(&s));
                }
            }
            log_fields::SOURCE => {
                let mut s = format!("{value:?}");
                if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                    s = s[1..s.len() - 1].to_string();
                }
                self.is_driver_source = s == log_fields::SOURCE_DRIVER;
            }
            _ => {}
        }
    }
}

/// Layer that extracts driver_type from spans and caches it.
#[derive(Default)]
pub struct DriverTypeExtractorLayer;

impl<S> Layer<S> for DriverTypeExtractorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = DriverTypeVisitor::default();
        attrs.record(&mut visitor);
        if visitor.driver_type.is_some() || visitor.is_driver_source {
            span.extensions_mut()
                .insert(DriverTypeExt(visitor.driver_type));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut visitor = DriverTypeVisitor::default();
        values.record(&mut visitor);
        if visitor.driver_type.is_some() || visitor.is_driver_source {
            let mut exts = span.extensions_mut();
            if let Some(ext) = exts.get_mut::<DriverTypeExt>() {
                ext.0 = visitor.driver_type;
            } else {
                exts.insert(DriverTypeExt(visitor.driver_type));
            }
        }
    }
}

/// Thread-safe registry of file writers for different driver types.
pub struct DriverFileRegistry {
    writers: DashMap<Arc<str>, NonBlocking>,
    guards: DashMap<Arc<str>, WorkerGuard>,
    log_dir: PathBuf,
}

impl DriverFileRegistry {
    fn new(log_dir: PathBuf) -> Self {
        // Ensure log directory exists (best-effort). This avoids surprising runtime failures
        // when the logger initializes before runtime directories are created.
        let _ = std::fs::create_dir_all(&log_dir);
        Self {
            writers: DashMap::new(),
            guards: DashMap::new(),
            log_dir,
        }
    }

    /// Get or create a writer for a specific driver type.
    fn get_or_create_writer(&self, driver_type: Arc<str>) -> NonBlocking {
        match self.writers.entry(Arc::clone(&driver_type)) {
            Entry::Occupied(o) => o.get().clone(),
            Entry::Vacant(v) => {
                // Create new writer exactly once per driver type.
                let appender = rolling::daily(&self.log_dir, format!("{}.log", &*driver_type));
                let (non_blocking, guard) = tracing_appender::non_blocking(appender);

                // Keep guard alive to avoid dropping the worker thread.
                self.guards.insert(Arc::clone(v.key()), guard);
                v.insert(non_blocking.clone());
                non_blocking
            }
        }
    }

    /// Get the host log writer (host.log).
    fn get_host_writer(&self) -> NonBlocking {
        const HOST_KEY: &str = "__host__";

        match self.writers.entry(Arc::<str>::from(HOST_KEY)) {
            Entry::Occupied(o) => o.get().clone(),
            Entry::Vacant(v) => {
                let appender = rolling::daily(&self.log_dir, "host.log");
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
        if let Ok(entries) = std::fs::read_dir(&self.log_dir) {
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
    fn push_kv(&mut self, key: &str, value: impl std::fmt::Display) {
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

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == log_fields::MESSAGE {
            self.message = format!("{value:?}");
            return;
        }
        self.push_kv(field.name(), format_args!("{value:?}"));
    }
}

/// Split file layer that routes logs to different files based on driver_type.
pub struct SplitFileLayer {
    registry: Arc<DriverFileRegistry>,
}

impl SplitFileLayer {
    /// Create a new split file layer.
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            registry: Arc::new(DriverFileRegistry::new(log_dir)),
        }
    }

    /// Get the registry for listing log files.
    pub fn registry(&self) -> Arc<DriverFileRegistry> {
        Arc::clone(&self.registry)
    }
}

impl<S> Layer<S> for SplitFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Determine which file to write to
        // Driver logs should hit the fast path: driver_type lives in span extensions as `Arc<str>`.
        let driver_type: Option<Arc<str>> = ctx
            .lookup_current()
            .and_then(|span| {
                span.extensions()
                    .get::<DriverTypeExt>()
                    .and_then(|ext| ext.0.as_ref().map(Arc::clone))
            })
            .or_else(|| {
                // Fallback: check event fields directly
                let mut visitor = DriverTypeVisitor::default();
                event.record(&mut visitor);
                visitor.driver_type
            });

        // Get appropriate writer
        let mut writer = if let Some(dt) = driver_type {
            self.registry.get_or_create_writer(dt)
        } else {
            self.registry.get_host_writer()
        };

        // Format and write the event
        let now = chrono::Utc::now();
        let level = event.metadata().level();
        let target = event.metadata().target();

        // Collect message + fields (best-effort).
        let mut ev_visitor = EventFieldsFormatter::default();
        event.record(&mut ev_visitor);

        // Format the log line.
        //
        // We intentionally keep this close to the console output:
        // - `target` (e.g. `sqlx::query`)
        // - `message` (if present)
        // - event fields (if present)
        let line = if ev_visitor.has_fields {
            format!(
                "{} [{}] {}: {} {{{}}}\n",
                now.format("%Y-%m-%d %H:%M:%S%.3f"),
                level.as_str(),
                target,
                ev_visitor.message,
                ev_visitor.fields
            )
        } else {
            format!(
                "{} [{}] {}: {}\n",
                now.format("%Y-%m-%d %H:%M:%S%.3f"),
                level.as_str(),
                target,
                ev_visitor.message
            )
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
