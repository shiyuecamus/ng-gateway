use super::common::require_ai_engine;
use crate::AppState;
use actix_web::web;
use actix_web_validator::{Path, Query};
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    domain::prelude::{
        NewPipeline, PageResult, PathId, PipelineInfo, PipelinePageParams,
        PipelineValidationReport, UpdatePipeline,
    },
    web::WebResponse,
};
use std::sync::Arc;
use tracing::instrument;

/// `GET /api/ai/pipelines/list` — list all registered pipelines (no pagination).
#[instrument(name = "ai-list-pipelines", skip_all)]
pub async fn list_pipelines(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<PipelineInfo>>> {
    let engine = require_ai_engine(&state)?;
    let pipelines = engine
        .pipelines()
        .list_pipelines()
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(pipelines))
}

/// `GET /api/ai/pipelines/page` — paginated pipeline list with filters.
#[instrument(name = "ai-page-pipelines", skip_all)]
pub async fn page_pipelines(
    params: Query<PipelinePageParams>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<PipelineInfo>>> {
    let engine = require_ai_engine(&state)?;
    let result = engine
        .pipelines()
        .page_pipelines(params.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}

/// `GET /api/ai/pipelines/detail/{id}` — pipeline details by ID.
#[instrument(name = "ai-get-pipeline", skip_all, fields(id))]
pub async fn get_pipeline(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PipelineInfo>> {
    let engine = require_ai_engine(&state)?;
    let config = engine
        .pipelines()
        .get_pipeline(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Pipeline {}", path.id)))?;
    Ok(WebResponse::ok(config))
}

/// `POST /api/ai/pipelines/{id}/validate` — validate pipeline DAG constraints.
#[instrument(name = "ai-validate-pipeline", skip_all, fields(id))]
pub async fn validate_pipeline(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PipelineValidationReport>> {
    let engine = require_ai_engine(&state)?;
    let config = engine
        .pipelines()
        .get_pipeline(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Pipeline {}", path.id)))?;
    let report = config.validate_dag();
    Ok(WebResponse::ok(report))
}

/// `POST /api/ai/pipelines` — create a new pipeline.
#[instrument(name = "ai-create-pipeline", skip_all)]
pub async fn create_pipeline(
    body: web::Json<NewPipeline>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PipelineInfo>> {
    let engine = require_ai_engine(&state)?;
    let info = engine
        .pipelines()
        .create_pipeline(body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `PUT /api/ai/pipelines` — update an existing pipeline.
#[instrument(name = "ai-update-pipeline", skip_all)]
pub async fn update_pipeline(
    body: web::Json<UpdatePipeline>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PipelineInfo>> {
    let engine = require_ai_engine(&state)?;
    let info = engine
        .pipelines()
        .update_pipeline(body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `DELETE /api/ai/pipelines/{id}` — delete pipeline definition.
#[instrument(name = "ai-delete-pipeline", skip_all, fields(id))]
pub async fn delete_pipeline(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .pipelines()
        .delete_pipeline(path.id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}
