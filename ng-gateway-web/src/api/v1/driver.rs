use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_multipart::Multipart;
use actix_web::{http::header, http::Method, web, HttpResponse};
use actix_web_validator::{Path, Query};
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::{
    constants::{CUSTOM_DIR, DRIVER_DIR, SYSTEM_ADMIN_ROLE_CODE},
    domain::prelude::{
        DriverInfo, DriverPageParams, NewDriver, PageResult, PathEntityId, PathId, TemplateQuery,
    },
    enums::common::{EntityType, Operation, SourceType},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{ChannelRepository, DriverRepository};
use ng_gateway_sdk::{
    probe_driver_library, BinaryOsType, DriverProbeInfo, DriverSchemas, FlattenEntity,
    TemplateMetadata,
};
use sea_orm::{DatabaseConnection, IntoActiveModel};
use std::{path::PathBuf, sync::Arc};
use tempfile::{Builder, NamedTempFile};
use tokio::{fs::File, io::AsyncWriteExt};
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/driver";

/// Configure driver routes
///
/// # Description
/// Registers all driver management endpoints with the Actix web service
///
/// # Routes
/// - POST `/install`: Install a new driver
/// - DELETE `/{id}`: Uninstall a driver
/// - POST `/probe`: Probe a driver
/// - GET `/list`: Retrieve a list of all drivers
/// - GET `/page`: Retrieve paginated list of drivers
/// - GET `/detail/{id}`: Retrieve driver details by ID
/// - GET `/metadata/{id}`: Retrieve driver metadata by ID
/// - GET `/referenced/{id}`: Check if driver is referenced by any channel
/// - GET `/template/{id}/{entity}`: Download template for a driver/entity
/// - POST `/import/{id}/{entity}`: Import an Excel file for a driver/entity
/// - POST `/import/{id}/{entity}/commit`: Import and commit an Excel file for a driver/entity
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/install", web::post().to(install))
        .route("/{id}", web::delete().to(uninstall))
        .route("/probe", web::post().to(probe))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/metadata/{id}", web::get().to(get_metadata_by_id))
        .route("/referenced/{id}", web::get().to(is_referenced))
        .route("/template/{id}/{entity}", web::get().to(download_template));
}

