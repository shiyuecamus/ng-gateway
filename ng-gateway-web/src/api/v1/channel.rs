use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};
use actix_multipart::Multipart;
use actix_web::{
    http::Method,
    web::{self, ServiceConfig},
};
use actix_web_validator::{Json, Path, Query};
use bytes::BytesMut;
use futures::StreamExt;
use ng_gateway_common::log::control as log_control;
use ng_gateway_common::{casbin::NGPermChecker, log::control::LogOverrideScope};
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        ChangeChannelStatus, ChannelInfo, ChannelLogLevelView, ChannelLogOverrideView,
        ChannelPageParams, CommitResult, DeviceGroup, DeviceInfo, DeviceRef, ImportPreview,
        NewChannel, NewDevice, NewPoint, PageResult, PathId, PreparedDeviceCommit,
        PreparedDevicePointsCommit, SetChannelLogLevelRequest, TtlRange, UpdateChannel,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{ChannelRepository, DeviceRepository, DriverRepository};
use ng_gateway_sdk::{
    DriverSchemas, FieldError, FlattenEntity, FromValidatedRow, RowMappingContext, ValidatedRow,
    ValidationCode,
};
use sea_orm::IntoActiveModel;
use std::{
    collections::{HashMap, HashSet},
    io::Cursor,
    sync::Arc,
};

use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/channel";

/// Configure channel routes
///
/// # Description
/// Registers all channel management endpoints with the Actix web service
///
/// # Routes
/// - GET `/list`: Retrieve a list of all channels
/// - GET `/page`: Retrieve paginated list of channels
/// - GET `/detail/{id}`: Retrieve channel details by ID
/// - POST `/create`: Create a new channel
/// - PUT `/update`: Update channel information
/// - DELETE `/delete/{id}`: Delete channel
/// - POST `/change-status`: Change channel status
pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/list", web::get().to(list))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/change-status", web::put().to(change_status))
        .route("/{id}", web::delete().to(delete))
        .route("/{id}/sub-devices", web::get().to(get_sub_devices))
        .route("/{id}/log-level", web::get().to(get_channel_log_level))
        .route("/{id}/log-level", web::put().to(set_channel_log_level))
        .route("/{id}/log-level", web::delete().to(clear_channel_log_level))
        .route(
            "{id}/import-device-preview",
            web::post().to(import_device_preview),
        )
        .route(
            "{id}/import-device-commit",
            web::post().to(import_device_commit),
        )
        .route(
            "{id}/import-device-points-preview",
            web::post().to(import_device_points_preview),
        )
        .route(
            "{id}/import-device-points-commit",
            web::post().to(import_device_points_commit),
        );
}

/// Initialize RBAC rules for channel module
///
/// # Description
/// Sets up role-based access control rules for the channel management endpoints
///
/// # Parameters
/// - `router_prefix`: Base URL prefix for all routes
/// - `perm_checker`: Permission checker instance for registering rules
///
/// # Returns
/// - `NGResult<(), RBACError>`: Success or error result
#[inline]
#[instrument(name = "init-channel-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    let rules = vec![
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Create,
                )?)
                .or(has_scope("channel:create")?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Delete,
                )?)
                .or(has_scope("channel:delete")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-points-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-points-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/sub-devices"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/log-level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("Channel module RBAC rules initialized successfully");
    Ok(())
}

/// Retrieve a list of all channels
///
/// # Endpoint
/// `GET /api/channel/list`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<Vec<ChannelInfo>>>`: List of all channels on success
///   or appropriate error response
pub async fn list(state: web::Data<Arc<AppState>>) -> WebResult<WebResponse<Vec<ChannelInfo>>> {
    let mut channels = ChannelRepository::find_all().await?;

    // Enrich with connection states from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let southward_manager = state.gateway.southward_manager();
    for channel in channels.iter_mut() {
        channel.connection_state = southward_manager.get_channel_connection_state(channel.id);
    }

    Ok(WebResponse::ok(channels))
}

