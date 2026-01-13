use actix_web::{http::Method, web};
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE, domain::prelude::MenuTree, enums::common::Status,
    web::WebResponse, PermChecker, DEFAULT_ROOT_TREE_ID,
};
use ng_gateway_repository::menu::MenuRepository;
use ng_gateway_utils::tree::build_tree;
use tracing::{info, instrument};

use crate::middleware::RequestContext;
use crate::rbac::has_any_role;

pub(super) const ROUTER_PREFIX: &str = "/menu";

/// Configure user routes
///
/// # Description
/// Registers all menu management endpoints with the Actix web service
///
/// # Routes
/// - GET `/all-tree`: Retrieve all menu tree
/// - GET `/authorized`: Retrieve authorized menu tree for current user
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/all", web::get().to(all_tree))
        .route("/accessable", web::get().to(accessable_tree));
}

/// Initialize RBAC rules for menu module
///
/// # Description
/// Sets up role-based access control rules for the menu management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-menu-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing menu module RBAC rules...");

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/all"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    info!("Menu module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve all menu tree excluding specific paths
///
/// # Description
/// Returns all enabled menus excluding those with path '/system/tenant-package'
async fn all_tree() -> WebResult<WebResponse<Vec<MenuTree>>> {
    let menus = MenuRepository::find_all_with_status(Status::Enabled).await?;
    // Filter out menus with path "/system/tenant-package"
    let (_, filtered_menus) = menus.into_iter().partition(|menu| {
        menu.path
            .as_ref()
            .is_none_or(|path| path.contains("tenant-package"))
    });

    Ok(WebResponse::ok(build_tree(
        filtered_menus,
        Some(DEFAULT_ROOT_TREE_ID),
    )))
}

/// Retrieve authorized menu tree for current user
///
/// # Description
/// Returns menu tree based on user's permissions and roles
async fn accessable_tree(ctx: RequestContext) -> WebResult<WebResponse<Vec<MenuTree>>> {
    let roles = ctx.roles.map(|roles| roles.roles);
    match roles {
        Some(roles) => {
            let menus = MenuRepository::find_all_with_roles_and_status(
                roles.iter().map(|role| role.id).collect(),
                Status::Enabled,
            )
            .await?;
            Ok(WebResponse::ok(build_tree(
                menus,
                Some(DEFAULT_ROOT_TREE_ID),
            )))
        }
        None => Ok(WebResponse::ok(vec![])),
    }
}
