//! AI engine REST API handlers.
//!
//! Provides endpoints for:
//! - **Phase 1**: Read-only queries for models, pipelines, engine status,
//!   channel snapshots, and built-in processor listings.
//! - **Phase 2**: Algorithm CRUD (upload, list, get, delete, test).

use crate::{
    rbac::{has_any_role, has_scope},
    AppState,
};
use actix_multipart::Multipart;
use actix_web::{
    http::{header, Method},
    web::{self, ServiceConfig},
    HttpResponse,
};
use bytes::BytesMut;
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    ai::{
        algorithm::{
            AlgorithmTestInput, AlgorithmTestResult, AlgorithmUploadMetadata, WasmAlgorithmInfo,
        },
        api::AiEngineApi,
        model::{ModelInfo, ModelUpdateRequest, ModelUploadMetadata},
        pipeline::{PipelineUpsertRequest, PipelineValidationReport},
        types::{EngineStatus, ProcessorInfo},
    },
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{AiPipelineSummary, PathId},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use std::sync::Arc;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/ai";

// ── Route configuration ───────────────────────────────────────────

/// Configure all AI API routes.
///
/// # Phase 1 Routes
/// - GET `/models` — list all models
/// - GET `/models/{model_id}` — model details
/// - POST `/models` — upload model
/// - PUT `/models/{model_id}` — update model config
/// - DELETE `/models/{model_id}` — delete model
/// - POST `/models/{model_id}/load` — hot load model
/// - POST `/models/{model_id}/unload` — hot unload model
/// - GET `/pipelines` — list all pipelines
/// - GET `/pipelines/{id}` — pipeline details
/// - POST `/pipelines/{id}/validate` — validate pipeline DAG constraints
/// - POST `/pipelines` — create pipeline binding
/// - PUT `/pipelines` — update pipeline binding
/// - DELETE `/pipelines/{id}` — delete pipeline binding
/// - GET `/engine/status` — engine global status
/// - GET `/channels/{id}/snapshot` — latest annotated JPEG snapshot
/// - GET `/processors/pre` — available preprocessors
/// - GET `/processors/post` — available postprocessors
///
/// # Phase 2 Routes
/// - GET `/algorithms` — list all algorithms
/// - GET `/algorithms/{id}` — algorithm details
/// - POST `/algorithms` — upload new WASM algorithm (multipart)
/// - DELETE `/algorithms/{id}` — delete algorithm
/// - POST `/algorithms/{id}/test` — test algorithm with mock data
pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg
        // Phase 1
        .route("/models", web::get().to(list_models))
        .route("/models/{model_id}", web::get().to(get_model))
        .route("/models", web::post().to(upload_model))
        .route("/models/{model_id}", web::put().to(update_model))
        .route("/models/{model_id}", web::delete().to(delete_model))
        .route("/models/{model_id}/load", web::post().to(load_model))
        .route("/models/{model_id}/unload", web::post().to(unload_model))
        .route("/pipelines", web::get().to(list_pipelines))
        .route("/pipelines/{id}", web::get().to(get_pipeline))
        .route(
            "/pipelines/{id}/validate",
            web::post().to(validate_pipeline),
        )
        .route("/pipelines", web::post().to(create_pipeline))
        .route("/pipelines", web::put().to(update_pipeline))
        .route("/pipelines/{id}", web::delete().to(delete_pipeline))
        .route("/engine/status", web::get().to(get_engine_status))
        .route("/channels/{id}/snapshot", web::get().to(get_snapshot))
        .route("/processors/pre", web::get().to(list_preprocessors))
        .route("/processors/post", web::get().to(list_postprocessors))
        // Phase 2 — Algorithm management
        .route("/algorithms", web::get().to(list_algorithms))
        .route("/algorithms/{id}", web::get().to(get_algorithm))
        .route("/algorithms", web::post().to(upload_algorithm))
        .route("/algorithms/{id}", web::delete().to(delete_algorithm))
        .route("/algorithms/{id}/test", web::post().to(test_algorithm));
}

