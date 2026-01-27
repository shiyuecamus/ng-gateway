//! Host-side logging subscriber installation (console + rolling file).
//!
//! This module is the single source of truth for the host process logging setup.
//! It installs a `tracing_subscriber` with:
//! - Console output
//! - Rolling file output

use super::{
    control,
    span_ext::{ChannelIdExt, ChannelIdLayer},
    DriverFileRegistry, DriverTypeExtractorLayer, SplitFileLayer,
};
use arc_swap::ArcSwapOption;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::{domain::prelude::LogLevel, settings::LoggingOutput};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};
use tracing::{
    subscriber::{set_global_default, Interest},
    Level, Metadata, Subscriber,
};
use tracing_subscriber::{
    filter::Filtered,
    fmt::{self},
    layer::{Context as FilterContext, Filter, SubscriberExt},
    registry::LookupSpan,
    reload, Layer, Registry,
};

type ReloadableFileLayer = Option<Filtered<SplitFileLayer, LogFilter, Registry>>;
type FileLayerHandle = reload::Handle<ReloadableFileLayer, Registry>;

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
    // Mapping uses `ng_gateway_models::domain::system_settings::LogLevel`:
    // - 0=ERROR, 1=WARN, 2=INFO, 3=DEBUG, 4=TRACE
    level: Arc<AtomicU8>,
    split_file_registry: ArcSwapOption<DriverFileRegistry>,
    file_layer_handle: Option<FileLayerHandle>,
    filter: Option<LogFilter>,
}

#[allow(unused)]
impl Logger {
    /// Create a new logger handle.
    pub fn new(level: Option<Level>) -> Self {
        let level = level.unwrap_or(Level::INFO);
        let level_u8: u8 = LogLevel::from(level).into();
        Self {
            level: Arc::new(AtomicU8::new(level_u8)),
            split_file_registry: ArcSwapOption::empty(),
            file_layer_handle: None,
            filter: None,
        }
    }

    /// Get the split file registry for listing log files.
    pub fn split_file_registry(&self) -> Option<Arc<DriverFileRegistry>> {
        self.split_file_registry.load_full()
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
    pub fn initialize(&mut self, output: &LoggingOutput) -> NGResult<()> {
        if let Some(rt) = control::global() {
            let base_u8 = self.level.load(Ordering::Relaxed);
            rt.overrides().set_base_level(LogLevel::from(base_u8));
        }

        let filter = LogFilter::new(Arc::clone(&self.level));
        self.filter = Some(filter.clone());

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

        let initial_file_layer = if output.file.enabled {
            let log_dir = PathBuf::from(output.file.dir.as_str());
            let mut split = SplitFileLayer::new(log_dir, output.file.rotation.clone());
            split.set_format(output.format.clone());
            split.set_include_span_fields(output.include_span_fields);
            let registry = split.registry();
            self.split_file_registry.store(Some(registry));
            Some(split.with_filter(filter.clone()))
        } else {
            self.split_file_registry.store(None);
            None
        };

        let (file_layer, file_handle) = reload::Layer::new(initial_file_layer);
        self.file_layer_handle = Some(file_handle);

        // Important: install the reload layer first so it binds to `Registry`.
        // Subsequent `.with(...)` calls wrap it, and the reload layer does not need to implement
        // `Layer<Layered<...>>`.
        let subscriber = Registry::default()
            // Split file layer: logs are routed to host.log or {driver_type}.log
            .with(file_layer)
            // Install a tiny span layer to cache `channel_id` for the filter.
            .with(ChannelIdLayer)
            // Install driver_type extractor layer to cache driver_type in spans.
            .with(DriverTypeExtractorLayer)
            // Console layer: all logs go to console
            .with(console_layer);

        set_global_default(subscriber).map_err(|_| NGError::from("Failed to set logger"))?;
        Ok(())
    }

    /// Reload the logging output pipeline (best-effort).
    ///
    /// # Semantics
    /// - Can toggle file output on/off
    /// - Can change file dir
    /// - Can switch text/json and include_span_fields
    ///
    /// # Note
    /// This does **not** rebuild the global subscriber; it reloads the file layer in-place.
    pub fn reload_output(&self, output: &LoggingOutput) -> NGResult<()> {
        let Some(handle) = self.file_layer_handle.as_ref() else {
            return Err(NGError::from("Logger runtime is not initialized"));
        };
        let Some(filter) = self.filter.as_ref() else {
            return Err(NGError::from("Logger runtime is not initialized"));
        };

        let next_layer = if output.file.enabled {
            let log_dir = PathBuf::from(output.file.dir.as_str());
            let mut split = SplitFileLayer::new(log_dir, output.file.rotation.clone());
            split.set_format(output.format.clone());
            split.set_include_span_fields(output.include_span_fields);
            let registry = split.registry();
            self.split_file_registry.store(Some(registry));
            Some(split.with_filter(filter.clone()))
        } else {
            self.split_file_registry.store(None);
            None
        };

        handle
            .reload(next_layer)
            .map_err(|e| NGError::from(format!("Failed to reload logging output: {e}")))?;
        Ok(())
    }
}
