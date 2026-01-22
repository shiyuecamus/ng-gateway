//! Prometheus metrics for NG Gateway.
//!
//! This module provides a **single, process-wide** Prometheus `Registry` and a
//! small set of core metrics required by the observability plan.
//!
//! # Design goals
//! - **Low overhead**: update metrics on scrape (pull model).
//! - **Low cardinality by default**: do not put device/point identifiers into labels.
//! - **One registry**: all crates register metrics into the same registry.

pub mod queue;
mod system;

use ng_gateway_error::{NGError, NGResult};
use once_cell::sync::Lazy;
use prometheus::{Encoder, Registry, TextEncoder};
use tracing::warn;

/// A pre-encoded Prometheus text payload.
///
/// This is designed to keep `ng-gateway-web` free from `prometheus` crate dependency.
#[derive(Debug, Clone)]
pub struct PrometheusTextPayload {
    /// The HTTP content type for Prometheus text exposition format.
    pub content_type: String,
    /// The response body in Prometheus text exposition format.
    pub body: Vec<u8>,
}

/// Global Prometheus registry for the gateway process.
pub(crate) static REGISTRY: Lazy<Registry> = Lazy::new(|| {
    match Registry::new_custom(Some("ng_gateway".into()), None) {
        Ok(registry) => registry,
        Err(e) => {
            warn!(error=%e, "Failed to create custom Prometheus registry, falling back to default");
            Registry::new()
        }
    }
});

/// Expose the global Prometheus registry so other crates can register their metrics.
#[inline]
pub fn registry() -> &'static Registry {
    &REGISTRY
}

/// Gather all metrics in Prometheus text exposition format.
///
/// # Notes
/// - Core system metrics are updated right before encoding.
/// - Queue depth gauges are refreshed right before encoding.
/// - The caller is expected to expose the returned payload at `GET /metrics`.
pub fn gather_prometheus_text() -> NGResult<PrometheusTextPayload> {
    system::update_system_metrics();
    queue::refresh_all_queue_depths();

    let metric_families = REGISTRY.gather();

    let encoder = TextEncoder::new();
    let mut buffer = Vec::with_capacity(8 * 1024);
    encoder
        .encode(&metric_families, &mut buffer)
        .map_err(|e| NGError::from(format!("Failed to encode Prometheus metric families: {e}")))?;

    Ok(PrometheusTextPayload {
        content_type: encoder.format_type().to_string(),
        body: buffer,
    })
}