/// Retrieve paginated list of channels
///
/// # Endpoint
/// `GET /api/channel/page`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<PageResult<ChannelInfo>>>`: Paginated list of channels on success
///   or appropriate error response
pub async fn page(
    params: Query<ChannelPageParams>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<ChannelInfo>>> {
    let mut result = ChannelRepository::page(params.into_inner()).await?;

    // Enrich with connection states from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let southward_manager = state.gateway.southward_manager();
    for channel in result.records.iter_mut() {
        channel.connection_state = southward_manager.get_channel_connection_state(channel.id);
    }

    Ok(WebResponse::ok(result))
}

/// Retrieve channel details by ID
///
/// # Endpoint
/// `GET /api/channel/detail/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<ChannelInfo>>`: Channel details on success
///   or appropriate error response
pub async fn get_by_id(
    req: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<ChannelInfo>> {
    let mut channel = ChannelRepository::find_info_by_id(req.id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Channel.to_string()))?;

    // Enrich with connection state from runtime manager
    // Use trait method to get connection state without depending on concrete type
    let southward_manager = state.gateway.southward_manager();
    channel.connection_state = southward_manager.get_channel_connection_state(channel.id);

    Ok(WebResponse::ok(channel))
}

/// Create a new channel
///
/// # Endpoint
/// `POST /api/channel/create`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel create permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When channel data is invalid
/// - Internal Server Error (500): When channel creation fails
pub async fn create(
    channel: Json<NewChannel>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = channel.into_inner();
    state
        .validator
        .validate(&model.clone().into_active_model(), Operation::Create)
        .await?;

    match state.gateway.create_channel(model).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Update channel information
///
/// # Endpoint
/// `PUT /api/channel/update`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel write permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When channel data is invalid
/// - Internal Server Error (500): When channel update fails
pub async fn update(
    channel: Json<UpdateChannel>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let payload = channel.into_inner();
    state
        .validator
        .validate(&payload.clone().into_active_model(), Operation::Write)
        .await?;

    match state.gateway.update_channel(payload).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Delete channel
///
/// # Endpoint
/// `DELETE /api/channel/delete/{id}`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel delete permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Not Found (404): When channel is not found
/// - Internal Server Error (500): When channel deletion fails
pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_channel(params.id).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Change channel status
///
/// # Endpoint
/// `POST /api/channel/change-status`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel write permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
///
/// # Errors
/// - Bad Request (400): When channel data is invalid
/// - Internal Server Error (500): When channel status change fails
pub async fn change_status(
    req: Json<ChangeChannelStatus>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    let channel = match ChannelRepository::find_by_id(req.id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };
    match state
        .gateway
        .change_channel_status(channel, req.status)
        .await
    {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Get sub devices by channel id
///
/// # Endpoint
/// `GET /api/channel/{id}/sub-devices`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<Vec<DeviceInfo>>>`: List of sub devices on success
///   or appropriate error response
pub async fn get_sub_devices(params: Path<PathId>) -> WebResult<WebResponse<Vec<DeviceInfo>>> {
    let devices = DeviceRepository::find_by_channel_id(params.id).await?;
    Ok(WebResponse::ok(devices))
}

#[inline]
async fn build_channel_log_level_view(id: i32) -> Result<ChannelLogLevelView, WebError> {
    // Validate channel exists.
    ChannelRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Channel.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let overrides = rt.overrides();
    let effective = overrides.effective_channel_level(id);
    let lease = overrides.active_scope_lease(LogOverrideScope::Channel(id));
    let s = rt.settings();

    Ok(ChannelLogLevelView {
        channel_id: id,
        effective,
        r#override: lease.map(|l| ChannelLogOverrideView {
            level: l.level,
            ttl_ms: l.ttl_ms,
            expires_at_ms: l.expires_at_ms,
        }),
        ttl: TtlRange {
            min_ms: s.channel_override_min_ttl_ms,
            max_ms: s.channel_override_max_ttl_ms,
            default_ms: s.channel_override_default_ttl_ms,
        },
    })
}

#[instrument(name = "get-channel-log-level", skip_all)]
pub async fn get_channel_log_level(
    params: Path<PathId>,
) -> WebResult<WebResponse<ChannelLogLevelView>> {
    let id = params.id;
    Ok(WebResponse::ok(build_channel_log_level_view(id).await?))
}

#[instrument(name = "set-channel-log-level", skip_all)]
pub async fn set_channel_log_level(
    params: Path<PathId>,
    req: web::Json<SetChannelLogLevelRequest>,
) -> WebResult<WebResponse<ChannelLogLevelView>> {
    let id = params.id;
    let req = req.into_inner();

    // Validate channel exists (avoid leaking overrides for invalid ids).
    ChannelRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Channel.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let s = rt.settings();
    let ttl = req.ttl_ms.unwrap_or(s.channel_override_default_ttl_ms);
    s.validate_channel_ttl_ms(ttl)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;

    rt.overrides()
        .set_temporary_override(LogOverrideScope::Channel(id), req.level, ttl);

    Ok(WebResponse::ok(build_channel_log_level_view(id).await?))
}

#[instrument(name = "clear-channel-log-level", skip_all)]
pub async fn clear_channel_log_level(
    params: Path<PathId>,
) -> WebResult<WebResponse<ChannelLogLevelView>> {
    let id = params.id;

    ChannelRepository::find_by_id(id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Channel.to_string()))?;

    let rt = log_control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    rt.overrides().clear_scope(LogOverrideScope::Channel(id));

    Ok(WebResponse::ok(build_channel_log_level_view(id).await?))
}

/// Import device template
///
/// # Endpoint
/// `GET /api/channel/{id}/import-device`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<ImportPreview>>`: Preview of imported devices on success
///   or appropriate error response
pub async fn import_device_preview(
    params: Path<PathId>,
    mut multipart: Multipart,
) -> WebResult<WebResponse<ImportPreview>> {
    let channel = match ChannelRepository::find_by_id(params.id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read first uploaded part fully into memory (Excel file)
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    // Load driver schemas
    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;

    // Excel parsing + row validation is CPU-heavy and uses blocking I/O internally (zip/XML).
    // Run it on a dedicated blocking thread to avoid starving Actix workers.
    let buf = accumulated_data.freeze();
    let driver_type = driver.driver_type;
    let preview = tokio::task::spawn_blocking(move || -> Result<ImportPreview, WebError> {
        // Read metadata and rows together, then validate meta
        let template = schemas.build_template(FlattenEntity::Device, "zh-CN");
        let (metadata, rows) = template
            .read_with_meta_from_reader(Cursor::new(buf))
            .map_err(|e| WebError::InternalError(e.to_string()))?;
        metadata
            .validate(&driver_type, FlattenEntity::Device)
            .map_err(|e| WebError::BadRequest(e.to_string()))?;
        let template = schemas.build_template(FlattenEntity::Device, &metadata.locale);

        // Validate and normalize rows
        let total = rows.len();
        let (valids, errors, warn_count) =
            template.validate_and_normalize_rows(rows, &metadata.locale);

        Ok(ImportPreview {
            total_rows: total,
            valid: valids.len(),
            invalid: total.saturating_sub(valids.len()),
            warn: warn_count,
            errors: errors.into_iter().take(50).collect(),
        })
    })
    .await
    .map_err(|e| WebError::InternalError(format!("Failed to run import preview task: {e}")))??;

    Ok(WebResponse::ok(preview))
}

/// Import device template and commit
///
/// # Endpoint
/// `POST /api/channel/{id}/import-device`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<CommitResult>>`: Result of import and commit on success
///   or appropriate error response
pub async fn import_device_commit(
    params: Path<PathId>,
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<CommitResult>> {
    let channel = match ChannelRepository::find_by_id(params.id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read first uploaded part fully into memory (Excel file)
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    // Load driver schemas
    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;
    // Parse + validate + map on blocking pool to avoid starving Actix workers.
    let buf = accumulated_data.freeze();
    let channel_id = channel.id;
    let driver_type = driver.driver_type.clone();
    let prepared =
        tokio::task::spawn_blocking(move || -> Result<PreparedDeviceCommit, WebError> {
            // Parse workbook once (meta + rows), then validate meta.
            let template = schemas.build_template(FlattenEntity::Device, "zh-CN");
            let (metadata, rows) = template
                .read_with_meta_from_reader(Cursor::new(buf))
                .map_err(|e| WebError::InternalError(e.to_string()))?;
            metadata
                .validate(&driver_type, FlattenEntity::Device)
                .map_err(|e| WebError::BadRequest(e.to_string()))?;

            let locale = metadata.locale.clone();
            let template = schemas.build_template(FlattenEntity::Device, &locale);

            let total_rows = rows.len();
            let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &locale);
            let valid_count = valids.len();

            let devices: Vec<NewDevice> = template
                .map_to_domain(
                    valids,
                    RowMappingContext {
                        entity_id: channel_id,
                        driver_type: driver_type.clone(),
                        locale,
                    },
                )
                .map_err(|e| WebError::InternalError(e.to_string()))?;

            Ok(PreparedDeviceCommit {
                total_rows,
                warn_count,
                valid_count,
                devices,
                errors,
            })
        })
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to run import commit task: {e}")))??;

    // Commit via gateway
    let num_devices = prepared.devices.len();
    match state.gateway.create_devices(prepared.devices).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows: prepared.total_rows,
            valid: prepared.valid_count,
            invalid: prepared.total_rows.saturating_sub(prepared.valid_count),
            warn: prepared.warn_count,
            inserted: num_devices,
            errors: prepared.errors,
        })),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Group validated rows by device name and validate consistency within each group.
///
/// Returns a map from device_name to rows, and a vector of validation errors
/// if any device group has inconsistent device_type or device_driver_config fields.
fn group_rows_by_device(
    rows: Vec<ValidatedRow>,
) -> Result<HashMap<String, Vec<ValidatedRow>>, Vec<FieldError>> {
    let mut groups: HashMap<String, DeviceGroup> = HashMap::new();
    let mut errors: Vec<FieldError> = Vec::new();

    for row in rows {
        let device_name = row
            .values
            .get("device_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if device_name.is_empty() {
            errors.push(FieldError {
                row: row.row_index,
                field: "device_name".to_string(),
                code: ValidationCode::Required,
                message: "device_name is required".to_string(),
            });
            continue;
        }

        let current_device_type = row.values.get("device_type").and_then(|v| v.as_str());
        let current_device_config = row.values.get("device_driver_config");

        if let Some(group) = groups.get_mut(device_name) {
            // Validate device_type consistency
            let ref_device_type = group.ref_device_type.as_deref();
            if current_device_type != ref_device_type {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_type".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!(
                        "device_type mismatch: expected {:?}, got {:?} for device {}",
                        ref_device_type, current_device_type, device_name
                    ),
                });
            }

            // Validate device_driver_config consistency (treat missing as empty object)
            let config_mismatch = match current_device_config {
                Some(v) => v != &group.ref_device_config,
                None => !matches!(
                    group.ref_device_config,
                    serde_json::Value::Object(ref m) if m.is_empty()
                ),
            };
            if config_mismatch {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_driver_config".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!("device_driver_config mismatch for device {}", device_name),
                });
            }

            group.rows.push(row);
        } else {
            // First time seeing this device: snapshot reference values.
            let ref_device_type = current_device_type.map(|s| s.to_string());
            let ref_device_config = current_device_config
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            groups.insert(
                device_name.to_string(),
                DeviceGroup {
                    ref_device_type,
                    ref_device_config,
                    rows: vec![row],
                },
            );
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(groups
        .into_iter()
        .map(|(k, g)| (k, g.rows))
        .collect::<HashMap<_, _>>())
}

