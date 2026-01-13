//! Router module for handling all API routes

pub mod public;
pub mod v1;

use actix_web::web;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, NGResult};
use ng_gateway_models::PermChecker;
use std::sync::Arc;
use tracing::{info, instrument};

use crate::validation::EntityValidator;

/// Configure all routes
pub fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.configure(v1::configure_v1_routes);
}

/// Configure public root routes (mounted outside `/api` router prefix).
///
/// # Notes
/// These routes must be registered at the root scope (e.g. `/favicon.ico`) and
/// therefore cannot live under `v1` which is mounted under the API router prefix.
pub fn configure_public_routes(cfg: &mut web::ServiceConfig) {
    cfg.configure(public::configure_public_routes);
}

/// Initialize all RBAC rules for the application
/// This should be called once during application startup
#[inline]
#[instrument(name = "init-rbac-rules", skip(router_prefix, perm_checker))]
pub async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: Arc<dyn PermChecker>,
) -> NGResult<(), RBACError> {
    info!("Initializing all RBAC rules for protected routes...");
    let checker = perm_checker
        .downcast_ref::<NGPermChecker>()
        .ok_or(RBACError::Primitive)?;

    v1::init_rbac_rules(router_prefix, checker).await?;

    info!("All RBAC rules initialized successfully");
    Ok(())
}

/// Collects additional entity validators from all API modules.
///
/// This function is called during server setup to gather all custom validators
/// defined within specific API modules (e.g., driver, user, etc.).
///
/// # Returns
/// * `Vec<Arc<dyn EntityValidator>>` - A vector of validators to be registered.
#[inline]
#[instrument(name = "collect-validators", skip_all)]
pub fn collect_additional_validators() -> Vec<Arc<dyn EntityValidator>> {
    let mut validators = Vec::new();
    validators.extend(v1::collect_additional_validators());
    info!("Collected {} additional validators", validators.len());
    validators
}
