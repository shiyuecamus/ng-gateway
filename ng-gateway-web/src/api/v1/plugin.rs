use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_multipart::Multipart;
use actix_web::{http::Method, web};
use actix_web_validator::Path;
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::{CUSTOM_DIR, PLUGIN_DIR, SYSTEM_ADMIN_ROLE_CODE},
    domain::prelude::{NewPlugin, PageResult, PathId, PluginInfo, PluginPageParams},
    enums::common::{EntityType, Operation, SourceType},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{AppRepository, PluginRepository};
use ng_gateway_sdk::{probe_north_library, BinaryOsType, NorthwardProbeInfo};
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::{path::PathBuf, sync::Arc};
use tempfile::{Builder, NamedTempFile};
use tokio::{fs::File, io::AsyncWriteExt};
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/plugin";

/// Configure northward plugin routes
///
/// # Description
/// Registers all northward plugin management endpoints with the Actix web service
///
/// # Routes
/// - POST `/install`: Install a new northward plugin
/// - DELETE `/{id}`: Uninstall a northward plugin
/// - POST `/probe`: Probe a northward plugin
/// - GET `/list`: Retrieve a list of all northward plugins
/// - GET `/page`: Retrieve paginated list of northward plugins
/// - GET `/detail/{id}`: Retrieve northward plugin details by ID
/// - GET `/metadata/{id}`: Retrieve northward plugin metadata by ID
/// - GET `/referenced/{id}`: Check if plugin is referenced by any app
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/install", web::post().to(install))
        .route("/{id}", web::delete().to(uninstall))
        .route("/probe", web::post().to(probe))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/metadata/{id}", web::get().to(get_metadata_by_id))
        .route("/referenced/{id}", web::get().to(is_referenced));
}

/// Initialize RBAC rules for northward plugin module
#[inline]
#[instrument(name = "init-northward-plugin-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing northward plugin module RBAC rules...");

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/install"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Plugin,
                    Operation::Create,
                )?)
                .or(has_scope("northward-plugin:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Plugin,
                    Operation::Delete,
                )?)
                .or(has_scope("northward-plugin:delete")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/probe"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/metadata/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/referenced/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Plugin, Operation::Read)?)
                .or(has_scope("northward-plugin:read")?),
        )
        .await?;

    info!("Northward plugin module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve a list of all northward plugins
pub async fn list() -> WebResult<WebResponse<Vec<PluginInfo>>> {
    Ok(WebResponse::ok(PluginRepository::find_all().await?))
}

/// Retrieve paginated list of northward plugins
pub async fn page(
    params: web::Query<PluginPageParams>,
) -> WebResult<WebResponse<PageResult<PluginInfo>>> {
    Ok(WebResponse::ok(
        PluginRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve northward plugin details by ID
pub async fn get_by_id(req: Path<PathId>) -> WebResult<WebResponse<PluginInfo>> {
    Ok(WebResponse::ok(
        PluginRepository::find_info_by_id(req.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Plugin.to_string()))?,
    ))
}

/// Upload and install a new northward plugin
pub async fn install(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    // Save upload to target directory as temporary file
    let temp_file = save_to_target_dir(&mut multipart, PLUGIN_DIR, CUSTOM_DIR).await?;
    let temp_path = temp_file.path().to_path_buf();

    // Probe library
    let probe = probe_north_library(temp_path.as_path())
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    // Generate final filename based on UUID
    let ext = temp_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("dylib");
    let final_filename = format!("{}.{}", uuid::Uuid::new_v4(), ext);
    let final_path = PathBuf::from(PLUGIN_DIR)
        .join(CUSTOM_DIR)
        .join(&final_filename);

    let plugin = NewPlugin {
        name: probe.name,
        description: probe.description,
        plugin_type: probe.plugin_type,
        source: SourceType::Custom,
        version: probe.version,
        api_version: probe.api_version,
        sdk_version: probe.sdk_version,
        os_type: probe.os_type.into(),
        os_arch: probe.os_arch.into(),
        size: probe.size,
        path: final_path.to_string_lossy().to_string(),
        checksum: probe.checksum,
        metadata: serde_json::to_value(&probe.metadata)
            .map_err(|e| WebError::InternalError(e.to_string()))?,
    }
    .into_active_model();

    state.validator.validate(&plugin, Operation::Create).await?;

    let created = PluginRepository::create::<DatabaseConnection>(plugin, None).await?;

    // Call gateway to install
    match state
        .gateway
        .install_plugin(created.id, temp_path.as_path())
        .await
    {
        Ok(_) => {
            // Persist temp file to final path
            temp_file.persist(&final_path).map_err(|e| {
                WebError::InternalError(format!("Failed to persist plugin file: {}", e.error))
            })?;
            Ok(WebResponse::ok(true))
        }
        Err(e) => {
            PluginRepository::delete::<DatabaseConnection>(created.id, None).await?;
            Err(WebError::from(e))
        }
    }
}

/// Uninstall a northward plugin
pub async fn uninstall(
    req: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let existing_plugin = PluginRepository::find_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Plugin.to_string()))?;

    // Check if the plugin is a built-in plugin
    if existing_plugin.source == SourceType::BuiltIn {
        return Err(WebError::BadRequest(
            "Cannot uninstall built-in plugin".to_string(),
        ));
    }

    let path = PathBuf::from(existing_plugin.path);
    match state.gateway.uninstall_plugin(req.id, path.as_path()).await {
        Ok(_) => {
            PluginRepository::delete::<DatabaseConnection>(req.id, None).await?;
            Ok(WebResponse::ok(true))
        }
        Err(e) => Err(WebError::from(e)),
    }
}

/// Probe a northward plugin
pub async fn probe(mut multipart: Multipart) -> WebResult<WebResponse<NorthwardProbeInfo>> {
    // Save to system temp directory
    let temp_file = save_to_system_temp(&mut multipart).await?;
    let temp_path = temp_file.path();
    let meta = match probe_north_library(temp_path) {
        Ok(p) => p,
        Err(e) => return Ok(WebResponse::error(&e.to_string())),
    };
    Ok(WebResponse::ok(meta))
}

/// Check if plugin is referenced by any app
pub async fn is_referenced(req: Path<PathId>) -> WebResult<WebResponse<bool>> {
    Ok(WebResponse::ok(
        AppRepository::exists_by_plugin_id(req.id).await?,
    ))
}

/// Get UI metadata (schema) for a given plugin
pub async fn get_metadata_by_id(req: Path<PathId>) -> WebResult<WebResponse<serde_json::Value>> {
    let model = PluginRepository::find_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Plugin.to_string()))?;
    Ok(WebResponse::ok(model.metadata))
}

