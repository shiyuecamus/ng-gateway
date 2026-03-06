//! WebSocket endpoints for v1 APIs.
//!
//! This module groups all WebSocket-based endpoints under a dedicated `/ws`
//! scope to keep them separate from standard REST APIs while still sharing
//! the same authentication and versioning scheme.

mod ai_live;
mod common;
mod metrics;
mod monitor;

use actix_web::web;

pub(super) const ROUTER_PREFIX: &str = "/ws";

/// Configure all WebSocket routes under `/api/ws`.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/monitor", web::get().to(monitor::monitor_ws))
        .route("/metrics", web::get().to(metrics::metrics_ws))
        .route(
            "/ai/channels/{channel_id}/live",
            web::get().to(ai_live::ai_live_ws),
        );
}