/// Initialize RBAC rules for AI module.
#[inline]
#[instrument(name = "init-ai-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    let ai_read =
        || has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| Ok(r.or(has_scope("ai:read")?)));
    let ai_write =
        || has_any_role(&[SYSTEM_ADMIN_ROLE_CODE]).and_then(|r| Ok(r.or(has_scope("ai:write")?)));

    let rules = vec![
        // Phase 1 — read-only
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/models"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{model_id}}"),
            ai_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models"),
            ai_write()?,
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{model_id}}"),
            ai_write()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{model_id}}"),
            ai_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{model_id}}/load"),
            ai_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/models/{{model_id}}/unload"),
            ai_write()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/{{id}}"),
            ai_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/{{id}}/validate"),
            ai_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines"),
            ai_write()?,
        ),
        (
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines"),
            ai_write()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/pipelines/{{id}}"),
            ai_write()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/engine/status"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/channels/{{id}}/snapshot"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/processors/pre"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/processors/post"),
            ai_read()?,
        ),
        // Phase 2 — algorithm management
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms"),
            ai_read()?,
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/{{id}}"),
            ai_read()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms"),
            ai_write()?,
        ),
        (
            Method::DELETE,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/{{id}}"),
            ai_write()?,
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/algorithms/{{id}}/test"),
            ai_write()?,
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    info!("AI module RBAC rules initialized successfully");
    Ok(())
}

// ── Helper ────────────────────────────────────────────────────────

/// Extract the AI engine from gateway state or return 503.
#[inline]
fn require_ai_engine(state: &AppState) -> Result<Arc<dyn AiEngineApi>, WebError> {
    state
        .gateway
        .ai_engine()
        .ok_or(WebError::InternalError("AI engine is not enabled".into()))
}

// ── Phase 1 Handlers ──────────────────────────────────────────────

