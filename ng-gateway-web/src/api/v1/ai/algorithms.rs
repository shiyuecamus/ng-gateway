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
        AlgorithmInfo, AlgorithmPageParams, AlgorithmProbeInfo, AlgorithmTestInput,
        AlgorithmTestResult, PageResult, PathId,
    },
    web::WebResponse,
};
use std::sync::Arc;
use tracing::instrument;

/// `POST /api/ai/algorithms/probe` — probe uploaded WASM artifact.
#[instrument(name = "ai-probe-algorithm", skip_all)]
pub async fn probe_algorithm(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlgorithmProbeInfo>> {
    let engine = require_ai_engine(&state)?;
    let wasm_bytes = read_wasm_bytes_from_multipart(&mut multipart).await?;
    let info = engine
        .algorithms()
        .probe_algorithm(wasm_bytes)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `POST /api/ai/algorithms/install` — install uploaded WASM algorithm.
#[instrument(name = "ai-install-algorithm", skip_all)]
pub async fn install_algorithm(
    mut multipart: Multipart,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlgorithmInfo>> {
    let engine = require_ai_engine(&state)?;
    let wasm_bytes = read_wasm_bytes_from_multipart(&mut multipart).await?;
    let info = engine
        .algorithms()
        .install_algorithm(wasm_bytes)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(info))
}

/// `DELETE /api/ai/algorithms/{id}` — uninstall an algorithm.
#[instrument(name = "ai-uninstall-algorithm", skip_all, fields(id))]
pub async fn uninstall_algorithm(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<bool>> {
    let engine = require_ai_engine(&state)?;
    engine
        .algorithms()
        .uninstall_algorithm(path.id)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(true))
}

/// `GET /api/ai/algorithms/list` — list all registered algorithms (no pagination).
#[instrument(name = "ai-list-algorithms", skip_all)]
pub async fn list_algorithms(
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<Vec<AlgorithmInfo>>> {
    let engine = require_ai_engine(&state)?;
    let algorithms = engine
        .algorithms()
        .list_algorithms()
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(algorithms))
}

/// `GET /api/ai/algorithms/page` — paginated algorithm list with filters.
#[instrument(name = "ai-page-algorithms", skip_all)]
pub async fn page_algorithms(
    params: Query<AlgorithmPageParams>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<AlgorithmInfo>>> {
    let engine = require_ai_engine(&state)?;
    let result = engine
        .algorithms()
        .page_algorithms(params.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}

/// `GET /api/ai/algorithms/detail/{id}` — single algorithm details.
#[instrument(name = "ai-get-algorithm", skip_all, fields(id))]
pub async fn get_algorithm(
    path: Path<PathId>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlgorithmInfo>> {
    let engine = require_ai_engine(&state)?;
    let algorithm = engine
        .algorithms()
        .get_algorithm(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or(WebError::NotFound(format!("Algorithm {}", path.id)))?;
    Ok(WebResponse::ok(algorithm))
}

/// `POST /api/ai/algorithms/{id}/test` — test algorithm with mock data.
#[instrument(name = "ai-test-algorithm", skip_all, fields(id))]
pub async fn test_algorithm(
    path: Path<PathId>,
    body: web::Json<AlgorithmTestInput>,
    state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlgorithmTestResult>> {
    let engine = require_ai_engine(&state)?;
    let result = engine
        .algorithms()
        .test_algorithm(path.id, body.into_inner())
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}

/// Read one WASM file from multipart payload.
async fn read_wasm_bytes_from_multipart(
    multipart: &mut Multipart,
) -> Result<bytes::Bytes, WebError> {
    let mut wasm_bytes: Option<bytes::Bytes> = None;
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

        if field_name.is_empty() || field_name == "file" {
            if buf.is_empty() {
                return Err(WebError::BadRequest("WASM file is empty".to_string()));
            }
            wasm_bytes = Some(buf.freeze());
        }
    }

    wasm_bytes.ok_or(WebError::BadRequest("missing 'file' part".to_string()))
}
