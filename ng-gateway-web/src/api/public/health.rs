//! Health check endpoints.
//!
//! # Why root health
//! A root `/health` endpoint is convenient for load balancers and Kubernetes probes.
//! It should not depend on API router prefix or API version headers.

use actix_web::{web, HttpResponse};

/// Configure health check routes.
pub fn configure_health_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/health", web::get().to(health));
}

/// Simple health check handler.
async fn health() -> HttpResponse {
    HttpResponse::Ok().body("OK")
}
