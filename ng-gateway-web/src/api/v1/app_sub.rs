use std::sync::Arc;

use actix_web::{http::Method, web};
use actix_web_validator::{Json, Path, Query};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        AppSubInfo, AppSubPageParams, ChannelDeviceTree, DeviceTreeNode, NewAppSub, PageResult,
        PathId, UpdateAppSub,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{AppSubRepository, ChannelRepository};
use sea_orm::DatabaseConnection;
use tracing::{info, instrument};

use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};

pub(super) const ROUTER_PREFIX: &str = "/northward-sub";

/// Configure northward subscription routes
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/device-tree", web::get().to(device_tree))
        .route("/{id}", web::delete().to(delete));
}

/// Initialize RBAC rules for northward subscription module
#[inline]
#[instrument(name = "init-northward-sub-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing northward subscription module RBAC rules...");

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-sub:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/device-tree"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-sub:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Create)?)
                .or(has_scope("northward-sub:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Write)?)
                .or(has_scope("northward-sub:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Delete)?)
                .or(has_scope("northward-sub:delete")?),
        )
        .await?;

    info!("Northward subscription module RBAC rules initialized successfully");
    Ok(())
}
/// Retrieve all northward subscriptions
async fn list() -> WebResult<WebResponse<Vec<AppSubInfo>>> {
    Ok(WebResponse::ok(AppSubRepository::find_all().await?))
}

/// Retrieve paginated list of northward subscriptions
async fn page(params: Query<AppSubPageParams>) -> WebResult<WebResponse<PageResult<AppSubInfo>>> {
    Ok(WebResponse::ok(
        AppSubRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve channel-device tree for subscription scope selection
async fn device_tree() -> WebResult<WebResponse<Vec<ChannelDeviceTree>>> {
    let start = std::time::Instant::now();
    let tree = ChannelRepository::find_with_devices::<DatabaseConnection>(None)
        .await?
        .into_iter()
        .map(|(channel, devices)| ChannelDeviceTree {
            id: channel.id,
            name: channel.name,
            devices: devices
                .into_iter()
                .map(|device| DeviceTreeNode {
                    id: device.id,
                    name: device.device_name,
                })
                .collect(),
        })
        .collect();
    let elapsed = start.elapsed();
    tracing::info!(?elapsed, "device_tree built");
    Ok(WebResponse::ok(tree))
}

/// Create northward subscriptions (batch)
pub async fn create(
    sub: Json<NewAppSub>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.create_sub(sub.into_inner()).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Update northward subscriptions (batch)
pub async fn update(
    sub: Json<UpdateAppSub>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.update_sub(sub.into_inner()).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Delete northward subscription
pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_sub(params.id).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}
