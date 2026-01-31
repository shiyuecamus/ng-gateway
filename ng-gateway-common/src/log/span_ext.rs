//! Shared span extensions for logging pipeline.
//!
//! This module is used by both:
//! - the dynamic filter (`LogFilter`) to resolve per-channel overrides
//! - the file output layer to optionally include span fields (channel_id, etc.)

use ng_gateway_sdk::log::fields::{APP_ID, CHANNEL_ID};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{layer::Context as FilterContext, registry::LookupSpan, Layer};

/// Span extension: cached `channel_id` for per-channel filtering.
///
/// This is intentionally tiny (single i32) to keep hot-path overhead minimal.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelIdExt(pub Option<i32>);

/// Span extension: cached `app_id` for per-app filtering.
///
/// This is intentionally tiny (single i32) to keep hot-path overhead minimal.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppIdExt(pub Option<i32>);

/// A tiny `tracing` layer that records `channel_id` from span fields into extensions.
///
/// This enables per-channel dynamic log filtering without requiring heavy JSON field caching.
#[derive(Default)]
pub struct ChannelIdLayer;

impl<S> Layer<S> for ChannelIdLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
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

/// A tiny `tracing` layer that records `app_id` from span fields into extensions.
///
/// This enables per-app dynamic log filtering based on span context.
#[derive(Default)]
pub struct AppIdLayer;

impl<S> Layer<S> for AppIdLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: FilterContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut v = AppIdVisitor::default();
        attrs.record(&mut v);

        // Inherit from ancestors to keep app context stable across nested spans.
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

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: FilterContext<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut exts = span.extensions_mut();
        let mut v = AppIdVisitor::default();
        values.record(&mut v);
        if v.app_id.is_none() {
            return;
        }
        if let Some(ext) = exts.get_mut::<AppIdExt>() {
            ext.0 = v.app_id;
        } else {
            exts.insert(AppIdExt(v.app_id));
        }
    }
}

#[derive(Default)]
struct AppIdVisitor {
    app_id: Option<i32>,
}

impl Visit for AppIdVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == APP_ID {
            self.app_id = Some(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == APP_ID {
            self.app_id = Some((value.min(i32::MAX as u64)) as i32);
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}
