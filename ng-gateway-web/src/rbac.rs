use casbin::CachedEnforcer;
use ng_gateway_common::{casbin::service::NGCasbinService, NGAppContext};
use ng_gateway_error::{rbac::RBACError, NGError, NGResult};
use ng_gateway_models::{
    casbin::{CasbinCmd, CasbinResult},
    domain::prelude::Claims,
    enums::common::{EntityType, Operation},
    rbac::BasePermRule,
    CasbinService,
};
use std::{future::Future, pin::Pin, sync::Arc};

// 定义权限检查闭包类型
type PermCheckFn = dyn Fn(&str, &str, Arc<Claims>) -> Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    + Send
    + Sync;

#[inline]
async fn get_casbin_service() -> NGResult<Arc<NGCasbinService<CachedEnforcer>>> {
    let ctx = NGAppContext::instance().await;
    ctx.casbin_service()?
        .downcast_arc::<NGCasbinService<CachedEnforcer>>()
        .map_err(|_| NGError::from("Casbin service not initialized"))
}

/// Creates a permission rule that checks if the user has a specific role
///
/// # Arguments
/// * `role` - The role code to check for
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has the specified role, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_role(role: &'static str) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if role.trim().is_empty() {
        return Err(RBACError::InvalidValue("Role cannot be empty".to_string()));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            let cmd = CasbinCmd::HasRole(grant.username.to_string(), role.to_string(), None);

            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::HasRole(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has all the specified roles
///
/// This rule passes only if the user has every role in the provided list.
///
/// # Arguments
/// * `roles` - A slice of role codes that the user must have
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has all specified roles, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_roles(roles: &'static [&str]) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if roles.is_empty() {
        return Err(RBACError::InvalidValue(
            "Roles list cannot be empty".to_string(),
        ));
    }

    // Check if any role is empty
    if roles.iter().any(|r| r.trim().is_empty()) {
        return Err(RBACError::InvalidValue(
            "Role names cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            let cmd = CasbinCmd::HasRoles(
                grant.username.to_string(),
                roles.iter().map(ToString::to_string).collect(),
                None,
            );

            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::HasRoles(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has any of the specified roles
///
/// This rule passes if the user has at least one role from the provided list.
///
/// # Arguments
/// * `roles` - A slice of role codes to check for
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has at least one of the specified roles
#[inline]
#[allow(unused)]
pub fn has_any_role(roles: &'static [&str]) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if roles.is_empty() {
        return Err(RBACError::InvalidValue("Roles is empty".to_string()));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            let cmd = CasbinCmd::HasAnyRole(
                grant.username.to_string(),
                roles.iter().map(ToString::to_string).collect(),
                None,
            );
            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::HasAnyRole(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has permission to perform all specified operations on a resource
///
/// This rule passes only if the user has permission for every operation in the provided list.
///
/// # Arguments
/// * `resource` - The resource type (enum value from EntityType)
/// * `operations` - A vector of operations to check for
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has all the specified resource permissions, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_resource_operations(
    resource: EntityType,
    operations: &'static [Operation],
) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if operations.is_empty() {
        return Err(RBACError::InvalidValue(
            "Operations list cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            for operation in operations {
                let cmd = CasbinCmd::Enforce(vec![
                    grant.username.to_string(),
                    resource.to_string(),
                    operation.to_string(),
                    "resource".to_string(),
                ]);

                match casbin_service
                    .handle_cmd(cmd)
                    .await
                    .map_err(|_| RBACError::Primitive)?
                {
                    CasbinResult::Enforce(false) => return Ok(false),
                    _ => continue,
                }
            }

            Ok(true)
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has permission to perform any of the specified operations on a resource
///
/// This rule passes if the user has permission for at least one operation from the provided list.
///
/// # Arguments
/// * `resource` - The resource type (enum value from EntityType)
/// * `operations` - A vector of operations to check for
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has any of the specified resource permissions, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_any_resource_operation(
    resource: EntityType,
    operations: &'static [Operation],
) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if operations.is_empty() {
        return Err(RBACError::InvalidValue(
            "Operations list cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            for operation in operations {
                let cmd = CasbinCmd::Enforce(vec![
                    grant.username.to_string(),
                    resource.to_string(),
                    operation.to_string(),
                    "resource".to_string(),
                ]);

                match casbin_service
                    .handle_cmd(cmd)
                    .await
                    .map_err(|_| RBACError::Primitive)?
                {
                    CasbinResult::Enforce(true) => return Ok(true),
                    _ => continue,
                }
            }

            Ok(false)
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has permission to perform an operation on a resource
///
/// # Arguments
/// * `resource` - The resource type (enum value from EntityType)
/// * `operation` - The operation to perform (enum value from Operation)
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has the specified resource permission
#[inline]
#[allow(unused)]
pub fn has_resource_operation(
    resource: EntityType,
    operation: Operation,
) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            // Use Casbin to check if the user has permission for the resource operation
            let cmd = CasbinCmd::Enforce(vec![
                grant.username.to_string(),
                resource.to_string(),
                operation.to_string(),
                "resource".to_string(),
            ]);

            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::Enforce(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has permission to access an API endpoint
///
/// # Arguments
/// * `path` - The API path to check (e.g., "/api/device/create")
/// * `method` - The HTTP method to check (e.g., "GET", "POST")
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has the specified API permission, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_api_permission(
    path: &'static str,
    method: &'static str,
) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if path.trim().is_empty() {
        return Err(RBACError::InvalidValue(
            "API path cannot be empty".to_string(),
        ));
    }

    if method.trim().is_empty() {
        return Err(RBACError::InvalidValue(
            "HTTP method cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            // Use Casbin to check if the user has permission for the API endpoint
            let cmd = CasbinCmd::Enforce(vec![
                grant.username.to_string(),
                path.to_string(),
                method.to_string(),
                "api".to_string(),
            ]);

            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::Enforce(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has a specific OAuth scope
///
/// # Arguments
/// * `scope` - The scope to check for (e.g., "device:read")
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has the specified scope, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_scope(scope: &'static str) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if scope.trim().is_empty() {
        return Err(RBACError::InvalidValue("Scope cannot be empty".to_string()));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            // Use Casbin to check if the client has the specified scope
            let cmd = CasbinCmd::Enforce(vec![
                grant.username.to_string(),
                scope.to_string(),
                "access".to_string(),
                "scope".to_string(),
            ]);

            match casbin_service
                .handle_cmd(cmd)
                .await
                .map_err(|_| RBACError::Primitive)?
            {
                CasbinResult::Enforce(result) => Ok(result),
                _ => Ok(false),
            }
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has all of the specified OAuth scopes
///
/// This rule passes only if the user has every scope in the provided list.
///
/// # Arguments
/// * `scopes` - A slice of scopes that the user must have
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has all specified scopes, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_scopes(scopes: &'static [&str]) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if scopes.is_empty() {
        return Err(RBACError::InvalidValue(
            "Scopes list cannot be empty".to_string(),
        ));
    }

    // Check if any scope is empty
    if scopes.iter().any(|s| s.trim().is_empty()) {
        return Err(RBACError::InvalidValue(
            "Scope names cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            for scope in scopes {
                let cmd = CasbinCmd::Enforce(vec![
                    grant.username.to_string(),
                    scope.to_string(),
                    "access".to_string(),
                    "scope".to_string(),
                ]);

                match casbin_service
                    .handle_cmd(cmd)
                    .await
                    .map_err(|_| RBACError::Primitive)?
                {
                    CasbinResult::Enforce(false) => return Ok(false),
                    _ => continue,
                }
            }

            Ok(true)
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}

/// Creates a permission rule that checks if the user has any of the specified OAuth scopes
///
/// This rule passes if the user has at least one scope from the provided list.
///
/// # Arguments
/// * `scopes` - A slice of scopes to check for
///
/// # Returns
/// * `BasePermRule` - A permission rule that passes if the user has at least one of the specified scopes, or an error if parameters are invalid
#[inline]
#[allow(unused)]
pub fn has_any_scope(
    scopes: &'static [&str],
) -> NGResult<BasePermRule<Box<PermCheckFn>>, RBACError> {
    if scopes.is_empty() {
        return Err(RBACError::InvalidValue(
            "Scopes list cannot be empty".to_string(),
        ));
    }

    let check_fn = Box::new(move |_method: &str, _path: &str, grant: Arc<Claims>| {
        Box::pin(async move {
            let casbin_service = get_casbin_service()
                .await
                .map_err(|_| RBACError::Primitive)?;

            for scope in scopes {
                if scope.trim().is_empty() {
                    continue;
                }

                let cmd = CasbinCmd::Enforce(vec![
                    grant.username.to_string(),
                    scope.to_string(),
                    "access".to_string(),
                    "scope".to_string(),
                ]);

                match casbin_service
                    .handle_cmd(cmd)
                    .await
                    .map_err(|_| RBACError::Primitive)?
                {
                    CasbinResult::Enforce(true) => return Ok(true),
                    _ => continue,
                }
            }

            Ok(false)
        }) as Pin<Box<dyn Future<Output = NGResult<bool, RBACError>> + Send>>
    });

    Ok(BasePermRule::new(check_fn))
}
