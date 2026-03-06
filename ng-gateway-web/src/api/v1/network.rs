//! Network configuration REST API handlers.
//!
//! Phase 1: interface enumeration and capabilities detection.
//! Phase 2: interface IP configuration and DNS.
//! Phase 3: Wi-Fi scanning and connection.
//! Phase 4: AP hotspot management.

use crate::{middleware::RequestContext, rbac::has_any_role};
use actix_web::{http::Method, web};
use actix_web_validator::Json;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_core::NetworkService;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE, domain::prelude::*, rbac::PermRule, web::WebResponse,
    PermChecker,
};
use std::sync::Arc;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/network";

/// Configure network routes.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg
        // Phase 1: Discovery
        .route("/interfaces", web::get().to(list_interfaces))
        .route("/interfaces/{name}", web::get().to(get_interface))
        .route("/capabilities", web::get().to(get_capabilities))
        // Aggregated status (best-interface selection by backend)
        .route("/wired/status", web::get().to(wired_status))
        // Phase 2: Interface configuration & DNS
        .route("/interfaces/{name}", web::put().to(configure_interface))
        .route("/dns", web::get().to(get_dns))
        .route("/dns", web::put().to(configure_dns))
        // Phase 3: Wi-Fi
        .route("/wifi/scan", web::get().to(scan_wifi))
        .route("/wifi/connect", web::post().to(connect_wifi))
        .route("/wifi/disconnect", web::post().to(disconnect_wifi))
        .route("/wifi/status", web::get().to(wifi_status))
        // Phase 4: AP Hotspot
        .route("/ap", web::get().to(get_ap_status))
        .route("/ap", web::put().to(configure_ap));
}