/// Validate device-level consistency constraints for `FlattenEntity::DevicePoints`.
///
/// # Why this exists
/// `DevicePoints` import represents a logical "device" repeated across multiple rows (one per point).
/// We must ensure that some device-level fields are consistent within the same `device_name`.
///
/// # What it validates
/// - `device_name` is required
/// - For the same `device_name`, all rows must have the same `device_type`
/// - For the same `device_name`, all rows must have the same `device_driver_config`
///
/// # Performance
/// This runs in a single pass over `rows` and does **not** clone the input rows.
fn validate_device_group_consistency(rows: &[ValidatedRow]) -> Vec<FieldError> {
    let mut refs: HashMap<String, DeviceRef> = HashMap::new();
    let mut errors: Vec<FieldError> = Vec::new();

    for row in rows {
        let device_name = row
            .values
            .get("device_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if device_name.is_empty() {
            errors.push(FieldError {
                row: row.row_index,
                field: "device_name".to_string(),
                code: ValidationCode::Required,
                message: "device_name is required".to_string(),
            });
            continue;
        }

        let current_device_type = row.values.get("device_type").and_then(|v| v.as_str());
        let current_device_config = row.values.get("device_driver_config");

        if let Some(existing) = refs.get(device_name) {
            // device_type consistency
            let ref_device_type = existing.device_type.as_deref();
            if current_device_type != ref_device_type {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_type".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!(
                        "device_type mismatch: expected {:?}, got {:?} for device {}",
                        ref_device_type, current_device_type, device_name
                    ),
                });
            }

            // device_driver_config consistency (treat missing as empty object)
            let config_mismatch = match current_device_config {
                Some(v) => v != &existing.device_driver_config,
                None => !matches!(
                    existing.device_driver_config,
                    serde_json::Value::Object(ref m) if m.is_empty()
                ),
            };
            if config_mismatch {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_driver_config".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!("device_driver_config mismatch for device {}", device_name),
                });
            }
        } else {
            // First time seeing this device_name: snapshot reference values once per device.
            let ref_device_type = current_device_type.map(|s| s.to_string());
            let ref_device_config = current_device_config
                .cloned()
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            refs.insert(
                device_name.to_string(),
                DeviceRef {
                    device_type: ref_device_type,
                    device_driver_config: ref_device_config,
                },
            );
        }
    }

    errors
}