/// Save uploaded file to system temporary directory (for probe operations)
async fn save_to_system_temp(multipart: &mut Multipart) -> WebResult<NamedTempFile> {
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;

    let content_disposition = field.content_disposition();
    let uploaded_name = content_disposition.and_then(|cd| cd.get_filename().map(String::from));
    let ext = uploaded_name
        .and_then(|n| {
            let path = std::path::Path::new(&n);
            path.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        })
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))?;

    // Basic extension check
    let current_os = BinaryOsType::current();
    let expected_ext = current_os.expected_driver_ext();
    if !expected_ext.is_empty() && ext != expected_ext {
        return Err(WebError::BadRequest(format!(
            "Invalid plugin file extension: .{} (expected .{} for {:?})",
            ext, expected_ext, current_os
        )));
    }

    // Create temp file in system temp directory
    let temp_file = Builder::new()
        .prefix("ng-plugin-probe-")
        .suffix(&format!(".{}", ext))
        .tempfile()
        .map_err(|e| WebError::InternalError(format!("Failed to create temp file: {}", e)))?;

    // Write uploaded data to temp file
    let mut f = File::from_std(
        temp_file
            .reopen()
            .map_err(|e| WebError::InternalError(format!("Failed to reopen temp file: {}", e)))?,
    );

    while let Some(chunk) = field.next().await {
        let data = chunk?;
        f.write_all(&data).await?;
    }
    f.sync_all().await?;

    Ok(temp_file)
}

/// Save uploaded file to target directory as temporary file (for install operations)
async fn save_to_target_dir(
    multipart: &mut Multipart,
    base_dir: &str,
    sub_dir: &str,
) -> WebResult<NamedTempFile> {
    let target_dir = PathBuf::from(base_dir).join(sub_dir);
    if !target_dir.exists() || !target_dir.is_dir() {
        return Err(WebError::InternalError(format!(
            "Directory {} does not exist",
            target_dir.display()
        )));
    }

    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;

    let content_disposition = field.content_disposition();
    let uploaded_name = content_disposition.and_then(|cd| cd.get_filename().map(String::from));
    let ext = uploaded_name
        .and_then(|n| {
            let path = std::path::Path::new(&n);
            path.extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_string())
        })
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))?;

    // Basic extension check
    let current_os = BinaryOsType::current();
    let expected_ext = current_os.expected_driver_ext();
    if !expected_ext.is_empty() && ext != expected_ext {
        return Err(WebError::BadRequest(format!(
            "Invalid plugin file extension: .{} (expected .{} for {:?})",
            ext, expected_ext, current_os
        )));
    }

    // Create temp file in target directory
    let temp_file = Builder::new()
        .prefix("ng-plugin-")
        .suffix(&format!(".{}", ext))
        .tempfile_in(&target_dir)
        .map_err(|e| WebError::InternalError(format!("Failed to create temp file: {}", e)))?;

    // Write uploaded data to temp file
    let mut f = File::from_std(
        temp_file
            .reopen()
            .map_err(|e| WebError::InternalError(format!("Failed to reopen temp file: {}", e)))?,
    );

    while let Some(chunk) = field.next().await {
        let data = chunk?;
        f.write_all(&data).await?;
    }
    f.sync_all().await?;

    Ok(temp_file)
}
