//! Host-side logging subscriber installation (console + rolling file + realtime layer).
//!
//! This module is the single source of truth for the host process logging setup.
//! It installs a `tracing_subscriber` with:
//! - Console output
//! - Rolling file output
//! - Realtime UI streaming layer (hot-pluggable via `log::runtime`)

use crate::log::{
    realtime::layer::{CachedSpanFields, RealtimeLogLayer},
    runtime,
};
use ng_gateway_error::{NGError, NGResult};
use std::sync::{Arc, Mutex};
use tracing::{
    subscriber::{set_global_default, Interest},
    Level, Metadata,
};
use tracing_appender::{non_blocking::WorkerGuard, rolling};
use tracing_subscriber::{
    fmt::{self},
    layer::SubscriberExt,
    layer::{Context as FilterContext, Filter},
    registry::LookupSpan,
    Layer, Registry,
};

/// Dynamic log filter used by all layers (console/file/realtime).
///
/// # Notes
/// We implement `Filter<S>` directly so the filter remains valid across subscriber stacks
/// and can inspect the current span context (`channel_id`) for per-channel overrides.
#[derive(Clone)]
struct LogFilter {
    level: Arc<Mutex<Level>>,
}

impl LogFilter {
    #[inline]
    fn new(level: Arc<Mutex<Level>>) -> Self {
        Self { level }
    }
}

impl<S> Filter<S> for LogFilter
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &tracing::Metadata<'_>, ctx: &FilterContext<'_, S>) -> bool {
        let base = *self.level.lock().unwrap();
        let mut effective = base;

        if let Some(rt) = runtime::global() {
            let overrides = rt.overrides();
            let global: Level = overrides.effective_global_level().into();
            if global > effective {
                effective = global;
            }

            if let Some(span) = ctx.lookup_current() {
                if let Some(cached) = span.extensions().get::<CachedSpanFields>() {
                    if let Some(channel_id) = cached.channel_id {
                        let ch: Level = overrides.effective_channel_level(channel_id).into();
                        if ch > effective {
                            effective = ch;
                        }
                    }
                }
            }
        }

        metadata.level() <= &effective
    }

    fn callsite_enabled(&self, _metadata: &'static Metadata<'static>) -> Interest {
        // Always allow dynamic decision at runtime.
        Interest::sometimes()
    }
}

/// Host logger handle.
///
/// # Responsibilities
/// - Hold the current base level (used by the dynamic filter)
/// - Hold file appender guard (to flush logs on shutdown)
pub struct Logger {
    level: Arc<Mutex<Level>>,
    _file_guard: Option<WorkerGuard>,
}

#[allow(unused)]
impl Logger {
    /// Create a new logger handle.
    pub fn new(level: Option<Level>) -> Self {
        Self {
            level: Arc::new(Mutex::new(level.unwrap_or(Level::INFO))),
            _file_guard: None,
        }
    }

    /// Set the baseline (non-lease) log level.
    pub fn set_level(&self, new_level: Level) {
        let mut level = self.level.lock().unwrap();
        *level = new_level;

        // Keep override manager in sync (best-effort).
        if let Some(rt) = runtime::global() {
            rt.overrides().set_base_level(new_level.into());
        }
    }

    /// Get the current baseline log level.
    pub fn get_level(&self) -> Level {
        *self.level.lock().unwrap()
    }

    /// Install the global tracing subscriber.
    ///
    /// # Important
    /// - This must only be called once per process.
    /// - `log::runtime::init(...)` must have been called before this, so the realtime layer
    ///   can be installed in a hot-pluggable way.
    pub fn initialize(&mut self) -> NGResult<()> {
        let file_appender = rolling::daily("logs", "ng.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        self._file_guard = Some(_guard);

        if let Some(rt) = runtime::global() {
            let base = *self.level.lock().unwrap();
            rt.overrides().set_base_level(base.into());
        }

        let filter = LogFilter::new(Arc::clone(&self.level));

        let console_layer = {
            #[cfg(debug_assertions)]
            let mut layer = fmt::layer().pretty().with_writer(std::io::stdout);
            #[cfg(not(debug_assertions))]
            let mut layer = fmt::layer().with_writer(std::io::stdout);

            #[cfg(debug_assertions)]
            {
                layer = layer.with_file(true).with_line_number(true);
            }
            #[cfg(not(debug_assertions))]
            {
                layer = layer.with_file(false).with_line_number(false);
            }

            layer.with_filter(filter.clone())
        };

        let file_layer = {
            #[cfg(debug_assertions)]
            let mut layer = fmt::layer()
                .pretty()
                .with_writer(non_blocking)
                .with_ansi(false);
            #[cfg(not(debug_assertions))]
            let mut layer = fmt::layer().with_writer(non_blocking).with_ansi(false);

            #[cfg(debug_assertions)]
            {
                layer = layer.with_file(true).with_line_number(true);
            }
            #[cfg(not(debug_assertions))]
            {
                layer = layer.with_file(false).with_line_number(false);
            }

            layer.with_filter(filter.clone())
        };

        // Always attach realtime layer (it is a no-op when disabled).
        let realtime_layer = RealtimeLogLayer::new().with_filter(filter);

        let subscriber = Registry::default()
            .with(console_layer)
            .with(file_layer)
            .with(realtime_layer);

        set_global_default(subscriber).map_err(|_| NGError::from("Failed to set logger"))?;
        Ok(())
    }
}