/// Initialize RBAC rules for network module (admin only).
#[inline]
#[instrument(name = "init-network-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing network module RBAC rules...");

    let rules: [(Method, String, Box<dyn PermRule>); 13] = [
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/interfaces"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/interfaces/*"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/interfaces/*"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/capabilities"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/wired/status"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/dns"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/dns"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/wifi/scan"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/wifi/connect"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/wifi/disconnect"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/wifi/status"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/ap"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/ap"),
            Box::new(has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?),
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("Network module RBAC rules initialized successfully");
    Ok(())
}

// ─── Phase 1: Discovery ───

/// `GET /network/interfaces` — List all network interfaces.
#[instrument(name = "network-list-interfaces", skip_all)]
async fn list_interfaces(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
) -> WebResult<WebResponse<Vec<NetworkInterfaceSummary>>> {
    let interfaces = network
        .list_interfaces()
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to list interfaces: {e}")))?;
    Ok(WebResponse::ok(interfaces))
}

#[derive(Debug, serde::Deserialize)]
pub struct InterfaceNamePath {
    name: String,
}

/// `GET /network/interfaces/{name}` — Get detailed interface info.
#[instrument(name = "network-get-interface", skip(_ctx, network), fields(name = %path.name))]
async fn get_interface(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    path: web::Path<InterfaceNamePath>,
) -> WebResult<WebResponse<NetworkInterfaceDetail>> {
    let detail = network
        .get_interface(&path.name)
        .await
        .map_err(|e| WebError::NotFound(format!("Interface '{}': {e}", path.name)))?;
    Ok(WebResponse::ok(detail))
}

/// `GET /network/capabilities` — Detect platform capabilities.
#[instrument(name = "network-capabilities", skip_all)]
async fn get_capabilities(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
) -> WebResult<WebResponse<NetworkCapabilities>> {
    let caps = network
        .capabilities()
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to detect capabilities: {e}")))?;
    Ok(WebResponse::ok(caps))
}

/// `GET /network/wired/status` — Best wired interface with enriched status.
#[instrument(name = "network-wired-status", skip_all)]
async fn wired_status(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
) -> WebResult<WebResponse<WiredStatus>> {
    let status = network
        .wired_status()
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to get wired status: {e}")))?;
    Ok(WebResponse::ok(status))
}

// ─── Phase 2: Interface Configuration & DNS ───

/// `PUT /network/interfaces/{name}` — Configure interface IP settings.
#[instrument(name = "network-configure-interface", skip(_ctx, network, payload), fields(name = %path.name))]
async fn configure_interface(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    path: web::Path<InterfaceNamePath>,
    payload: Json<ConfigureInterfaceRequest>,
) -> WebResult<WebResponse<bool>> {
    network
        .configure_interface(&path.name, &payload.into_inner())
        .await
        .map_err(|e| {
            WebError::InternalError(format!(
                "Failed to configure interface '{}': {e}",
                path.name
            ))
        })?;
    Ok(WebResponse::ok(true))
}

/// `GET /network/dns` — Get current DNS configuration.
#[instrument(name = "network-get-dns", skip_all)]
async fn get_dns(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
) -> WebResult<WebResponse<DnsConfig>> {
    let dns = network
        .get_dns()
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to get DNS config: {e}")))?;
    Ok(WebResponse::ok(dns))
}

/// `PUT /network/dns` — Set DNS configuration.
#[instrument(name = "network-configure-dns", skip(_ctx, network, payload))]
async fn configure_dns(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    payload: Json<ConfigureDnsRequest>,
) -> WebResult<WebResponse<bool>> {
    network
        .configure_dns(&payload.into_inner())
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to configure DNS: {e}")))?;
    Ok(WebResponse::ok(true))
}

// ─── Phase 3: Wi-Fi ───

/// Optional query parameter for Wi-Fi interface selection.
#[derive(Debug, serde::Deserialize)]
pub struct WifiInterfaceQuery {
    interface: Option<String>,
}

/// `GET /network/wifi/scan` — Scan for Wi-Fi access points.
#[instrument(name = "network-wifi-scan", skip(_ctx, network))]
async fn scan_wifi(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    query: web::Query<WifiInterfaceQuery>,
) -> WebResult<WebResponse<Vec<WifiAccessPoint>>> {
    let aps = network
        .scan_wifi(query.interface.as_deref())
        .await
        .map_err(|e| WebError::InternalError(format!("Wi-Fi scan failed: {e}")))?;
    Ok(WebResponse::ok(aps))
}

/// `POST /network/wifi/connect` — Connect to a Wi-Fi network.
#[instrument(name = "network-wifi-connect", skip(_ctx, network, payload))]
async fn connect_wifi(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    payload: Json<WifiConnectRequest>,
) -> WebResult<WebResponse<WifiStaStatus>> {
    let status = network
        .connect_wifi(&payload.into_inner())
        .await
        .map_err(|e| WebError::InternalError(format!("Wi-Fi connect failed: {e}")))?;
    Ok(WebResponse::ok(status))
}

/// `POST /network/wifi/disconnect` — Disconnect Wi-Fi STA.
#[instrument(name = "network-wifi-disconnect", skip(_ctx, network))]
async fn disconnect_wifi(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    query: web::Query<WifiInterfaceQuery>,
) -> WebResult<WebResponse<bool>> {
    network
        .disconnect_wifi(query.interface.as_deref())
        .await
        .map_err(|e| WebError::InternalError(format!("Wi-Fi disconnect failed: {e}")))?;
    Ok(WebResponse::ok(true))
}

/// `GET /network/wifi/status` — Get Wi-Fi STA connection status.
#[instrument(name = "network-wifi-status", skip(_ctx, network))]
async fn wifi_status(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    query: web::Query<WifiInterfaceQuery>,
) -> WebResult<WebResponse<WifiStaStatus>> {
    let status = network
        .wifi_sta_status(query.interface.as_deref())
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to get Wi-Fi status: {e}")))?;
    Ok(WebResponse::ok(status))
}

// ─── Phase 4: AP Hotspot ───

/// `GET /network/ap` — Get AP hotspot status.
#[instrument(name = "network-ap-status", skip_all)]
async fn get_ap_status(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
) -> WebResult<WebResponse<ApStatus>> {
    let status = network
        .ap_status()
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to get AP status: {e}")))?;
    Ok(WebResponse::ok(status))
}

/// `PUT /network/ap` — Configure AP hotspot.
#[instrument(name = "network-configure-ap", skip(_ctx, network, payload))]
async fn configure_ap(
    _ctx: RequestContext,
    network: web::Data<Arc<NetworkService>>,
    payload: Json<ConfigureApRequest>,
) -> WebResult<WebResponse<ApStatus>> {
    let status = network
        .configure_ap(&payload.into_inner())
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to configure AP: {e}")))?;
    Ok(WebResponse::ok(status))
}
