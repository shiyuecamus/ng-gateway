use std::sync::Arc;

use actix_multipart::Multipart;
use actix_web::{http::Method, web};
use actix_web_validator::{Json, Path, Query};
use bytes::BytesMut;
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        BatchDeletePayload, ChangeDeviceStatus, ClearByChannelPayload, CommitResult, DeviceInfo,
        DevicePageParams, ImportPreview, NewAction, NewDevice, NewPoint, PageResult, PathId,
        UpdateDevice,
    },
    enums::common::{EntityType, Operation},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_repository::{ChannelRepository, DeviceRepository, DriverRepository};
use ng_gateway_sdk::{DriverEntityTemplate, DriverSchemas, FlattenEntity, RowMappingContext};
use sea_orm::IntoActiveModel;
use tracing::{info, instrument};

use crate::{
    rbac::{has_any_role, has_resource_operation, has_scope},
    AppState,
};

pub(super) const ROUTER_PREFIX: &str = "/device";

pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("", web::post().to(create))
        .route("", web::put().to(update))
        .route("/page", web::get().to(page))
        .route("/detail/{id}", web::get().to(get_by_id))
        .route("/change-status", web::put().to(change_status))
        .route("/batch-delete", web::post().to(batch_delete))
        .route("/clear", web::post().to(clear))
        .route("/{id}", web::delete().to(delete))
        .route(
            "{id}/import-point-preview",
            web::post().to(import_point_preview),
        )
        .route(
            "{id}/import-point-commit",
            web::post().to(import_point_commit),
        )
        .route(
            "{id}/import-action-preview",
            web::post().to(import_action_preview),
        )
        .route(
            "{id}/import-action-commit",
            web::post().to(import_action_commit),
        );
}

#[inline]
#[instrument(name = "init-device-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    // Page
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/page"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Device, Operation::Read)?)
                .or(has_scope("device:read")?),
        )
        .await?;

    // Detail
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/detail/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Device, Operation::Read)?)
                .or(has_scope("device:read")?),
        )
        .await?;

    // Create
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Create,
                )?)
                .or(has_scope("device:create")?),
        )
        .await?;

    // Update
    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Write,
                )?)
                .or(has_scope("device:write")?),
        )
        .await?;

    // Delete
    perm_checker
        .register(
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Delete,
                )?)
                .or(has_scope("device:delete")?),
        )
        .await?;

    // Batch delete
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/batch-delete"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Delete,
                )?)
                .or(has_scope("device:delete")?),
        )
        .await?;

    // Clear by channel
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/clear"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Delete,
                )?)
                .or(has_scope("device:delete")?),
        )
        .await?;

    // Change status
    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/change-status"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Write,
                )?)
                .or(has_scope("device:write")?),
        )
        .await?;

    // Import points preview
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-point-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::ReadPoint,
                )?)
                .or(has_scope("device:read")?),
        )
        .await?;

    // Import points commit
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-point-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::WritePoint,
                )?)
                .or(has_scope("device:write")?),
        )
        .await?;

    // Import actions preview
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-action-preview"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(EntityType::Device, Operation::Read)?)
                .or(has_scope("device:read")?),
        )
        .await?;

    // Import actions commit
    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/{{id}}/import-action-commit"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?
                .or(has_resource_operation(
                    EntityType::Device,
                    Operation::Write,
                )?)
                .or(has_scope("device:write")?),
        )
        .await?;

    info!("Device module RBAC rules initialized successfully");
    Ok(())
}

pub async fn page(
    params: Query<DevicePageParams>,
) -> WebResult<WebResponse<PageResult<DeviceInfo>>> {
    Ok(WebResponse::ok(
        DeviceRepository::page(params.into_inner()).await?,
    ))
}

pub async fn get_by_id(req: Path<PathId>) -> WebResult<WebResponse<DeviceInfo>> {
    Ok(WebResponse::ok(
        DeviceRepository::find_info_by_id(req.id)
            .await?
            .ok_or(WebError::NotFound(EntityType::Device.to_string()))?,
    ))
}