/// Import device with points template preview
///
/// # Endpoint
/// `POST /api/channel/{id}/import-device-points-preview`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel read permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<ImportPreview>>`: Preview of imported devices and points on success
///   or appropriate error response
pub async fn import_device_points_preview(
    params: Path<PathId>,
    mut multipart: Multipart,
) -> WebResult<WebResponse<ImportPreview>> {
    let channel = match ChannelRepository::find_by_id(params.id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read first uploaded part fully into memory (Excel file)
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    // Load driver schemas
    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;

    // Excel parsing + row validation is CPU-heavy and uses blocking I/O internally (zip/XML).
    // Run it on a dedicated blocking thread to avoid starving Actix workers.
    let buf = accumulated_data.freeze();
    let driver_type = driver.driver_type;
    let preview = tokio::task::spawn_blocking(move || -> Result<ImportPreview, String> {
        // Read metadata and rows together, then validate meta
        let template = schemas.build_template(FlattenEntity::DevicePoints, "zh-CN");
        let (metadata, rows) = template
            .read_with_meta_from_reader(Cursor::new(buf))
            .map_err(|e| e.to_string())?;
        metadata
            .validate(&driver_type, FlattenEntity::DevicePoints)
            .map_err(|e| e.to_string())?;
        let template = schemas.build_template(FlattenEntity::DevicePoints, &metadata.locale);

        // Validate and normalize rows
        let total = rows.len();
        let (valids, errors, warn_count) =
            template.validate_and_normalize_rows(rows, &metadata.locale);

        // Validate device-level consistency without cloning all rows
        let group_errors = validate_device_group_consistency(&valids);

        // Combine validation errors and compute row-level statistics.
        // Note:
        // - `validate_and_normalize_rows` already filters out rows with field-level errors from `valids`.
        // - `validate_device_group_consistency` may introduce additional errors for otherwise field-valid rows.
        // - A single row can have multiple `FieldError`s; we must deduplicate by row index to avoid
        //   double-counting the same row as multiple invalid entries.
        let mut all_errors = errors;
        all_errors.extend(group_errors);

        let mut invalid_row_indices: HashSet<usize> = HashSet::with_capacity(all_errors.len());
        for err in &all_errors {
            invalid_row_indices.insert(err.row);
        }

        let invalid = invalid_row_indices.len();
        let valid = total.saturating_sub(invalid);

        Ok(ImportPreview {
            total_rows: total,
            valid,
            invalid,
            warn: warn_count,
            errors: all_errors.into_iter().take(50).collect(),
        })
    })
    .await
    .map_err(|e| WebError::InternalError(format!("Failed to run import preview task: {e}")))?
    .map_err(WebError::BadRequest)?;

    Ok(WebResponse::ok(preview))
}

/// Import device with points template and commit
///
/// # Endpoint
/// `POST /api/channel/{id}/import-device-points-commit`
///
/// # Authorization
/// Requires `system_admin` role
/// Channel write permission on the Channel resource type
///
/// # Returns
/// - `WebResult<WebResponse<CommitResult>>`: Result of import and commit on success
///   or appropriate error response
pub async fn import_device_points_commit(
    params: Path<PathId>,
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<CommitResult>> {
    let channel = match ChannelRepository::find_by_id(params.id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read first uploaded part fully into memory (Excel file)
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    // Load driver schemas
    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;

    // Excel parsing + validation + row-to-domain mapping is CPU-heavy and uses blocking I/O
    // internally (zip/XML). Run it on a dedicated blocking thread to avoid starving Actix workers.
    let buf = accumulated_data.freeze();
    let channel_id = channel.id;
    let driver_type = driver.driver_type.clone();
    let prepared =
        tokio::task::spawn_blocking(move || -> Result<PreparedDevicePointsCommit, WebError> {
            // Parse workbook once (meta + rows), then validate meta.
            let template = schemas.build_template(FlattenEntity::DevicePoints, "zh-CN");
            let (metadata, rows) = template
                .read_with_meta_from_reader(Cursor::new(buf))
                .map_err(|e| WebError::InternalError(e.to_string()))?;
            metadata
                .validate(&driver_type, FlattenEntity::DevicePoints)
                .map_err(|e| WebError::BadRequest(e.to_string()))?;

            let locale = metadata.locale.clone();
            let template = schemas.build_template(FlattenEntity::DevicePoints, &locale);

            // Validate and normalize
            let total_rows = rows.len();
            let (valids, mut errors, warn_count) =
                template.validate_and_normalize_rows(rows, &locale);

            // Group by device and validate device-level consistency in a single pass
            let grouped = group_rows_by_device(valids).map_err(|group_errors| {
                WebError::BadRequest(format!(
                    "Device grouping errors: {}",
                    group_errors
                        .iter()
                        .map(|e| format!("row {}: {}", e.row, e.message))
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
            })?;

            let mut devices: Vec<NewDevice> = Vec::new();
            let mut device_name_to_type: HashMap<String, String> = HashMap::new();
            let mut points_by_device: HashMap<String, Vec<ValidatedRow>> = HashMap::new();

            for (device_name, group_rows) in grouped.into_iter() {
                if group_rows.is_empty() {
                    continue;
                }

                // The first row contains device fields; points are in every row.
                let mut row_iter = group_rows.into_iter();
                let first_row = match row_iter.next() {
                    Some(r) => r,
                    None => continue,
                };

                let device_context = RowMappingContext {
                    entity_id: channel_id,
                    driver_type: driver_type.clone(),
                    locale: locale.clone(),
                };

                // Map device fields: move `device_driver_config` to `driver_config` for NewDevice mapping.
                // Note: cloning once per **device** is acceptable and avoids cloning per **point row**.
                let mut device_row_values = first_row.values.clone();
                if let Some(device_config) = device_row_values.remove("device_driver_config") {
                    device_row_values.insert("driver_config".to_string(), device_config);
                }
                let device_row = ValidatedRow {
                    row_index: first_row.row_index,
                    values: device_row_values,
                };

                let new_device = match NewDevice::from_validated_row(&device_row, &device_context) {
                    Ok(d) => d,
                    Err(e) => {
                        errors.push(FieldError {
                            row: first_row.row_index,
                            field: "device".to_string(),
                            code: ValidationCode::Unknown,
                            message: format!("Failed to create device: {}", e),
                        });
                        continue;
                    }
                };

                device_name_to_type.insert(device_name.clone(), new_device.device_type.clone());
                devices.push(new_device);

                // Build point rows without cloning: consume owned `ValidatedRow` values and remove keys in-place.
                let mut point_rows: Vec<ValidatedRow> = Vec::new();

                for row in std::iter::once(first_row).chain(row_iter) {
                    let mut point_row_values = row.values;
                    point_row_values.remove("device_name");
                    point_row_values.remove("device_type");
                    point_row_values.remove("device_driver_config");

                    // Ensure driver_config exists (for point-level driver config)
                    if !point_row_values.contains_key("driver_config") {
                        point_row_values.insert(
                            "driver_config".to_string(),
                            serde_json::Value::Object(serde_json::Map::new()),
                        );
                    }

                    point_rows.push(ValidatedRow {
                        row_index: row.row_index,
                        values: point_row_values,
                    });
                }

                points_by_device.insert(device_name, point_rows);
            }

            Ok(PreparedDevicePointsCommit {
                total_rows,
                warn_count,
                locale,
                devices,
                device_name_to_type,
                points_by_device,
                errors,
            })
        })
        .await
        .map_err(|e| WebError::InternalError(format!("Failed to run import commit task: {e}")))??;

    let PreparedDevicePointsCommit {
        total_rows,
        warn_count,
        locale,
        devices,
        device_name_to_type,
        points_by_device,
        errors,
    } = prepared;

    // Create devices first
    let num_devices = devices.len();
    match state.gateway.create_devices(devices).await {
        Ok(_) => {}
        Err(e) => {
            return Ok(WebResponse::error(&format!(
                "Failed to create devices: {}",
                e
            )));
        }
    }

    // Fetch created devices to get their IDs
    // Note: We query by channel_id and match by device_name and device_type to get the correct IDs
    let created_devices = DeviceRepository::find_by_channel_id(channel.id).await?;

    // Map device names to device IDs
    // Match by both device_name and device_type to handle potential duplicates
    let mut device_name_to_id: HashMap<String, i32> = HashMap::new();
    for created_device in created_devices {
        if let Some(expected_type) = device_name_to_type.get(&created_device.device_name) {
            if expected_type == &created_device.device_type {
                device_name_to_id.insert(created_device.device_name.clone(), created_device.id);
            }
        }
    }

    // Map point rows to NewPoint (CPU-heavy); do it on blocking pool.
    let driver_type_for_points = driver.driver_type.clone();
    let locale_for_points = locale.clone();
    let mut all_errors = errors;
    let (points_to_create, point_errors) = tokio::task::spawn_blocking(move || {
        let mut points: Vec<NewPoint> = Vec::new();
        let mut errors: Vec<FieldError> = Vec::new();

        for (device_name, point_rows) in points_by_device.into_iter() {
            let Some(&device_id) = device_name_to_id.get(&device_name) else {
                // Device not found after creation; report once per device to avoid flooding.
                if let Some(first) = point_rows.first() {
                    errors.push(FieldError {
                        row: first.row_index,
                        field: "device".to_string(),
                        code: ValidationCode::Unknown,
                        message: format!("Device not found after creation: {}", device_name),
                    });
                }
                continue;
            };

            let point_context = RowMappingContext {
                entity_id: device_id,
                driver_type: driver_type_for_points.clone(),
                locale: locale_for_points.clone(),
            };

            for point_row in point_rows {
                match NewPoint::from_validated_row(&point_row, &point_context) {
                    Ok(p) => points.push(p),
                    Err(e) => errors.push(FieldError {
                        row: point_row.row_index,
                        field: "point".to_string(),
                        code: ValidationCode::Unknown,
                        message: format!("Failed to create point: {}", e),
                    }),
                }
            }
        }

        (points, errors)
    })
    .await
    .map_err(|e| WebError::InternalError(format!("Failed to run point mapping task: {e}")))?;

    all_errors.extend(point_errors);
    let num_points = points_to_create.len();

    let invalid_row_count = all_errors
        .iter()
        .map(|e| e.row)
        .collect::<HashSet<_>>()
        .len();

    // Create points
    match state.gateway.create_points(points_to_create).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows,
            valid: total_rows.saturating_sub(invalid_row_count),
            invalid: invalid_row_count,
            warn: warn_count,
            inserted: num_devices + num_points,
            errors: all_errors.into_iter().take(50).collect(),
        })),
        Err(e) => Ok(WebResponse::error(&format!(
            "Failed to create points: {}",
            e
        ))),
    }
}
