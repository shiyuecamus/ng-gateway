//! Prometheus metrics endpoint.
//!
//! This module exposes the gateway's Prometheus metrics at the root `/metrics` path.
//! The endpoint is intentionally mounted outside the `/api` router prefix to make
//! Kubernetes `ServiceMonitor` / `PodMonitor` configuration straightforward.

use crate::AppState;
use actix_web::{web, HttpResponse};
use std::sync::Arc;
use tracing::warn;

/// Configure Prometheus metrics routes.
#[inline]
pub fn configure_metrics_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/metrics", web::get().to(metrics_handler));
}

/// Prometheus scrape handler.
///
/// # Notes
/// - For Phase 0, system metrics are updated right before encoding.
/// - All other metrics registered into the global registry will be included automatically.
async fn metrics_handler(state: web::Data<Arc<AppState>>) -> HttpResponse {
    match state.gateway.export_prometheus_metrics() {
        Ok(payload) => HttpResponse::Ok()
            .content_type(payload.content_type)
            .body(payload.body),
        Err(e) => {
            warn!(error=%e, "Failed to gather Prometheus metrics");
            HttpResponse::InternalServerError().body("metrics scrape failed")
        }
    }
}
