//! V1 version API routes
mod action;
mod app;
mod app_sub;
mod auth;
mod branding;
mod channel;
mod device;
mod driver;
mod menu;
mod plugin;
mod point;
mod role;
mod user;
mod ws;

use crate::{
    middleware::{auth::Authentication, casbin::CasbinService},
    validation::EntityValidator,
};
use actix_web::{
    guard::{Guard, GuardContext},
    web,
};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, NGResult};
use std::sync::Arc;
use tracing::instrument;

/// API version guard for v1
pub struct ApiV1Guard;

impl Guard for ApiV1Guard {
    fn check(&self, ctx: &GuardContext<'_>) -> bool {
        // Check both Accept-Api-Version and X-API-Version headers
        ctx.head().headers().get("Accept-Api-Version").map_or_else(
            || {
                ctx.head()
                    .headers()
                    .get("X-API-Version")
                    .is_some_and(|v| v.as_bytes() == b"v1")
            },
            |v| v.as_bytes() == b"v1",
        )
    }
}

/// Configure all v1 routes
pub fn configure_v1_routes(cfg: &mut web::ServiceConfig) {
    cfg.configure(configure_public_routes)
        .configure(configure_protected_routes);
}

/// Configure public routes that don't require authentication
fn configure_public_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        format!("{}/login", auth::ROUTER_PREFIX).as_str(),
        web::post().to(auth::login),
    )
    // WebSocket monitor endpoints: authenticated only, no RBAC.
    .service(web::scope(ws::ROUTER_PREFIX).configure(ws::configure_routes));
}

/// Helper function to configure protected routes with casbin middleware
fn configure_protected_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("")
            .guard(ApiV1Guard)
            .wrap(CasbinService)
            .wrap(Authentication)
            .service(web::scope(auth::ROUTER_PREFIX).configure(auth::configure_routes))
            .service(web::scope(menu::ROUTER_PREFIX).configure(menu::configure_routes))
            .service(web::scope(role::ROUTER_PREFIX).configure(role::configure_routes))
            .service(web::scope(user::ROUTER_PREFIX).configure(user::configure_routes))
            .service(web::scope(branding::ROUTER_PREFIX).configure(branding::configure_routes))
            .service(web::scope(channel::ROUTER_PREFIX).configure(channel::configure_routes))
            .service(web::scope(driver::ROUTER_PREFIX).configure(driver::configure_routes))
            .service(web::scope(device::ROUTER_PREFIX).configure(device::configure_routes))
            .service(web::scope(point::ROUTER_PREFIX).configure(point::configure_routes))
            .service(web::scope(action::ROUTER_PREFIX).configure(action::configure_routes))
            .service(web::scope(plugin::ROUTER_PREFIX).configure(plugin::configure_routes))
            .service(web::scope(app::ROUTER_PREFIX).configure(app::configure_routes))
            .service(web::scope(app_sub::ROUTER_PREFIX).configure(app_sub::configure_routes)),
    );
}

/// Initialize RBAC rules for all v1 API modules
///
/// # Arguments
/// * `router_prefix` - Base URL prefix for all routes
/// * `perm_checker` - Permission checker instance for registering rules
///
/// # Returns
/// * `NGResult<()>` - Success or error result
pub async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    // User module rules
    user::init_rbac_rules(router_prefix, perm_checker).await?;

    // Role module rules
    role::init_rbac_rules(router_prefix, perm_checker).await?;

    // Menu module rules
    menu::init_rbac_rules(router_prefix, perm_checker).await?;

    // Branding module rules
    branding::init_rbac_rules(router_prefix, perm_checker).await?;

    // Channel module rules
    channel::init_rbac_rules(router_prefix, perm_checker).await?;

    // Driver module rules
    driver::init_rbac_rules(router_prefix, perm_checker).await?;

    // Device module rules
    device::init_rbac_rules(router_prefix, perm_checker).await?;

    // Point module rules
    point::init_rbac_rules(router_prefix, perm_checker).await?;

    // Action module rules
    action::init_rbac_rules(router_prefix, perm_checker).await?;

    // Northward plugin module rules
    plugin::init_rbac_rules(router_prefix, perm_checker).await?;

    // Northward app module rules
    app::init_rbac_rules(router_prefix, perm_checker).await?;

    // Northward subscription module rules
    app_sub::init_rbac_rules(router_prefix, perm_checker).await?;

    Ok(())
}

/// Collects additional entity validators from all v1 API modules.
///
/// # Returns
/// * `Vec<Arc<dyn EntityValidator>>` - A vector of validators to be registered.
#[inline]
#[allow(clippy::let_and_return)]
#[instrument(name = "collect-v1-validators", skip_all)]
pub fn collect_additional_validators() -> Vec<Arc<dyn EntityValidator>> {
    let validators = Vec::new();
    // TODO: Add validators
    validators
}
