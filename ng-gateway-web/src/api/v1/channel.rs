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
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        ChangeChannelStatus, ChannelInfo, ChannelPageParams, CommitResult, DeviceInfo,
        ImportPreview, NewChannel, NewDevice, NewPoint, PageResult, PathId, UpdateChannel,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{ChannelRepository, DeviceRepository, DriverRepository};
use ng_gateway_sdk::{
    DriverEntityTemplate, DriverSchemas, FieldError, FlattenEntity, FromValidatedRow,
    RowMappingContext, ValidatedRow, ValidationCode,
};
use sea_orm::IntoActiveModel;
use std::{
    collections::{BTreeMap, BTreeSet},
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
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/list"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Create,
                )?)
                .or(has_scope("channel:create")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Delete,
                )?)
                .or(has_scope("channel:delete")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-points-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Read,
                )?)
                .or(has_scope("channel:read")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-device-points-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Channel,
                    Operation::Write,
                )?)
                .or(has_scope("channel:write")?),
        )
        .await?;

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
    // Read metadata and rows together, then validate meta
    let template = schemas.build_template(FlattenEntity::Device, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(Cursor::new(accumulated_data.freeze()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Device)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Device, &metadata.locale);

    // Validate and normalize rows
    let total = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    Ok(WebResponse::ok(ImportPreview {
        total_rows: total,
        valid: valids.len(),
        invalid: total.saturating_sub(valids.len()),
        warn: warn_count,
        errors: errors.into_iter().take(50).collect(),
    }))
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
    // Read metadata to resolve locale and validate compatibility
    let buf = accumulated_data.freeze();
    let metadata = DriverEntityTemplate::read_template_metadata(Cursor::new(buf.clone()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Device)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Device, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(Cursor::new(buf))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Device)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Device, &metadata.locale);

    // Validate and normalize
    let total_rows = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    // Map to NewDevice via SDK mapping helper for thinner web layer
    let valid_count = valids.len();
    let devices: Vec<NewDevice> = template
        .map_to_domain(
            valids,
            RowMappingContext {
                entity_id: channel.id,
                driver_type: driver.driver_type.clone(),
                locale: metadata.locale.clone(),
            },
        )
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    // Commit via gateway
    let num_devices = devices.len();
    match state.gateway.create_devices(devices).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows,
            valid: valid_count,
            invalid: total_rows.saturating_sub(valid_count),
            warn: warn_count,
            inserted: num_devices,
            errors,
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
) -> Result<BTreeMap<String, Vec<ValidatedRow>>, Vec<FieldError>> {
    use serde_json::Value as Json;

    let mut groups: BTreeMap<String, Vec<ValidatedRow>> = BTreeMap::new();
    let mut errors = Vec::new();

    // First pass: group by device_name
    for row in rows {
        let device_name = row
            .values
            .get("device_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
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

        groups.entry(device_name).or_default().push(row);
    }

    // Second pass: validate consistency within each group
    for (device_name, group_rows) in groups.iter() {
        if group_rows.is_empty() {
            continue;
        }

        // Extract device_type and device_driver_config from first row as reference
        let first_row = &group_rows[0];
        let ref_device_type = first_row
            .values
            .get("device_type")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract device_driver_config as a JSON object
        let ref_device_config = first_row
            .values
            .get("device_driver_config")
            .cloned()
            .unwrap_or_else(|| Json::Object(serde_json::Map::new()));

        // Check consistency for all other rows in the group
        for row in group_rows.iter().skip(1) {
            let device_type = row
                .values
                .get("device_type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if device_type != ref_device_type {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_type".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!(
                        "device_type mismatch: expected {:?}, got {:?} for device {}",
                        ref_device_type, device_type, device_name
                    ),
                });
            }

            let device_config = row
                .values
                .get("device_driver_config")
                .cloned()
                .unwrap_or_else(|| Json::Object(serde_json::Map::new()));

            if device_config != ref_device_config {
                errors.push(FieldError {
                    row: row.row_index,
                    field: "device_driver_config".to_string(),
                    code: ValidationCode::TypeMismatch,
                    message: format!("device_driver_config mismatch for device {}", device_name),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(groups)
    } else {
        Err(errors)
    }
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

    // Read metadata and rows together, then validate meta
    let template = schemas.build_template(FlattenEntity::DevicePoints, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(Cursor::new(accumulated_data.freeze()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::DevicePoints)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::DevicePoints, &metadata.locale);

    // Validate and normalize rows
    let total = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    // Group by device and validate consistency
    let group_errors = match group_rows_by_device(valids.clone()) {
        Ok(_) => Vec::new(),
        Err(errs) => errs,
    };

    // Combine validation errors and compute row-level statistics.
    // Note:
    // - `validate_and_normalize_rows` already filters out rows with field-level errors from `valids`.
    // - `group_rows_by_device` may introduce additional errors for otherwise field-valid rows.
    // - A single row can have multiple `FieldError`s; we must deduplicate by row index to avoid
    //   double-counting the same row as multiple invalid entries.
    let mut all_errors = errors;
    all_errors.extend(group_errors);

    // Collect unique row indices that have any kind of error
    let mut invalid_row_indices: BTreeSet<usize> = BTreeSet::new();
    for err in &all_errors {
        invalid_row_indices.insert(err.row);
    }

    let invalid = invalid_row_indices.len();
    let valid = total.saturating_sub(invalid);

    Ok(WebResponse::ok(ImportPreview {
        total_rows: total,
        valid,
        invalid,
        warn: warn_count,
        errors: all_errors.into_iter().take(50).collect(),
    }))
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

    // Read metadata to resolve locale and validate compatibility
    let buf = accumulated_data.freeze();
    let metadata = DriverEntityTemplate::read_template_metadata(Cursor::new(buf.clone()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::DevicePoints)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::DevicePoints, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(Cursor::new(buf))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::DevicePoints)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::DevicePoints, &metadata.locale);

    // Validate and normalize
    let total_rows = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    // Group by device and validate consistency
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

    // Process each device group
    let mut all_devices: Vec<NewDevice> = Vec::new();
    let mut device_name_to_new_device: BTreeMap<String, NewDevice> = BTreeMap::new();
    let mut all_points: Vec<(String, ValidatedRow)> = Vec::new();
    let mut device_errors = errors;

    for (device_name, group_rows) in grouped {
        if group_rows.is_empty() {
            continue;
        }

        // Extract device info from first row
        let first_row = &group_rows[0];
        let device_context = RowMappingContext {
            entity_id: channel.id,
            driver_type: driver.driver_type.clone(),
            locale: metadata.locale.clone(),
        };

        // Map device fields: need to extract device_driver_config and rename to driver_config
        let mut device_row_values = first_row.values.clone();

        // Move device_driver_config to driver_config for NewDevice mapping
        if let Some(device_config) = device_row_values.remove("device_driver_config") {
            device_row_values.insert("driver_config".to_string(), device_config);
        }

        let device_row = ValidatedRow {
            row_index: first_row.row_index,
            values: device_row_values,
        };

        // Create NewDevice
        let new_device = match NewDevice::from_validated_row(&device_row, &device_context) {
            Ok(d) => d,
            Err(e) => {
                device_errors.push(FieldError {
                    row: first_row.row_index,
                    field: "device".to_string(),
                    code: ValidationCode::Unknown,
                    message: format!("Failed to create device: {}", e),
                });
                continue;
            }
        };

        all_devices.push(new_device.clone());
        device_name_to_new_device.insert(device_name.clone(), new_device);

        // Extract points from all rows in the group
        for row in group_rows {
            // Remove device-specific fields from point row
            let mut point_row_values = row.values.clone();
            point_row_values.remove("device_name");
            point_row_values.remove("device_type");
            point_row_values.remove("device_driver_config");

            // Ensure driver_config exists (for point driver config)
            if !point_row_values.contains_key("driver_config") {
                point_row_values.insert(
                    "driver_config".to_string(),
                    serde_json::Value::Object(serde_json::Map::new()),
                );
            }

            let point_row = ValidatedRow {
                row_index: row.row_index,
                values: point_row_values,
            };

            // We'll create points after devices are created
            all_points.push((device_name.clone(), point_row));
        }
    }

    // Create devices first
    let num_devices = all_devices.len();
    match state.gateway.create_devices(all_devices).await {
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
    let mut device_name_to_id: BTreeMap<String, i32> = BTreeMap::new();
    for created_device in created_devices {
        if let Some(new_device) = device_name_to_new_device.get(&created_device.device_name) {
            if new_device.device_type == created_device.device_type {
                device_name_to_id.insert(created_device.device_name.clone(), created_device.id);
            }
        }
    }

    // Create points with correct device_id
    let mut points_to_create = Vec::new();
    for (device_name, point_row) in all_points {
        if let Some(&device_id) = device_name_to_id.get(&device_name) {
            let point_context = RowMappingContext {
                entity_id: device_id,
                driver_type: driver.driver_type.clone(),
                locale: metadata.locale.clone(),
            };

            match NewPoint::from_validated_row(&point_row, &point_context) {
                Ok(p) => points_to_create.push(p),
                Err(e) => {
                    device_errors.push(FieldError {
                        row: point_row.row_index,
                        field: "point".to_string(),
                        code: ValidationCode::Unknown,
                        message: format!("Failed to create point: {}", e),
                    });
                }
            }
        }
    }

    let num_points = points_to_create.len();
    match state.gateway.create_points(points_to_create).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows,
            valid: total_rows.saturating_sub(device_errors.len()),
            invalid: device_errors.len(),
            warn: warn_count,
            inserted: num_devices + num_points,
            errors: device_errors.into_iter().take(50).collect(),
        })),
        Err(e) => Ok(WebResponse::error(&format!(
            "Failed to create points: {}",
            e
        ))),
    }
}
