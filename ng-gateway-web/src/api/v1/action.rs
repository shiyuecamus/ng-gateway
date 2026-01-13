use std::{sync::Arc, time::Instant};

use actix_web::{http::Method, web};
use actix_web_validator::{Json, Path, Query};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        ActionInfo, ActionPageParams, BatchDeletePayload, ClearByDevicePayload, NewAction,
        PageResult, PathId, UpdateAction,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::ActionRepository;
use sea_orm::IntoActiveModel;
use tracing::{info, instrument};

use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};

pub(super) const ROUTER_PREFIX: &str = "/action";

/// Configure action routes
///
/// # Description
/// Registers all action management endpoints with the Actix web service
///
/// # Routes
/// - GET `/list`: Retrieve a list of all actions
/// - GET `/page`: Retrieve paginated list of actions
/// - GET `/detail/{id}`: Retrieve action details by ID
/// - POST `/create`: Create a new action
/// - PUT `/update`: Update action information
/// - DELETE `/delete/{id}`: Delete action
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/by-device/{id}", web::get().to(list_by_device))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/batch-delete", web::post().to(batch_delete))
        .route("/clear", web::post().to(clear))
        .route("/{id}/debug", web::post().to(debug))
        .route("/{id}", web::delete().to(delete));
}

/// Initialize RBAC rules for action module
///
/// # Description
/// Sets up role-based access control rules for the action management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-action-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    // Detail
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Action, Operation::Read)?)
                .or(has_scope("action:read")?),
        )
        .await?;

    // List
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    // Page
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Action, Operation::Read)?)
                .or(has_scope("action:read")?),
        )
        .await?;

    // List by device
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/by-device/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Action, Operation::Read)?)
                .or(has_scope("action:read")?),
        )
        .await?;

    // Create
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Action,
                    Operation::Create,
                )?)
                .or(has_scope("action:create")?),
        )
        .await?;

    // Update
    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Action,
                    Operation::Write,
                )?)
                .or(has_scope("action:write")?),
        )
        .await?;

    // Delete
    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Action,
                    Operation::Delete,
                )?)
                .or(has_scope("action:delete")?),
        )
        .await?;

    // Batch Delete
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/batch-delete"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Action,
                    Operation::Delete,
                )?)
                .or(has_scope("action:delete")?),
        )
        .await?;

    // Clear by device
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/clear"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Action,
                    Operation::Delete,
                )?)
                .or(has_scope("action:delete")?),
        )
        .await?;

    // Debug
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/debug"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_resource_operation(
                EntityType::Action,
                Operation::DoAction,
            )?),
        )
        .await?;

    info!("Action module RBAC rules initialized successfully");
    Ok(())
}

pub async fn get_by_id(req: Path<PathId>) -> WebResult<WebResponse<ActionInfo>> {
    Ok(WebResponse::ok(
        ActionRepository::find_info_by_id(req.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Action.to_string()))?,
    ))
}

/// Retrieve all actions
///
/// Endpoint
/// `GET /api/action/list`
///
/// Authorization
/// Requires `system_admin` role
///
/// Returns
/// - `WebResult<WebResponse<Vec<ActionInfo>>>`
async fn list() -> WebResult<WebResponse<Vec<ActionInfo>>> {
    Ok(WebResponse::ok(ActionRepository::find_all_info().await?))
}

/// Retrieve paginated list of actions
///
/// Endpoint
/// `GET /api/action/page`
///
/// Parameters
/// - Query parameters matching `ActionPageParams`
///
/// Authorization
/// Requires either `system_admin` role or action read permission
///
/// Returns
/// - `WebResult<WebResponse<PageResult<ActionInfo>>>`
async fn page(params: Query<ActionPageParams>) -> WebResult<WebResponse<PageResult<ActionInfo>>> {
    Ok(WebResponse::ok(
        ActionRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve all actions for a specific device
///
/// Endpoint
/// `GET /api/action/by-device/{id}`
///
/// Authorization
/// Requires either `system_admin` role or action read permission
///
/// Returns
/// - `WebResult<WebResponse<Vec<ActionInfo>>>`
pub async fn list_by_device(path: Path<PathId>) -> WebResult<WebResponse<Vec<ActionInfo>>> {
    Ok(WebResponse::ok(
        ActionRepository::find_info_by_device_id(path.id).await?,
    ))
}

/// Create a new action
///
/// # Endpoint
/// `POST /api/action/create`
///
/// # Authorization
/// Requires `system_admin` role
/// Action create permission on the Action resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When action data is invalid
pub async fn create(
    action: Json<NewAction>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = action.into_inner();
    state
        .validator
        .validate(&model.clone().into_active_model(), Operation::Create)
        .await?;

    match state.gateway.create_actions(vec![model]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Update an existing action
///
/// # Endpoint
/// `PUT /api/action/update`
///
/// # Authorization
/// Requires `system_admin` role
/// Action write permission on the Action resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When action data is invalid
/// - Not Found (404): When action is not found
/// - Internal Server Error (500): When action update fails
pub async fn update(
    action: Json<UpdateAction>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let payload = action.into_inner();
    state
        .validator
        .validate(&payload.clone().into_active_model(), Operation::Write)
        .await?;

    match state.gateway.update_actions(vec![payload]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Delete an existing action
///
/// # Endpoint
/// `DELETE /api/action/delete/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Action delete permission on the Action resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When action is not found
/// - Internal Server Error (500): When action delete fails
pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_actions(vec![params.id]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DebugPayload {
    params: serde_json::Value,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DebugResponse {
    result: serde_json::Value,
    elapsed_ms: u128,
}

#[instrument(name = "action-debug", skip(state, body))]
pub async fn debug(
    path: Path<PathId>,
    body: actix_web::web::Json<DebugPayload>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<DebugResponse>> {
    let action_id = path.id;
    let timeout_ms = body.timeout_ms.unwrap_or(5000);
    let params = body.params.clone();

    let start = Instant::now();
    match state
        .gateway
        .debug_action(action_id, params, Some(timeout_ms))
        .await
    {
        Ok(result) => {
            let elapsed_ms = start.elapsed().as_millis();
            Ok(WebResponse::ok(DebugResponse { result, elapsed_ms }))
        }
        Err(e) => Err(WebError::from(e)),
    }
}

/// Batch delete actions by IDs
///
/// # Endpoint
/// `POST /api/action/batch-delete`
///
/// # Authorization
/// Requires `system_admin` role or action delete permission
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
pub async fn batch_delete(
    payload: Json<BatchDeletePayload>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let ids = payload.into_inner().ids;

    if ids.is_empty() {
        // Nothing to delete; treat as success
        return Ok(WebResponse::ok(true));
    }

    match state.gateway.delete_actions(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Clear all actions for a given device
///
/// # Endpoint
/// `POST /api/action/clear`
///
/// # Authorization
/// Requires `system_admin` role or action delete permission
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
pub async fn clear(
    payload: Json<ClearByDevicePayload>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let device_id = payload.device_id;

    // Fetch all actions for the device and delete them via gateway to keep runtime in sync
    let actions = ActionRepository::find_by_device_id(device_id).await?;
    let ids: Vec<i32> = actions.into_iter().map(|a| a.id).collect();

    if ids.is_empty() {
        return Ok(WebResponse::ok(true));
    }

    match state.gateway.delete_actions(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}
