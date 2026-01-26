//! Realtime log layer (LogHub ingestion).
//!
//! This layer is installed into the host tracing subscriber unconditionally.
//! It becomes a no-op when realtime logs are disabled via `log::runtime`.

use super::{
    super::{fields, runtime},
    hub::{LogEvent, LogSource, LogSpan},
};
use serde_json::{Map, Value};
use std::{error::Error as StdError, fmt};
use tracing::{
    field::{Field, Visit},
    span::{Attributes, Id, Record},
    Event, Subscriber,
};
use tracing_subscriber::{
    layer::Context,
    registry::{LookupSpan, SpanRef},
    Layer,
};

/// Span extension key: cached span fields for later event attribution.
#[derive(Debug, Clone, Default)]
pub(crate) struct CachedSpanFields {
    pub(crate) fields: Map<String, Value>,
    pub(crate) channel_id: Option<i32>,
}

/// A `tracing` layer that writes events into the current `LogHub` (when enabled).
pub struct RealtimeLogLayer;

impl RealtimeLogLayer {
    /// Create a new layer.
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealtimeLogLayer {
    /// Create a default `RealtimeLogLayer`.
    ///
    /// This is equivalent to calling [`RealtimeLogLayer::new`].
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for RealtimeLogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = JsonVisitor::default();
        attrs.record(&mut visitor);

        let channel_id = fields::map_i32(&visitor.fields, fields::CHANNEL_ID);

        span.extensions_mut().insert(CachedSpanFields {
            fields: visitor.fields,
            channel_id,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut exts = span.extensions_mut();
        if let Some(cached) = exts.get_mut::<CachedSpanFields>() {
            let mut visitor = JsonVisitor::default();
            values.record(&mut visitor);
            for (k, v) in visitor.fields.into_iter() {
                cached.fields.insert(k, v);
            }
            cached.channel_id = fields::map_i32(&cached.fields, fields::CHANNEL_ID);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(rt) = runtime::global() else {
            return;
        };
        let Some(hub) = rt.hub() else {
            return;
        };

        let settings = rt.settings();
        let max_bytes = settings.event_max_bytes.max(256);

        let meta = event.metadata();
        let mut visitor = JsonVisitor::default();
        event.record(&mut visitor);

        let message = visitor
            .fields
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let message = truncate_utf8(&message, max_bytes);

        let current_span: Option<SpanRef<'_, S>> = ctx.lookup_current();
        let (span_info, channel_from_span) = current_span
            .as_ref()
            .and_then(|s| {
                let exts = s.extensions();
                exts.get::<CachedSpanFields>().map(|cached| {
                    let span = LogSpan {
                        name: s.metadata().name().to_string(),
                        fields: if cached.fields.is_empty() {
                            None
                        } else {
                            Some(cached.fields.clone())
                        },
                    };
                    (Some(span), cached.channel_id)
                })
            })
            .unwrap_or((None, None));

        let channel_id =
            channel_from_span.or_else(|| fields::map_i32(&visitor.fields, fields::CHANNEL_ID));

        let source = match visitor.fields.get("source").and_then(|v| v.as_str()) {
            Some("driver") => LogSource::Driver,
            _ => LogSource::Host,
        };

        let fields = if visitor.fields.is_empty() {
            None
        } else {
            Some(visitor.fields)
        };

        let ev = LogEvent {
            ts: chrono::Utc::now().timestamp_millis(),
            level: meta.level().into(),
            target: meta.target().to_string(),
            message,
            source,
            channel_id,
            fields,
            span: span_info,
        };

        hub.push(ev);
    }
}

/// Record `tracing` fields into JSON values.
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
    fn record_error(&mut self, field: &Field, value: &(dyn StdError + 'static)) {
        self.fields
            .insert(field.name().to_string(), Value::from(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields
            .insert(field.name().to_string(), Value::from(format!("{value:?}")));
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
