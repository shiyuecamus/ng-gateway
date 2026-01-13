//! Role management API endpoints

use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_web::{
    http::Method,
    web::{self},
};
use actix_web_validator::{Json, Path, Query};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        ChangeRoleStatus, NewRole, PageResult, PathId, RoleInfo, RolePageParams, UpdateRole,
    },
    enums::common::{EntityType, Operation},
    enums::role::RoleType,
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::role::RoleRepository;
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::sync::Arc;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/role";

/// TODO: 删除角色、修改角色需要清空用户角色缓存、删除对应关联、清空权限相关
/// Configure role routes
///
/// # Description
/// Registers all role management endpoints with the Actix web service
///
/// # Routes
/// - GET `/list`: Retrieve a list of all roles
/// - GET `/page`: Retrieve paginated list of roles
/// - GET `/detail/{id}`: Retrieve role details by ID
/// - POST `/create`: Create a new role
/// - PUT `/update`: Update role information
/// - DELETE `/delete/{id}`: Delete role
/// - POST `/change-status`: Change role status
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/change-status", web::put().to(change_status))
        .route("/{id}", web::delete().to(delete));
}

/// Initialize RBAC rules for role module
///
/// # Description
/// Sets up role-based access control rules for the role management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-role-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing role module RBAC rules...");

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
                .or(has_resource_operation(EntityType::Role, Operation::Read)?)
                .or(has_scope("role:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Role, Operation::Read)?)
                .or(has_scope("role:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Role, Operation::Create)?)
                .or(has_scope("role:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Role, Operation::Write)?)
                .or(has_scope("role:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Role, Operation::Delete)?)
                .or(has_scope("role:delete")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Role, Operation::Write)?)
                .or(has_scope("role:write")?),
        )
        .await?;

    info!("Role module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve a list of all roles
///
/// # Endpoint
/// `GET /api/role/list`
///
/// # Authorization
/// Requires either `system_admin` or `tenant_admin` role
///
/// # Returns
/// - `WebResult<WebResponse<Vec<RoleInfo>>>`: List of all roles on success
///   or appropriate error response
async fn list() -> WebResult<WebResponse<Vec<RoleInfo>>> {
    Ok(WebResponse::ok(RoleRepository::find_all().await?))
}

/// Retrieve paginated list of roles
///
/// # Endpoint
/// `GET /api/role/page`
///
/// # Parameters
/// - Query parameters matching `RolePageParams` structure:
///   - page: page number
///   - pageSize: page size
///   - name: optional name filter
///   - code: optional code filter
///   - status: optional status filter
///
/// # Authorization
/// Requires either:
/// - `system_admin` role, OR
/// - Role read permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<PageResult<RoleInfo>>>`: Paginated role list on success
///   or appropriate error response
async fn page(params: Query<RolePageParams>) -> WebResult<WebResponse<PageResult<RoleInfo>>> {
    Ok(WebResponse::ok(
        RoleRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve role details by ID
///
/// # Endpoint
/// `GET /api/role/detail/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Role read permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<RoleModel>>`: Role details on success
///
/// # Errors
/// - Not Found (404): When role is not found
async fn get_by_id(params: Path<PathId>) -> WebResult<WebResponse<RoleInfo>> {
    Ok(WebResponse::ok(
        RoleRepository::find_role_info(params.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Role.to_string()))?,
    ))
}

/// Create a new role
///
/// # Endpoint
/// `POST /api/role/create`
///
/// # Authorization
/// Requires `system_admin` role
/// Role create permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When role data is invalid
async fn create(
    role: Json<NewRole>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = role.into_inner().into_active_model();
    state.validator.validate(&model, Operation::Create).await?;
    RoleRepository::create::<DatabaseConnection>(model, None).await?;
    Ok(WebResponse::ok(true))
}

/// Update role information
///
/// # Endpoint
/// `PUT /api/role/update`
///
/// # Authorization
/// Requires `system_admin` role
/// Role write permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When role data is invalid
/// - Not Found (404): When role is not found
async fn update(
    role: Json<UpdateRole>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let role = role.into_inner();
    let existing_role = match RoleRepository::find_by_id(role.id).await? {
        Some(role) => role,
        None => {
            return Err(WebError::NotFound(EntityType::Role.to_string()));
        }
    };

    // Check if the role is a built-in role
    if existing_role.r#type == RoleType::BuiltIn {
        return Err(WebError::BadRequest(
            "Cannot modify built-in role".to_string(),
        ));
    }

    let model = role.into_active_model();
    state.validator.validate(&model, Operation::Write).await?;
    RoleRepository::update::<DatabaseConnection>(model, None).await?;
    Ok(WebResponse::ok(true))
}

/// Delete role
///
/// # Endpoint
/// `DELETE /api/role/delete/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Role write permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When role is not found
async fn delete(params: Path<PathId>) -> WebResult<WebResponse<bool>> {
    let existing_role = RoleRepository::find_by_id(params.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Role.to_string()))?;

    // Check if the role is a built-in role
    if existing_role.r#type == RoleType::BuiltIn {
        return Err(WebError::BadRequest(
            "Cannot delete built-in role".to_string(),
        ));
    }

    RoleRepository::delete::<DatabaseConnection>(params.id, None).await?;
    Ok(WebResponse::ok(true))
}

/// Change role status
///
/// # Endpoint
/// `POST /api/role/change-status`
///
/// # Authorization
/// Requires `system_admin` role or `tenant_admin` role
/// Role write permission on the Role resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When role is not found
async fn change_status(req: Json<ChangeRoleStatus>) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    let existing_role = RoleRepository::find_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Role.to_string()))?;

    // Check if the role is a built-in role
    if existing_role.r#type == RoleType::BuiltIn {
        return Err(WebError::BadRequest(
            "Cannot change status of built-in role".to_string(),
        ));
    }

    RoleRepository::update::<DatabaseConnection>(req.into_active_model(), None).await?;
    Ok(WebResponse::ok(true))
}