pub async fn create(
    device: Json<NewDevice>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model = device.into_inner();
    state
        .validator
        .validate(&model.clone().into_active_model(), Operation::Create)
        .await?;

    match state.gateway.create_devices(vec![model]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

pub async fn update(
    device: Json<UpdateDevice>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let payload = device.into_inner();
    state
        .validator
        .validate(&payload.clone().into_active_model(), Operation::Write)
        .await?;

    match state.gateway.update_devices(vec![payload]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

pub async fn delete(
    params: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    match state.gateway.delete_devices(vec![params.id]).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Batch delete devices by IDs
///
/// # Endpoint
/// `POST /api/device/batch-delete`
///
/// # Authorization
/// Requires `system_admin` role or device delete permission
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

    match state.gateway.delete_devices(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

pub async fn change_status(
    req: Json<ChangeDeviceStatus>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let req = req.into_inner();
    let device = match DeviceRepository::find_by_id(req.id).await? {
        Some(device) => device,
        None => return Err(WebError::NotFound(EntityType::Device.to_string())),
    };
    match state.gateway.change_device_status(device, req.status).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Clear all devices for a given channel
///
/// # Endpoint
/// `POST /api/device/clear`
///
/// # Authorization
/// Requires `system_admin` role or device delete permission
///
/// # Returns
/// - `WebResult<WebResponse<bool>>`: `true` on success, `false` on failure
pub async fn clear(
    payload: Json<ClearByChannelPayload>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let channel_id = payload.channel_id;

    // Fetch all devices for the channel and delete them via gateway to keep runtime in sync
    let devices = DeviceRepository::find_by_channel_id(channel_id).await?;
    let ids: Vec<i32> = devices.into_iter().map(|d| d.id).collect();

    if ids.is_empty() {
        return Ok(WebResponse::ok(true));
    }

    match state.gateway.delete_devices(ids).await {
        Ok(_) => Ok(WebResponse::ok(true)),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Import point template preview
///
/// # Endpoint
/// `POST /api/device/{id}/import-point-preview`
///
/// # Authorization
/// Requires `system_admin` role
/// Device read permission on the Device resource type
///
/// # Returns
/// - `WebResult<WebResponse<ImportPreview>>`: Preview of imported points on success
///   or appropriate error response
pub async fn import_point_preview(
    params: Path<PathId>,
    mut multipart: Multipart,
) -> WebResult<WebResponse<ImportPreview>> {
    let device = match DeviceRepository::find_by_id(params.id).await? {
        Some(device) => device,
        None => return Err(WebError::NotFound(EntityType::Device.to_string())),
    };
    let channel = match ChannelRepository::find_by_id(device.channel_id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read uploaded file
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

    // Read metadata and rows, validate meta
    let template = schemas.build_template(FlattenEntity::Point, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(std::io::Cursor::new(accumulated_data.freeze()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Point)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Point, &metadata.locale);

    // Validate and normalize rows
    let total = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    // Semantics:
    // - `valids` contains rows that passed all field-level validation.
    // - Any row that failed validation is excluded from `valids`, regardless of how many
    //   `FieldError`s it produced.
    // Therefore:
    // - `valid` should be the number of rows in `valids`.
    // - `invalid` should be `total - valid`, which is the number of rows that had at least one error.
    let valid_count = valids.len();

    Ok(WebResponse::ok(ImportPreview {
        total_rows: total,
        valid: valid_count,
        invalid: total.saturating_sub(valid_count),
        warn: warn_count,
        errors: errors.into_iter().take(50).collect(),
    }))
}

/// Import point template and commit
///
/// # Endpoint
/// `POST /api/device/{id}/import-point-commit`
///
/// # Authorization
/// Requires `system_admin` role
/// Device write permission on the Device resource type
///
/// # Returns
/// - `WebResult<WebResponse<CommitResult>>`: Result of import and commit on success
///   or appropriate error response
pub async fn import_point_commit(
    params: Path<PathId>,
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<CommitResult>> {
    let device = match DeviceRepository::find_by_id(params.id).await? {
        Some(device) => device,
        None => return Err(WebError::NotFound(EntityType::Device.to_string())),
    };
    let channel = match ChannelRepository::find_by_id(device.channel_id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read uploaded file
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

    // Resolve locale + validate
    let buf = accumulated_data.freeze();
    let metadata = DriverEntityTemplate::read_template_metadata(std::io::Cursor::new(buf.clone()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Point)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Point, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(std::io::Cursor::new(buf))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Point)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Point, &metadata.locale);

    let total_rows = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);
    let valid_count = valids.len();

    let points: Vec<NewPoint> = template
        .map_to_domain(
            valids,
            RowMappingContext {
                entity_id: device.id,
                driver_type: driver.driver_type.clone(),
                locale: metadata.locale.clone(),
            },
        )
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    let num_points = points.len();
    match state.gateway.create_points(points).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows,
            valid: valid_count,
            invalid: total_rows.saturating_sub(valid_count),
            warn: warn_count,
            inserted: num_points,
            errors,
        })),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}

/// Import action template preview
///
/// # Endpoint
/// `POST /api/device/{id}/import-action-preview`
///
/// # Authorization
/// Requires `system_admin` role
/// Device read permission on the Device resource type
///
/// # Returns
/// - `WebResult<WebResponse<ImportPreview>>`: Preview of imported actions on success
///   or appropriate error response
pub async fn import_action_preview(
    params: Path<PathId>,
    mut multipart: Multipart,
) -> WebResult<WebResponse<ImportPreview>> {
    let device = match DeviceRepository::find_by_id(params.id).await? {
        Some(device) => device,
        None => return Err(WebError::NotFound(EntityType::Device.to_string())),
    };
    let channel = match ChannelRepository::find_by_id(device.channel_id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read uploaded file
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;

    // Parse + validate meta
    let template = schemas.build_template(FlattenEntity::Action, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(std::io::Cursor::new(accumulated_data.freeze()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Action)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Action, &metadata.locale);

    let total = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);

    // Same semantics as point preview:
    // - `valids` contains only rows without field-level errors.
    // - `valid` is the number of such rows; `invalid` is the remaining rows.
    let valid_count = valids.len();

    Ok(WebResponse::ok(ImportPreview {
        total_rows: total,
        valid: valid_count,
        invalid: total.saturating_sub(valid_count),
        warn: warn_count,
        errors: errors.into_iter().take(50).collect(),
    }))
}

/// Import action template and commit (aggregate parameters by action name/command)
///
/// # Endpoint
/// `POST /api/device/{id}/import-action-commit`
///
/// # Authorization
/// Requires `system_admin` role
/// Device write permission on the Device resource type
///
/// # Returns
/// - `WebResult<WebResponse<CommitResult>>`: Result of import and commit on success
///   or appropriate error response
pub async fn import_action_commit(
    params: Path<PathId>,
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<CommitResult>> {
    use std::collections::BTreeMap;

    let device = match DeviceRepository::find_by_id(params.id).await? {
        Some(device) => device,
        None => return Err(WebError::NotFound(EntityType::Device.to_string())),
    };
    let channel = match ChannelRepository::find_by_id(device.channel_id).await? {
        Some(channel) => channel,
        None => return Err(WebError::NotFound(EntityType::Channel.to_string())),
    };

    // Read uploaded file
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;
    let mut accumulated_data = BytesMut::new();
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        accumulated_data.extend_from_slice(&data);
    }

    let driver = DriverRepository::find_by_id(channel.driver_id)
        .await?
        .ok_or(WebError::NotFound(EntityType::Driver.to_string()))?;
    let schemas: DriverSchemas = serde_json::from_value(driver.metadata)
        .map_err(|e| WebError::InternalError(format!("Invalid driver schemas: {e}")))?;

    let buf = accumulated_data.freeze();
    let metadata = DriverEntityTemplate::read_template_metadata(std::io::Cursor::new(buf.clone()))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Action)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Action, "zh-CN");
    let (metadata, rows) = template
        .read_with_meta_from_reader(std::io::Cursor::new(buf))
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    metadata
        .validate(&driver.driver_type, FlattenEntity::Action)
        .map_err(|e| WebError::BadRequest(e.to_string()))?;
    let template = schemas.build_template(FlattenEntity::Action, &metadata.locale);

    let total_rows = rows.len();
    let (valids, errors, warn_count) = template.validate_and_normalize_rows(rows, &metadata.locale);
    let valid_count = valids.len();

    // Map each row to NewAction (single parameter)
    let per_row_actions: Vec<NewAction> = template
        .map_to_domain(
            valids,
            RowMappingContext {
                entity_id: device.id,
                driver_type: driver.driver_type.clone(),
                locale: metadata.locale.clone(),
            },
        )
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    // Aggregate parameters by (name, command)
    let mut grouped: BTreeMap<(String, String), NewAction> = BTreeMap::new();
    for a in per_row_actions.into_iter() {
        let key = (a.name.clone(), a.command.clone());
        if let Some(existing) = grouped.get_mut(&key) {
            existing.inputs.0.extend(a.inputs.0.into_iter());
        } else {
            grouped.insert(key, a);
        }
    }
    let actions: Vec<NewAction> = grouped.into_values().collect();
    let num_actions = actions.len();

    match state.gateway.create_actions(actions).await {
        Ok(_) => Ok(WebResponse::ok(CommitResult {
            total_rows,
            valid: valid_count,
            invalid: total_rows.saturating_sub(valid_count),
            warn: warn_count,
            inserted: num_actions,
            errors,
        })),
        Err(e) => Ok(WebResponse::error(&e.to_string())),
    }
}
