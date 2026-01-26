//! WebSocket endpoints for v1 APIs.
//!
//! This module groups all WebSocket-based endpoints under a dedicated `/ws`
//! scope to keep them separate from standard REST APIs while still sharing
//! the same authentication and versioning scheme.

mod common;
mod logs;
mod metrics;
mod monitor;

use actix_web::web;

pub(super) const ROUTER_PREFIX: &str = "/ws";

/// Configure all WebSocket routes under `/api/ws`.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/monitor", web::get().to(monitor::monitor_ws));
    cfg.route("/metrics", web::get().to(metrics::metrics_ws));
    cfg.route("/logs", web::get().to(logs::logs_ws));
}