/// `GET /api/ai/models` — list all registered models (with processor info).
#[instrument(name = "ai-list-models", skip_all)]
pub async fn list_models(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<Arc<ModelInfo>>>> {
    let engine = require_ai_engine(&state)?;
    let models = engine.list_models().await.map_err(WebError::from)?;
    Ok(WebResponse::ok(models))
}

/// `GET /api/ai/models/{model_id}` — single model details.
#[instrument(name = "ai-get-model", skip_all, fields(model_id))]
pub async fn get_model(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Arc<ModelInfo>>> {
    let model_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    let model = engine
        .get_model(&model_id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Model '{model_id}'")))?;
    Ok(WebResponse::ok(model))
}

/// `POST /api/ai/models` — upload new ONNX model.
///
/// Expects `multipart/form-data`:
/// - `file`: ONNX bytes
/// - `metadata`: JSON [`ModelUploadMetadata`]
#[instrument(name = "ai-upload-model", skip_all)]
pub async fn upload_model(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Arc<ModelInfo>>> {
    let engine = require_ai_engine(&state)?;

    let mut model_bytes: Option<bytes::Bytes> = None;
    let mut metadata: Option<ModelUploadMetadata> = None;

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
                    return Err(WebError::BadRequest("ONNX file is empty".to_string()));
                }
                model_bytes = Some(buf.freeze());
            }
            "metadata" => {
                metadata =
                    Some(serde_json::from_slice(&buf).map_err(|e| {
                        WebError::BadRequest(format!("invalid metadata JSON: {e}"))
                    })?);
            }
            _ => {}
        }
    }

    let model_bytes = model_bytes.ok_or(WebError::BadRequest("missing 'file' part".to_string()))?;
    let metadata = metadata.ok_or(WebError::BadRequest("missing 'metadata' part".to_string()))?;

    let info = engine
        .upload_model(model_bytes, metadata)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `PUT /api/ai/models/{model_id}` — update model config.
#[instrument(name = "ai-update-model", skip_all, fields(model_id))]
pub async fn update_model(
    path: web::Path<String>,
    body: web::Json<ModelUpdateRequest>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Arc<ModelInfo>>> {
    let model_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    let info = engine
        .update_model(&model_id, body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `DELETE /api/ai/models/{model_id}` — delete model.
#[instrument(name = "ai-delete-model", skip_all, fields(model_id))]
pub async fn delete_model(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    engine
        .delete_model(&model_id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `POST /api/ai/models/{model_id}/load` — hot load model.
#[instrument(name = "ai-load-model", skip_all, fields(model_id))]
pub async fn load_model(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    engine.load_model(&model_id).await.map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `POST /api/ai/models/{model_id}/unload` — hot unload model.
#[instrument(name = "ai-unload-model", skip_all, fields(model_id))]
pub async fn unload_model(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let model_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    engine
        .unload_model(&model_id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `GET /api/ai/pipelines` — list all registered pipelines.
#[instrument(name = "ai-list-pipelines", skip_all)]
pub async fn list_pipelines(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<AiPipelineSummary>>> {
    let engine = require_ai_engine(&state)?;
    let pipelines = engine.list_pipelines().await.map_err(WebError::from)?;

    let summaries: Vec<AiPipelineSummary> = pipelines
        .into_iter()
        .map(|(channel_id, config)| AiPipelineSummary { channel_id, config })
        .collect();

    Ok(WebResponse::ok(summaries))
}

/// `GET /api/ai/pipelines/{id}` — pipeline details by channel ID.
#[instrument(name = "ai-get-pipeline", skip_all, fields(pipeline_id))]
pub async fn get_pipeline(
    path: web::Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AiPipelineSummary>> {
    let engine = require_ai_engine(&state)?;
    let config = engine
        .get_pipeline(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!(
            "Pipeline for channel {}",
            path.id
        )))?;

    Ok(WebResponse::ok(AiPipelineSummary {
        channel_id: path.id,
        config,
    }))
}

/// `POST /api/ai/pipelines/{id}/validate` — validate pipeline DAG constraints.
///
/// This endpoint validates the currently registered pipeline on the given channel.
/// It does not mutate pipeline state.
#[instrument(name = "ai-validate-pipeline", skip_all, fields(pipeline_id))]
pub async fn validate_pipeline(
    path: web::Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PipelineValidationReport>> {
    let engine = require_ai_engine(&state)?;
    let channel_id = path.id;

    let config = engine
        .get_pipeline(channel_id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!(
            "Pipeline for channel {channel_id}"
        )))?;

    let report = config.validate_dag();
    Ok(WebResponse::ok(report))
}

/// `POST /api/ai/pipelines` — create pipeline binding.
#[instrument(name = "ai-create-pipeline", skip_all)]
pub async fn create_pipeline(
    body: web::Json<PipelineUpsertRequest>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .upsert_pipeline(body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `PUT /api/ai/pipelines` — update pipeline binding.
#[instrument(name = "ai-update-pipeline", skip_all)]
pub async fn update_pipeline(
    body: web::Json<PipelineUpsertRequest>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .upsert_pipeline(body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `DELETE /api/ai/pipelines/{id}` — delete pipeline by channel ID.
#[instrument(name = "ai-delete-pipeline", skip_all, fields(channel_id))]
pub async fn delete_pipeline(
    path: web::Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let channel_id = path.id;
    let engine = require_ai_engine(&state)?;
    engine
        .delete_pipeline(channel_id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `GET /api/ai/engine/status` — AI engine global status.
#[instrument(name = "ai-engine-status", skip_all)]
pub async fn get_engine_status(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<EngineStatus>> {
    let engine = require_ai_engine(&state)?;
    let status = engine.get_engine_status().await.map_err(WebError::from)?;
    Ok(WebResponse::ok(status))
}

/// `GET /api/ai/channels/{id}/snapshot` — latest annotated JPEG frame.
///
/// Returns `image/jpeg` binary data directly (not JSON-wrapped).
/// - 404 if no active pipeline on this channel.
/// - 503 if pipeline exists but no frame has been analyzed yet.
#[instrument(name = "ai-snapshot", skip_all, fields(channel_id))]
pub async fn get_snapshot(
    path: web::Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<HttpResponse> {
    let engine = require_ai_engine(&state)?;
    let channel_id = path.id;

    let has_pipeline = engine
        .get_pipeline(channel_id)
        .await
        .map_err(WebError::from)?
        .is_some();
    if !has_pipeline {
        return Err(WebError::NotFound(format!(
            "No active AI pipeline on channel {channel_id}"
        )));
    }

    let result = engine
        .get_latest_result(channel_id)
        .await
        .map_err(WebError::from)?;

    match result.and_then(|r| r.annotated_frame) {
        Some(jpeg_bytes) => Ok(HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, "image/jpeg"))
            .insert_header((
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"channel_{channel_id}_snapshot.jpg\""),
            ))
            .body(jpeg_bytes)),
        None => Err(WebError::InternalError(format!(
            "No analysis result available yet for channel {channel_id}"
        ))),
    }
}

/// `GET /api/ai/processors/pre` — list available preprocessors.
#[instrument(name = "ai-list-preprocessors", skip_all)]
pub async fn list_preprocessors(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<ProcessorInfo>>> {
    let engine = require_ai_engine(&state)?;
    Ok(WebResponse::ok(engine.list_preprocessors()))
}

/// `GET /api/ai/processors/post` — list available postprocessors.
#[instrument(name = "ai-list-postprocessors", skip_all)]
pub async fn list_postprocessors(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<ProcessorInfo>>> {
    let engine = require_ai_engine(&state)?;
    Ok(WebResponse::ok(engine.list_postprocessors()))
}

// ── Phase 2 Handlers — Algorithm management ───────────────────────

/// `GET /api/ai/algorithms` — list all registered WASM algorithms.
#[instrument(name = "ai-list-algorithms", skip_all)]
pub async fn list_algorithms(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<Arc<WasmAlgorithmInfo>>>> {
    let engine = require_ai_engine(&state)?;
    let algorithms = engine.list_algorithms().await.map_err(WebError::from)?;
    Ok(WebResponse::ok(algorithms))
}

/// `GET /api/ai/algorithms/{id}` — single algorithm details.
#[instrument(name = "ai-get-algorithm", skip_all, fields(algorithm_id))]
pub async fn get_algorithm(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Arc<WasmAlgorithmInfo>>> {
    let algorithm_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    let algorithm = engine
        .get_algorithm(&algorithm_id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Algorithm '{algorithm_id}'")))?;
    Ok(WebResponse::ok(algorithm))
}

/// `POST /api/ai/algorithms` — upload new WASM algorithm.
///
/// Expects `multipart/form-data` with two parts:
/// - `file` — the `.wasm` binary (required)
/// - `metadata` — JSON metadata (required)
///
/// ```json
/// {
///   "name": "PPE Compliance Checker",
///   "description": "Checks if detected persons are wearing required PPE items",
///   "version": "1.0.0",
///   "module_type": "result_processor",
///   "config_schema": { ... }
/// }
/// ```
#[instrument(name = "ai-upload-algorithm", skip_all)]
pub async fn upload_algorithm(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Arc<WasmAlgorithmInfo>>> {
    let engine = require_ai_engine(&state)?;

    let mut wasm_bytes: Option<bytes::Bytes> = None;
    let mut metadata: Option<AlgorithmUploadMetadata> = None;

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
                    return Err(WebError::BadRequest("WASM file is empty".to_string()));
                }
                wasm_bytes = Some(buf.freeze());
            }
            "metadata" => {
                metadata =
                    Some(serde_json::from_slice(&buf).map_err(|e| {
                        WebError::BadRequest(format!("invalid metadata JSON: {e}"))
                    })?);
            }
            _ => {
                // Ignore unknown fields
            }
        }
    }

    let wasm_bytes = wasm_bytes.ok_or(WebError::BadRequest("missing 'file' part".to_string()))?;
    let metadata = metadata.ok_or(WebError::BadRequest("missing 'metadata' part".to_string()))?;

    let info = engine
        .upload_algorithm(wasm_bytes, metadata)
        .await
        .map_err(WebError::from)?;

    Ok(WebResponse::ok(info))
}

/// `DELETE /api/ai/algorithms/{id}` — delete an algorithm.
#[instrument(name = "ai-delete-algorithm", skip_all, fields(algorithm_id))]
pub async fn delete_algorithm(
    path: web::Path<String>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let algorithm_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    engine
        .delete_algorithm(&algorithm_id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `POST /api/ai/algorithms/{id}/test` — test algorithm with mock data.
///
/// Request body:
/// ```json
/// {
///   "detections": [...],
///   "classifications": [...],
///   "frame_width": 1920,
///   "frame_height": 1080,
///   "config": { ... }
/// }
/// ```
#[instrument(name = "ai-test-algorithm", skip_all, fields(algorithm_id))]
pub async fn test_algorithm(
    path: web::Path<String>,
    body: web::Json<AlgorithmTestInput>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlgorithmTestResult>> {
    let algorithm_id = path.into_inner();
    let engine = require_ai_engine(&state)?;
    let result = engine
        .test_algorithm(&algorithm_id, body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}
