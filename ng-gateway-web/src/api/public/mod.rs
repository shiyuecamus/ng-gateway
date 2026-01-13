//! Public (non-API-prefix) routes.
//!
//! These routes are mounted at the **root** (e.g. `/favicon.ico`) and therefore
//! must be registered outside the `/api` router prefix scope.
//!
//! # Why this module exists
//! The v1 API routes are mounted under `/api` (or configured router prefix), so
//! they cannot serve browser-required root paths such as `/favicon.ico`.

mod branding;
mod health;

use actix_web::web;

/// Configure all public root routes.
#[inline]
pub fn configure_public_routes(cfg: &mut web::ServiceConfig) {
    cfg.configure(health::configure_health_routes)
        .configure(branding::configure_public_branding_routes);
}
