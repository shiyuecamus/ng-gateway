use super::common::require_ai_engine;
use crate::AppState;
use actix_multipart::Multipart;
use actix_web::web;
use actix_web_validator::{Path, Query};
use bytes::BytesMut;
use futures::StreamExt;
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    domain::prelude::{
        ModelInfo, ModelInstallRequest, ModelPageParams, ModelProbeInfo, PageResult, PathId,
        UpdateModel,
    },
    web::WebResponse,
};
use std::sync::Arc;
use tempfile::Builder;
use tokio::io::AsyncWriteExt;
use tracing::instrument;

/// `POST /api/ai/models/probe` — probe a model artifact and return metadata.
///
/// Accepts a multipart upload with a single `file` field. The file is saved
/// to a system temp directory, probed via runtime session to extract precise
/// tensor metadata, and immediately cleaned up. No model is registered.
#[instrument(name = "ai-probe-model", skip_all)]
pub async fn probe_model(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<ModelProbeInfo>> {
    let temp_file = save_model_to_system_temp(&mut multipart).await?;
    let engine = require_ai_engine(&state)?;
    let probe_info = engine
        .models()
        .probe_model(temp_file.path())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(probe_info))
}

/// `POST /api/ai/models/install` — install a model from uploaded file.
///
/// Accepts a multipart upload with `file` and optional `metadata` (JSON)
/// fields. The file is probed, metadata is merged with user overrides,
/// persisted to DB, and the file is atomically moved to the models directory.
#[instrument(name = "ai-install-model", skip_all)]
pub async fn install_model(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<ModelInfo>> {
    let (temp_file, user_meta) = read_model_install_multipart(&mut multipart).await?;
    let engine = require_ai_engine(&state)?;
    let info = engine
        .models()
        .install_model(temp_file.path(), user_meta)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `GET /api/ai/models/list` — list all registered models (no pagination).
#[instrument(name = "ai-list-models", skip_all)]
pub async fn list_models(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<ModelInfo>>> {
    let engine = require_ai_engine(&state)?;
    let models = engine
        .models()
        .list_models()
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(models))
}

/// `GET /api/ai/models/page` — paginated model list with filters.
#[instrument(name = "ai-page-models", skip_all)]
pub async fn page_models(
    params: Query<ModelPageParams>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<ModelInfo>>> {
    let engine = require_ai_engine(&state)?;
    let result = engine
        .models()
        .page_models(params.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}

/// `GET /api/ai/models/detail/{id}` — single model details.
#[instrument(name = "ai-get-model", skip_all, fields(id))]
pub async fn get_model(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<ModelInfo>> {
    let engine = require_ai_engine(&state)?;
    let model = engine
        .models()
        .get_model(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Model {}", path.id)))?;
    Ok(WebResponse::ok(model))
}

/// `PUT /api/ai/models/{id}` — update model metadata.
#[instrument(name = "ai-update-model", skip_all, fields(id))]
pub async fn update_model(
    path: Path<PathId>,
    body: web::Json<UpdateModel>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<ModelInfo>> {
    let engine = require_ai_engine(&state)?;
    let mut update = body.into_inner();
    update.id = path.id;
    let info = engine
        .models()
        .update_model(update)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `DELETE /api/ai/models/{id}` — uninstall a model.
#[instrument(name = "ai-uninstall-model", skip_all, fields(id))]
pub async fn uninstall_model(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .models()
        .uninstall_model(path.id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `POST /api/ai/models/{id}/load` — load model into inference backend.
#[instrument(name = "ai-load-model", skip_all, fields(id))]
pub async fn load_model(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .models()
        .load_model(path.id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `POST /api/ai/models/{id}/unload` — unload model from inference backend.
#[instrument(name = "ai-unload-model", skip_all, fields(id))]
pub async fn unload_model(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .models()
        .unload_model(path.id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// Save uploaded model file to a system temp directory.
async fn save_model_to_system_temp(
    multipart: &mut Multipart,
) -> Result<tempfile::NamedTempFile, WebError> {
    let mut file_bytes: Option<(bytes::Bytes, String)> = None;

    while let Some(field_result) = multipart.next().await {
        let mut field =
            field_result.map_err(|e| WebError::BadRequest(format!("multipart read error: {e}")))?;
        let field_name = field
            .content_disposition()
            .and_then(|cd| cd.get_name().map(String::from))
            .unwrap_or_default();

        if field_name != "file" {
            continue;
        }

        let filename = field
            .content_disposition()
            .and_then(|cd| cd.get_filename().map(String::from))
            .unwrap_or("model.onnx".to_string());

        let mut buf = BytesMut::new();
        while let Some(chunk) = field.next().await {
            let data =
                chunk.map_err(|e| WebError::BadRequest(format!("multipart chunk error: {e}")))?;
            buf.extend_from_slice(&data);
        }
        if buf.is_empty() {
            return Err(WebError::BadRequest("model file is empty".to_string()));
        }
        file_bytes = Some((buf.freeze(), filename));
    }

    let (data, filename) =
        file_bytes.ok_or(WebError::BadRequest("missing 'file' part".to_string()))?;

    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("onnx");

    let temp_file = Builder::new()
        .prefix("ng-model-")
        .suffix(&format!(".{ext}"))
        .tempfile()
        .map_err(|e| WebError::InternalError(format!("create temp file: {e}")))?;

    let mut async_file = tokio::fs::File::from_std(
        temp_file
            .as_file()
            .try_clone()
            .map_err(|e| WebError::InternalError(format!("clone temp file handle: {e}")))?,
    );
    async_file
        .write_all(&data)
        .await
        .map_err(|e| WebError::InternalError(format!("write temp file: {e}")))?;

    Ok(temp_file)
}

/// Read model install multipart: extracts file and optional metadata JSON.
async fn read_model_install_multipart(
    multipart: &mut Multipart,
) -> Result<(tempfile::NamedTempFile, ModelInstallRequest), WebError> {
    let mut file_bytes: Option<(bytes::Bytes, String)> = None;
    let mut user_meta: Option<ModelInstallRequest> = None;

    while let Some(field_result) = multipart.next().await {
        let mut field =
            field_result.map_err(|e| WebError::BadRequest(format!("multipart read error: {e}")))?;
        let field_name = field
            .content_disposition()
            .and_then(|cd| cd.get_name().map(String::from))
            .unwrap_or_default();

        let mut buf = BytesMut::new();
        while let Some(chunk) = field.next().await {
            let data =
                chunk.map_err(|e| WebError::BadRequest(format!("multipart chunk error: {e}")))?;
            buf.extend_from_slice(&data);
        }

        match field_name.as_str() {
            "file" => {
                if buf.is_empty() {
                    return Err(WebError::BadRequest("model file is empty".to_string()));
                }
                let filename = field
                    .content_disposition()
                    .and_then(|cd| cd.get_filename().map(String::from))
                    .unwrap_or("model.onnx".to_string());
                file_bytes = Some((buf.freeze(), filename));
            }
            "metadata" => {
                user_meta =
                    Some(serde_json::from_slice(&buf).map_err(|e| {
                        WebError::BadRequest(format!("invalid metadata JSON: {e}"))
                    })?);
            }
            _ => {}
        }
    }

    let (data, filename) =
        file_bytes.ok_or(WebError::BadRequest("missing 'file' part".to_string()))?;
    let meta = user_meta.unwrap_or(ModelInstallRequest {
        name: None,
        task: None,
        version: None,
        labels: None,
        default_preprocess: None,
        default_postprocess: None,
    });

    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("onnx");

    let temp_file = Builder::new()
        .prefix("ng-model-install-")
        .suffix(&format!(".{ext}"))
        .tempfile()
        .map_err(|e| WebError::InternalError(format!("create temp file: {e}")))?;

    let mut async_file = tokio::fs::File::from_std(
        temp_file
            .as_file()
            .try_clone()
            .map_err(|e| WebError::InternalError(format!("clone temp file handle: {e}")))?,
    );
    async_file
        .write_all(&data)
        .await
        .map_err(|e| WebError::InternalError(format!("write temp file: {e}")))?;

    Ok((temp_file, meta))
}
