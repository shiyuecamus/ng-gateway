use super::common::require_ai_engine;
use crate::AppState;
use actix_web::{http::header, web, HttpResponse};
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    domain::prelude::{EngineStatus, PathId, ProcessorInfo},
    web::WebResponse,
};
use std::sync::Arc;
use tracing::instrument;

/// `GET /api/ai/engine/status` — AI engine global status.
#[instrument(name = "ai-engine-status", skip_all)]
pub async fn get_engine_status(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<EngineStatus>> {
    let engine = require_ai_engine(&state)?;
    let status = engine
        .runtime()
        .get_engine_status()
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(status))
}

/// `GET /api/ai/channels/{id}/snapshot` — latest annotated JPEG frame.
#[instrument(name = "ai-snapshot", skip_all, fields(channel_id))]
pub async fn get_snapshot(
    path: web::Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<HttpResponse> {
    let engine = require_ai_engine(&state)?;
    let channel_id = path.id;

    let has_pipeline = engine
        .pipelines()
        .get_channel_pipeline(channel_id)
        .is_some();
    if !has_pipeline {
        return Err(WebError::NotFound(format!(
            "No active AI pipeline on channel {channel_id}"
        )));
    }

    let result = engine
        .runtime()
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
    Ok(WebResponse::ok(engine.runtime().list_preprocessors()))
}

/// `GET /api/ai/processors/post` — list available postprocessors.
#[instrument(name = "ai-list-postprocessors", skip_all)]
pub async fn list_postprocessors(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<ProcessorInfo>>> {
    let engine = require_ai_engine(&state)?;
    Ok(WebResponse::ok(engine.runtime().list_postprocessors()))
}
