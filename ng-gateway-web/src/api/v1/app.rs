use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_web::{http::Method, web};
use actix_web_validator::{Json, Path, Query};
use ng_gateway_common::{
    casbin::NGPermChecker,
    log::control::{self as log_control, LogOverrideScope},
};
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        AppInfo, AppLogLevelView, AppLogOverrideView, AppPageParams, ChangeAppStatus, NewApp,
        PageResult, PathId, SetAppLogLevelRequest, TtlRange, UpdateApp,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::AppRepository;
use sea_orm::IntoActiveModel;
use std::sync::Arc;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/northward-app";

/// Configure northward app routes
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/change-status", web::put().to(change_status))
        .route("/{id}", web::delete().to(delete))
        .route("/{id}/log-level", web::get().to(get_app_log_level))
        .route("/{id}/log-level", web::put().to(set_app_log_level))
        .route("/{id}/log-level", web::delete().to(clear_app_log_level));
}

/// Initialize RBAC rules for northward app module
#[inline]
#[instrument(name = "init-northward-app-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing northward app module RBAC rules...");

    let rules = vec![
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-app:read")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-app:read")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-app:read")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Create)?)
                .or(has_scope("northward-app:create")?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Write)?)
                .or(has_scope("northward-app:write")?),
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Delete)?)
                .or(has_scope("northward-app:delete")?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Write)?)
                .or(has_scope("northward-app:write")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Read)?)
                .or(has_scope("northward-app:read")?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Write)?)
                .or(has_scope("northward-app:write")?),
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::App, Operation::Write)?)
                .or(has_scope("northward-app:write")?),
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("Northward app module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve a list of all northward apps
pub async fn list(state: web::Data<Arc<AppState>>) -> WebResult<WebResponse<Vec<AppInfo>>> {
    let mut apps = AppRepository::find_all().await?;

    // Enrich with connection states from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let northward_manager = state.gateway.northward_manager();
    for app in apps.iter_mut() {
        app.connection_state = northward_manager
            .get_app_connection_state(app.id)
            .map(|s| s.as_ref().clone());
    }

    Ok(WebResponse::ok(apps))
}

/// Retrieve paginated list of northward apps
pub async fn page(
    params: Query<AppPageParams>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<AppInfo>>> {
    let mut result = AppRepository::page(params.into_inner()).await?;

    // Enrich with connection states from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let northward_manager = state.gateway.northward_manager();
    for app in result.records.iter_mut() {
        app.connection_state = northward_manager
            .get_app_connection_state(app.id)
            .map(|s| s.as_ref().clone());
    }

    Ok(WebResponse::ok(result))
}

/// Retrieve northward app details by ID
pub async fn get_by_id(
    req: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AppInfo>> {
    let mut app = AppRepository::find_info_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    // Enrich with connection state from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let northward_manager = state.gateway.northward_manager();
    app.connection_state = northward_manager
        .get_app_connection_state(app.id)
        .map(|s| s.as_ref().clone());

    Ok(WebResponse::ok(app))
}

/// Create a new northward app
pub async fn create(
    app: Json<NewApp>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = app.into_inner();
    state
        .validator
        .validate(&model.clone().into_active_model(), Operation::Create)
        .await?;

    match state.gateway.create_app(model).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Update northward app information
pub async fn update(
    app: Json<UpdateApp>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let payload = app.into_inner();
    state
        .validator
        .validate(&payload.clone().into_active_model(), Operation::Write)
        .await?;

    match state.gateway.update_app(payload).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Delete northward app
pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_app(params.id).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Change northward app status
pub async fn change_status(
    payload: Json<ChangeAppStatus>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let change = payload.into_inner();
    let app = AppRepository::find_by_id(change.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    match state.gateway.change_app_status(app, change.status).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

#[inline]
async fn build_app_log_level_view(id: i32) -> Result<AppLogLevelView, WebError> {
    // Validate app exists.
    AppRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let overrides = rt.overrides();
    let effective = overrides.effective_app_level(id);
    let lease = overrides.active_scope_lease(LogOverrideScope::App(id));
    let s = rt.settings();

    Ok(AppLogLevelView {
        app_id: id,
        effective,
        r#override: lease.map(|l| AppLogOverrideView {
            level: l.level,
            ttl_ms: l.ttl_ms,
            expires_at_ms: l.expires_at_ms,
        }),
        ttl: TtlRange {
            min_ms: s.override_min_ttl_ms,
            max_ms: s.override_max_ttl_ms,
            default_ms: s.override_default_ttl_ms,
        },
    })
}

#[instrument(name = "get-app-log-level", skip_all)]
pub async fn get_app_log_level(params: Path<PathId>) -> WebResult<WebResponse<AppLogLevelView>> {
    let id = params.id;
    Ok(WebResponse::ok(build_app_log_level_view(id).await?))
}

#[instrument(name = "set-app-log-level", skip_all)]
pub async fn set_app_log_level(
    params: Path<PathId>,
    req: web::Json<SetAppLogLevelRequest>,
) -> WebResult<WebResponse<AppLogLevelView>> {
    let id = params.id;
    let req = req.into_inner();

    // Validate app exists (avoid leaking overrides for invalid ids).
    AppRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let s = rt.settings();
    let ttl = req.ttl_ms.unwrap_or(s.override_default_ttl_ms);
    s.validate_override_ttl_ms(ttl)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;

    rt.overrides()
        .set_temporary_override(LogOverrideScope::App(id), req.level, ttl);

    Ok(WebResponse::ok(build_app_log_level_view(id).await?))
}

#[instrument(name = "clear-app-log-level", skip_all)]
pub async fn clear_app_log_level(params: Path<PathId>) -> WebResult<WebResponse<AppLogLevelView>> {
    let id = params.id;

    AppRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::App.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    rt.overrides().clear_scope(LogOverrideScope::App(id));

    Ok(WebResponse::ok(build_app_log_level_view(id).await?))
}
