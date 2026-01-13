//! User management API endpoints

use crate::{
    middleware::RequestContext,
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
        ChangeUserPassword, ChangeUserPasswordWithId, ChangeUserStatus, NewUser, PageResult,
        PathId, ResetUserPassword, UpdateUser, UserInfo, UserInfoWithRoles, UserPageParams,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::user::UserRepository;
use ng_gateway_utils::hash;
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::sync::Arc;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/user";

/// Configure user routes
///
/// # Description
/// Registers all user management endpoints with the Actix web service
///
/// # Routes
/// - GET `/list`: Retrieve a list of all users
/// - GET `/page`: Retrieve paginated list of users
/// - GET `/detail/{id}`: Retrieve user details by ID
/// - POST `/create`: Create a new user
/// - PUT `/update`: Update user information
/// - DELETE `/delete/{id}`: Delete user
/// - GET `/userinfo`: Retrieve user information
/// - POST `/change-password`: Change user password
/// - POST `/change-status`: Change user status
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/userinfo", web::get().to(user_info))
        .route("/change-password", web::put().to(change_password))
        .route("/reset-password", web::put().to(reset_password))
        .route("/change-status", web::put().to(change_status))
        .route("/{id}", web::delete().to(delete));
}

/// Initialize RBAC rules for user module
///
/// # Description
/// Sets up role-based access control rules for the user management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-user-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing user module RBAC rules...");

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
                .or(has_resource_operation(EntityType::User, Operation::Read)?)
                .or(has_scope("user:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::User, Operation::Read)?)
                .or(has_scope("user:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::User, Operation::Create)?)
                .or(has_scope("user:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::User, Operation::Write)?)
                .or(has_scope("user:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::User, Operation::Delete)?)
                .or(has_scope("user:delete")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/reset-password"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::User,
                    Operation::ResetPassword,
                )?)
                .or(has_scope("user:reset-password")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::User, Operation::Write)?)
                .or(has_scope("user:write")?),
        )
        .await?;

    info!("User module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve user information
///
/// # Endpoint
/// `GET /api/user/userinfo`
///
/// # Authorization
/// Requires authenticated user
///
/// # Returns
/// - `WebResult<WebResponse<UserInfo>>`: User information on success
///
/// # Errors
/// - Bad Request (400): When user ID is missing
/// - Not Found (404): When user is not found
async fn user_info(ctx: RequestContext) -> WebResult<WebResponse<UserInfoWithRoles>> {
    let user_id = ctx.grant.unwrap().user_id.parse::<i32>().unwrap();
    let userinfo = UserRepository::find_user_info(user_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::User.to_string()))?;
    let mut user_with_roles: UserInfoWithRoles = userinfo.into();
    user_with_roles.roles = ctx.roles.map(|roles| roles.roles);
    Ok(WebResponse::ok(user_with_roles))
}

/// Retrieve a list of all users
///
/// # Endpoint
/// `GET /api/user/list`
///
/// # Authorization
/// Requires either `system_admin` role
///
/// # Returns
/// - `WebResult<WebResponse<Vec<UserInfo>>>`: List of all users on success
///   or appropriate error response
async fn list() -> WebResult<WebResponse<Vec<UserInfo>>> {
    Ok(WebResponse::ok(
        UserRepository::find_all().await.map_err(WebError::from)?,
    ))
}

/// Retrieve paginated list of users
///
/// # Endpoint
/// `GET /api/user/page`
///
/// # Parameters
/// - Query parameters matching `UserPageParams` structure:
///   - page: page number
///   - pageSize: page size
///   - username: optional username filter
///   - email: optional email filter
///   - status: optional status filter
///
/// # Authorization
/// Requires either:
/// - `system_admin` role, OR
/// - User read permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<PageResult<UserInfo>>>`: Paginated user list on success
///   or appropriate error response
async fn page(params: Query<UserPageParams>) -> WebResult<WebResponse<PageResult<UserInfo>>> {
    Ok(WebResponse::ok(
        UserRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve user details by ID
///
/// # Endpoint
/// `GET /api/user/detail/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// User read permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<UserModel>>`: User details on success
///
/// # Errors
/// - Not Found (404): When user is not found
async fn get_by_id(params: Path<PathId>) -> WebResult<WebResponse<UserInfo>> {
    Ok(WebResponse::ok(
        UserRepository::find_user_info(params.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::User.to_string()))?,
    ))
}

/// Create a new user
///
/// # Endpoint
/// `POST /api/user/create`
///
/// # Authorization
/// Requires `system_admin` role
/// User create permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When user data is invalid
async fn create(
    user: Json<NewUser>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let mut user = user.into_inner();
    user.password = hash::bcrypt_hash(&user.password);
    let model = user.into_active_model();
    state.validator.validate(&model, Operation::Create).await?;
    UserRepository::create::<DatabaseConnection>(model, None).await?;
    Ok(WebResponse::ok(true))
}

/// Update user information
///
/// # Endpoint
/// `PUT /api/user/update`
///
/// # Authorization
/// Requires `system_admin` role
/// User write permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When user data is invalid
/// - Not Found (404): When user is not found
async fn update(
    user: Json<UpdateUser>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let user = user.into_inner();
    if !UserRepository::exists_by_id(user.id).await? {
        return Err(WebError::NotFound(EntityType::User.to_string()));
    }
    let model = user.into_active_model();
    state.validator.validate(&model, Operation::Write).await?;
    UserRepository::update::<DatabaseConnection>(model, None).await?;
    Ok(WebResponse::ok(true))
}

/// Delete user
///
/// # Endpoint
/// `DELETE /api/user/delete/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// User write permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When user is not found
async fn delete(params: Path<PathId>) -> WebResult<WebResponse<bool>> {
    if !UserRepository::exists_by_id(params.id).await? {
        return Err(WebError::NotFound(EntityType::User.to_string()));
    }
    UserRepository::delete::<DatabaseConnection>(params.id, None).await?;
    Ok(WebResponse::ok(true))
}

/// Change user password
///
/// # Endpoint
/// `POST /api/user/change-password`
///
/// # Authorization
/// Requires `system_admin` role
/// User write permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When user data is invalid
/// - Not Found (404): When user is not found
async fn change_password(req: Json<ChangeUserPassword>) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    let user = match UserRepository::find_by_id(req.id).await? {
        Some(user) => user,
        None => {
            return Err(WebError::NotFound(EntityType::User.to_string()));
        }
    };
    if !hash::bcrypt_check(req.old_password.as_deref().unwrap(), &user.password) {
        return Err(WebError::BadRequest(
            "Old password is incorrect".to_string(),
        ));
    }
    UserRepository::update::<DatabaseConnection>(
        ChangeUserPasswordWithId {
            id: req.id,
            password: hash::bcrypt_hash(req.new_password.as_deref().unwrap()),
        }
        .into_active_model(),
        None,
    )
    .await?;
    Ok(WebResponse::ok(true))
}

/// Reset user password
///
/// # Endpoint
/// `POST /api/user/reset-password`
///
/// # Authorization
/// Requires `system_admin` role
/// User write permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When user is not found
async fn reset_password(req: Json<ResetUserPassword>) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    if !UserRepository::exists_by_id(req.id).await? {
        return Err(WebError::NotFound(EntityType::User.to_string()));
    }
    UserRepository::update::<DatabaseConnection>(
        ChangeUserPasswordWithId {
            id: req.id,
            password: hash::bcrypt_hash(req.new_password.as_deref().unwrap()),
        }
        .into_active_model(),
        None,
    )
    .await?;
    Ok(WebResponse::ok(true))
}

/// Change user status
///
/// # Endpoint
/// `POST /api/user/change-status`
///
/// # Authorization
/// Requires `system_admin` role
/// User write permission on the User resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When user is not found
async fn change_status(req: Json<ChangeUserStatus>) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    if !UserRepository::exists_by_id(req.id).await? {
        return Err(WebError::NotFound(EntityType::User.to_string()));
    }
    UserRepository::update::<DatabaseConnection>(req.into_active_model(), None).await?;
    Ok(WebResponse::ok(true))
}