/// Initialize RBAC rules for driver module
///
/// # Description
/// Sets up role-based access control rules for the driver management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `WebResult<()>`: Success or error result
#[inline]
#[instrument(name = "init-driver-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing driver module RBAC rules...");

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/install"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Driver,
                    Operation::Create,
                )?)
                .or(has_scope("driver:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Driver,
                    Operation::Delete,
                )?)
                .or(has_scope("driver:delete")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/probe"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/metadata/{{driver_type}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Driver,
                    Operation::Write,
                )?)
                .or(has_scope("driver:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/referenced/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    // Template download (read)
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/template/{{id}}/{{entity}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Driver, Operation::Read)?)
                .or(has_scope("driver:read")?),
        )
        .await?;

    info!("Driver module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve a list of all drivers
///
/// # Endpoint
/// `GET /api/driver/list`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<Vec<DriverInfo>>>`: Success or error result
pub async fn list() -> WebResult<WebResponse<Vec<DriverInfo>>> {
    Ok(WebResponse::ok(DriverRepository::find_all().await?))
}

/// Retrieve paginated list of drivers
///
/// # Endpoint
/// `GET /api/driver/page`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<PageResult<DriverInfo>>>`: Success or error result
///
/// # Errors
/// - Internal Server Error (500): When page fails
pub async fn page(
    params: Query<DriverPageParams>,
) -> WebResult<WebResponse<PageResult<DriverInfo>>> {
    Ok(WebResponse::ok(
        DriverRepository::page(params.into_inner()).await?,
    ))
}

/// Retrieve driver details by ID
///
/// # Endpoint
/// `GET /api/driver/detail/{id}`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<DriverInfo>>>`: Success or error result
///
/// # Errors
/// - Not Found (404): When driver is not found
/// - Internal Server Error (500): When get by id fails
pub async fn get_by_id(req: Path<PathId>) -> WebResult<WebResponse<DriverInfo>> {
    Ok(WebResponse::ok(
        DriverRepository::find_info_by_id(req.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?,
    ))
}

/// Upload and install a new driver
///
/// # Endpoint
/// `POST /api/driver/install`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: Success or error result
///
/// # Errors
/// - Bad Request (400): When driver is built-in
/// - Internal Server Error (500): When install fails
pub async fn install(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    // Save upload to target directory as temporary file (auto-cleanup on failure)
    let temp_file = save_to_target_dir(&mut multipart, DRIVER_DIR, CUSTOM_DIR).await?;
    let temp_path = temp_file.path().to_path_buf();

    // Probe library (platform/API gates), includes checksum/size/os/arch
    let probe = probe_driver_library(temp_path.as_path())
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    // Generate final filename based on UUID
    let ext = temp_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("dylib");
    let final_filename = format!("{}.{}", uuid::Uuid::new_v4(), ext);
    let final_path = PathBuf::from(DRIVER_DIR)
        .join(CUSTOM_DIR)
        .join(&final_filename);

    let driver = NewDriver {
        name: probe.name,
        description: probe.description,
        driver_type: probe.driver_type.to_ascii_lowercase(),
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

    state.validator.validate(&driver, Operation::Create).await?;

    let created = DriverRepository::create::<DatabaseConnection>(driver, None).await?;

    // Call gateway to install (persist as Disabled); cleanup temp file on failure
    match state
        .gateway
        .install_driver(created.id, temp_path.as_path())
        .await
    {
        Ok(_) => {
            // Persist temp file to final path (atomic rename within same filesystem)
            temp_file.persist(&final_path).map_err(|e| {
                WebError::InternalError(format!("Failed to persist driver file: {}", e.error))
            })?;
            Ok(WebResponse::ok(true))
        }
        Err(e) => {
            DriverRepository::delete::<DatabaseConnection>(created.id, None).await?;
            // temp_file auto-deleted on drop
            Err(WebError::from(e))
        }
    }
}

/// Uninstall a driver
///
/// # Endpoint
/// `DELETE /api/driver/{id}`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: Success or error result
///
/// # Errors
/// - Bad Request (400): When driver is built-in
/// - Internal Server Error (500): When uninstall fails
pub async fn uninstall(
    req: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let existing_driver = DriverRepository::find_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;

    // Check if the role is a built-in role
    if existing_driver.source == SourceType::BuiltIn {
        return Err(WebError::BadRequest(
            "Cannot uninstall built-in driver".to_string(),
        ));
    }
    let path = PathBuf::from(existing_driver.path);
    match state.gateway.uninstall_driver(req.id, path.as_path()).await {
        Ok(_) => {
            DriverRepository::delete::<DatabaseConnection>(req.id, None).await?;
            Ok(WebResponse::ok(true))
        }
        Err(e) => Err(WebError::from(e)),
    }
}

/// Probe a driver
///
/// # Endpoint
/// `POST /api/driver/probe`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<DriverProbeInfo>>`: Success or error result
///
/// # Errors
/// - Internal Server Error (500): When probe fails
pub async fn probe(mut multipart: Multipart) -> WebResult<WebResponse<DriverProbeInfo>> {
    // Save to system temp directory (auto-cleanup after probe)
    let temp_file = save_to_system_temp(&mut multipart).await?;
    let temp_path = temp_file.path();
    let meta = match probe_driver_library(temp_path) {
        Ok(p) => p,
        Err(e) => return Ok(WebResponse::error(&e.to_string())),
    };
    // temp_file auto-deleted on drop
    Ok(WebResponse::ok(meta))
}

/// Check if driver is referenced by any channel
///
/// # Endpoint
/// `GET /api/driver/referenced/{id}`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: Success or error result
pub async fn is_referenced(req: Path<PathId>) -> WebResult<WebResponse<bool>> {
    let count = ChannelRepository::count_by_driver_id(req.id).await?;
    Ok(WebResponse::ok(count > 0))
}

/// Get UI metadata (schema) for a given driverType.
/// Source of truth: DB stored driver schemas.
/// # Endpoint
/// `GET /api/driver/metadata/{driver_id}`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<WebResponse<DriverSchemas>>`: Success or error result
///
/// # Errors
/// - Not Found (404): When driver is not found
/// - Internal Server Error (500): When get metadata by type fails
pub async fn get_metadata_by_id(req: Path<PathId>) -> WebResult<WebResponse<DriverSchemas>> {
    let list = DriverRepository::find_by_id(req.id).await?;
    let model = list.ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    Ok(WebResponse::ok(
        serde_json::from_value(model.metadata).map_err(|e| {
            WebError::InternalError(format!("Invalid driver schemas for {}: {}", req.id, e))
        })?,
    ))
}

/// Download Excel template for a driver/entity with localized headers.
/// # Endpoint
/// `GET /api/driver/template/{id}/{entity}`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role
///
/// # Returns
/// - `WebResult<HttpResponse>`: Success or error result
///
/// # Errors
/// - Not Found (404): When driver is not found
/// - Internal Server Error (500): When download template fails
pub async fn download_template(
    path: Path<PathEntityId>,
    query: Query<TemplateQuery>,
) -> WebResult<HttpResponse> {
    let query = query.into_inner();

    let driver = DriverRepository::find_by_id(path.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;
    let entity = FlattenEntity::try_from(path.entity.as_str())
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let locale = query.locale.unwrap_or("zh-CN".to_string());
    let template = schemas.build_template(entity, &locale);

    // Generate workbook in-memory and return as attachment
    // Special handling for device-points to use descriptive filename
    let filename = if path.entity.eq_ignore_ascii_case("device-points") {
        format!("{}_device_points_template.xlsx", driver.driver_type)
    } else {
        format!(
            "{}_{}_template.xlsx",
            driver.driver_type,
            path.entity.to_ascii_lowercase()
        )
    };
    let bytes = template
        .write_with_meta_to_buffer(&TemplateMetadata {
            driver_type: driver.driver_type,
            driver_version: Some(driver.version),
            api_version: Some(driver.api_version.to_string()),
            entity: entity.to_string(),
            locale,
            schema_version: "1.0".to_string(),
        })
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    Ok(HttpResponse::Ok()
        .insert_header((
            header::CONTENT_TYPE,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        ))
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        ))
        .body(bytes))
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

    // Basic extension check to fail fast (OS-aware)
    let current_os = BinaryOsType::current();
    let expected_ext = current_os.expected_driver_ext();
    if !expected_ext.is_empty() && ext != expected_ext {
        return Err(WebError::BadRequest(format!(
            "Invalid driver file extension: .{} (expected .{} for {:?})",
            ext, expected_ext, current_os
        )));
    }

    // Create temp file in system temp directory with proper extension
    let temp_file = Builder::new()
        .prefix("ng-driver-probe-")
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
/// This avoids cross-filesystem moves when persisting
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

    // Basic extension check to fail fast (OS-aware)
    let current_os = BinaryOsType::current();
    let expected_ext = current_os.expected_driver_ext();
    if !expected_ext.is_empty() && ext != expected_ext {
        return Err(WebError::BadRequest(format!(
            "Invalid driver file extension: .{} (expected .{} for {:?})",
            ext, expected_ext, current_os
        )));
    }

    // Create temp file in target directory to avoid cross-filesystem moves
    let temp_file = Builder::new()
        .prefix("ng-driver-")
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
