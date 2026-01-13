use std::sync::Arc;

use actix_web::{http::Method, web};
use actix_web_validator::{Json, Path, Query};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        BatchDeletePayload, ClearByDevicePayload, NewPoint, PageResult, PathId, PointInfo,
        PointPageParams, UpdatePoint,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::PointRepository;
use sea_orm::IntoActiveModel;
use tracing::{info, instrument};

use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};

pub(super) const ROUTER_PREFIX: &str = "/point";

/// Configure point routes
///
/// # Description
/// Registers all point management endpoints with the Actix web service
///
/// # Routes
/// - GET `/list`: Retrieve a list of all points
/// - GET `/page`: Retrieve paginated list of points
/// - GET `/detail/{id}`: Retrieve point details by ID
/// - POST `/create`: Create a new point
/// - PUT `/update`: Update point information
/// - DELETE `/delete/{id}`: Delete point
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/batch-delete", web::post().to(batch_delete))
        .route("/clear", web::post().to(clear))
        .route("/{id}", web::delete().to(delete));
}

/// Initialize RBAC rules for point module
///
/// # Description
/// Sets up role-based access control rules for the point management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-point-rbac", skip(router_prefix, perm_checker))]
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
                .or(has_resource_operation(EntityType::Point, Operation::Read)?)
                .or(has_scope("point:read")?),
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
                .or(has_resource_operation(EntityType::Point, Operation::Read)?)
                .or(has_scope("point:read")?),
        )
        .await?;

    // Create
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Point,
                    Operation::Create,
                )?)
                .or(has_scope("point:create")?),
        )
        .await?;

    // Update
    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Point, Operation::Write)?)
                .or(has_scope("point:write")?),
        )
        .await?;

    // Delete
    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Point,
                    Operation::Delete,
                )?)
                .or(has_scope("point:delete")?),
        )
        .await?;

    // Batch Delete
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/batch-delete"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Point,
                    Operation::Delete,
                )?)
                .or(has_scope("point:delete")?),
        )
        .await?;

    // Clear by device
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/clear"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Point,
                    Operation::Delete,
                )?)
                .or(has_scope("point:delete")?),
        )
        .await?;

    info!("Point module RBAC rules initialized successfully");
    Ok(())
}

pub async fn get_by_id(req: Path<PathId>) -> WebResult<WebResponse<PointInfo>> {
    Ok(WebResponse::ok(
        PointRepository::find_info_by_id(req.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Point.to_string()))?,
    ))
}

/// Retrieve all points
///
/// Endpoint
/// `GET /api/point/list`
///
/// Authorization
/// Requires `system_admin` role
///
/// Returns
/// - `WebResult<WebResponse<Vec<PointInfo>>>`: List of all points
async fn list() -> WebResult<WebResponse<Vec<PointInfo>>> {
    Ok(WebResponse::ok(PointRepository::find_all_info().await?))
}

/// Retrieve paginated list of points
///
/// Endpoint
/// `GET /api/point/page`
///
/// Parameters
/// - Query parameters matching `PointPageParams`
///
/// Authorization
/// Requires either `system_admin` role or point read permission
///
/// Returns
/// - `WebResult<WebResponse<PageResult<PointInfo>>>`: Paginated list of points
async fn page(params: Query<PointPageParams>) -> WebResult<WebResponse<PageResult<PointInfo>>> {
    Ok(WebResponse::ok(
        PointRepository::page(params.into_inner()).await?,
    ))
}

/// Create a new point
///
/// # Endpoint
/// `POST /api/point/create`
///
/// # Authorization
/// Requires `system_admin` role
/// Point create permission on the Point resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When point data is invalid
/// - Internal Server Error (500): When point creation fails
pub async fn create(
    point: Json<NewPoint>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = point.into_inner();
    // Validate using ActiveModel shape
    state
        .validator
        .validate(&model.clone().into_active_model(), Operation::Create)
        .await?;

    match state.gateway.create_points(vec![model]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Update an existing point
///
/// # Endpoint
/// `PUT /api/point/update`
///
/// # Authorization
/// Requires `system_admin` role
/// Point write permission on the Point resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When point data is invalid
/// - Not Found (404): When point is not found
/// - Internal Server Error (500): When point update fails
pub async fn update(
    point: Json<UpdatePoint>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let payload = point.into_inner();
    state
        .validator
        .validate(&payload.clone().into_active_model(), Operation::Write)
        .await?;

    match state.gateway.update_points(vec![payload]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Delete an existing point
///
/// # Endpoint
/// `DELETE /api/point/delete/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Point delete permission on the Point resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When point is not found
/// - Internal Server Error (500): When point deletion fails
pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_points(vec![params.id]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Batch delete points by IDs
///
/// # Endpoint
/// `POST /api/point/batch-delete`
///
/// # Authorization
/// Requires `system_admin` role or point delete permission
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

    match state.gateway.delete_points(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Clear all points for a given device
///
/// # Endpoint
/// `POST /api/point/clear`
///
/// # Authorization
/// Requires `system_admin` role or point delete permission
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
pub async fn clear(
    payload: Json<ClearByDevicePayload>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let device_id = payload.device_id;

    // Fetch all points for the device and delete them via gateway to keep runtime in sync
    let points = PointRepository::find_by_device_id(device_id).await?;
    let ids: Vec<i32> = points.into_iter().map(|p| p.id).collect();

    if ids.is_empty() {
        return Ok(WebResponse::ok(true));
    }

    match state.gateway.delete_points(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}
