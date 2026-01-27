//! Host-side logging subscriber installation (console + rolling file).
//!
//! This module is the single source of truth for the host process logging setup.
//! It installs a `tracing_subscriber` with:
//! - Console output
//! - Rolling file output

use super::{control, DriverFileRegistry, DriverTypeExtractorLayer, SplitFileLayer};
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{constants::LOG_DIR, domain::prelude::LogLevel};
use ng_gateway_sdk::log::fields::CHANNEL_ID;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    subscriber::{set_global_default, Interest},
    Level, Metadata, Subscriber,
};
use tracing_subscriber::{
    fmt::{self},
    layer::SubscriberExt,
    layer::{Context as FilterContext, Filter},
    registry::LookupSpan,
    Layer, Registry,
};

/// Span extension: cached `channel_id` for per-channel filtering.
///
/// This is intentionally tiny (single i32) to keep hot-path overhead minimal.
#[derive(Debug, Clone, Copy, Default)]
struct ChannelIdExt(Option<i32>);

/// A tiny `tracing` layer that records `channel_id` from span fields into extensions.
///
/// This enables per-channel dynamic log filtering without requiring heavy JSON field caching.
#[derive(Default)]
struct ChannelIdLayer;

impl<S> Layer<S> for ChannelIdLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: FilterContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = ChannelIdVisitor::default();
        attrs.record(&mut v);
        span.extensions_mut().insert(ChannelIdExt(v.channel_id));
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: FilterContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut exts = span.extensions_mut();
        let mut v = ChannelIdVisitor::default();
        values.record(&mut v);
        if v.channel_id.is_none() {
            return;
        }
        if let Some(ext) = exts.get_mut::<ChannelIdExt>() {
            ext.0 = v.channel_id;
        } else {
            exts.insert(ChannelIdExt(v.channel_id));
        }
    }
}

#[derive(Default)]
struct ChannelIdVisitor {
    channel_id: Option<i32>,
}

impl Visit for ChannelIdVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == CHANNEL_ID {
            self.channel_id = Some(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == CHANNEL_ID {
            self.channel_id = Some((value.min(i32::MAX as u64)) as i32);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

/// Dynamic log filter used by all layers (console/file/realtime).
///
/// # Notes
/// We implement `Filter<S>` directly so the filter remains valid across subscriber stacks
/// and can inspect the current span context (`channel_id`) for per-channel overrides.
#[derive(Clone)]
struct LogFilter {
    level: Arc<AtomicU8>,
}

impl LogFilter {
    #[inline]
    fn new(level: Arc<AtomicU8>) -> Self {
        Self { level }
    }
}

impl<S> Filter<S> for LogFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, metadata: &Metadata<'_>, ctx: &FilterContext<'_, S>) -> bool {
        // Baseline (used when log control runtime isn't available).
        let base_u8 = self.level.load(Ordering::Relaxed);
        let mut effective: Level = LogLevel::from(base_u8).into();

        // Semantics:
        // - Global effective level comes from the override manager (baseline + global overrides).
        // - If a per-channel override exists, it **overrides** the global level for that channel
        //   (it can both raise and lower verbosity).
        if let Some(rt) = control::global() {
            let overrides = rt.overrides();
            effective = overrides.effective_global_level().into();

            if let Some(span) = ctx.lookup_current() {
                // Scope `Extensions` borrow to avoid holding it across this block.
                let channel_id = {
                    let exts = span.extensions();
                    exts.get::<ChannelIdExt>().and_then(|e| e.0)
                };
                if let Some(channel_id) = channel_id {
                    effective = overrides.effective_channel_level(channel_id).into();
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
/// - Hold split file layer registry (for listing log files)
pub struct Logger {
    // Stored as u8 to keep the hot-path lock-free.
    //
    // Mapping uses `ng_gateway_models::domain::logging::LogLevel`:
    // - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
    level: Arc<AtomicU8>,
    split_file_registry: Option<Arc<DriverFileRegistry>>,
}

#[allow(unused)]
impl Logger {
    /// Create a new logger handle.
    pub fn new(level: Option<Level>) -> Self {
        let level = level.unwrap_or(Level::INFO);
        let level_u8: u8 = LogLevel::from(level).into();
        Self {
            level: Arc::new(AtomicU8::new(level_u8)),
            split_file_registry: None,
        }
    }

    /// Get the split file registry for listing log files.
    pub fn split_file_registry(&self) -> Option<Arc<DriverFileRegistry>> {
        self.split_file_registry.clone()
    }

    /// Set the baseline (non-lease) log level.
    pub fn set_level(&self, new_level: Level) {
        let new_u8: u8 = LogLevel::from(new_level).into();
        self.level.store(new_u8, Ordering::Relaxed);

        // Keep override manager in sync (best-effort).
        if let Some(rt) = control::global() {
            rt.overrides().set_base_level(LogLevel::from(new_level));
        }
    }

    /// Get the current baseline log level.
    pub fn get_level(&self) -> Level {
        let u8 = self.level.load(Ordering::Relaxed);
        LogLevel::from(u8).into()
    }

    /// Install the global tracing subscriber.
    ///
    /// # Important
    /// - This must only be called once per process.
    /// - `log::control::init(...)` must have been called before this, so overrides are available.
    pub fn initialize(&mut self) -> NGResult<()> {
        if let Some(rt) = control::global() {
            let base_u8 = self.level.load(Ordering::Relaxed);
            rt.overrides().set_base_level(LogLevel::from(base_u8));
        }

        let filter = LogFilter::new(Arc::clone(&self.level));

        // Console layer: output all logs to console
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

        // Split file layer: route logs to different files based on driver_type
        let log_dir = PathBuf::from(LOG_DIR);
        let split_file_layer = SplitFileLayer::new(log_dir);
        let registry = split_file_layer.registry();
        self.split_file_registry = Some(Arc::clone(&registry));

        let subscriber = Registry::default()
            // Install a tiny span layer to cache `channel_id` for the filter.
            .with(ChannelIdLayer)
            // Install driver_type extractor layer to cache driver_type in spans.
            .with(DriverTypeExtractorLayer)
            // Console layer: all logs go to console
            .with(console_layer)
            // Split file layer: logs are routed to host.log or {driver_type}.log
            .with(split_file_layer.with_filter(filter.clone()));

        set_global_default(subscriber).map_err(|_| NGError::from("Failed to set logger"))?;
        Ok(())
    }
}
