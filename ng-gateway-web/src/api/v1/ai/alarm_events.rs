//! Alarm event REST API handlers.

use crate::AppState;
use actix_web::web;
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::{
    domain::prelude::{
        AlarmEventInfo, AlarmEventPageParams, ChangeAlarmEventStatus, PageResult, PathId,
    },
    web::WebResponse,
};
use ng_gateway_repository::ai::alarm_event::AlarmEventRepository;
use std::sync::Arc;
use tracing::instrument;

/// `GET /api/ai/alarms/page` — paginated alarm event list.
#[instrument(name = "ai-alarm-events-page", skip_all)]
pub async fn page_alarm_events(
    params: web::Query<AlarmEventPageParams>,
    _state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<PageResult<AlarmEventInfo>>> {
    let result = AlarmEventRepository::page(&params)
        .await
        .map_err(WebError::from)?;
    Ok(WebResponse::ok(result))
}

/// `GET /api/ai/alarms/detail/{id}` — single alarm event detail.
#[instrument(name = "ai-alarm-event-detail", skip_all, fields(id))]
pub async fn get_alarm_event(
    path: web::Path<PathId>,
    _state: web::Data<Arc<AppState>>,
) -> WebResult<WebResponse<AlarmEventInfo>> {
    let model = AlarmEventRepository::get_by_id(path.id)
        .await
        .map_err(WebError::from)?
        .ok_or_else(|| WebError::NotFound(format!("alarm event {} not found", path.id)))?;

    Ok(WebResponse::ok(AlarmEventInfo {
        id: model.id,
        channel_id: model.channel_id,
        pipeline_id: model.pipeline_id,
        alarm_type: model.alarm_type,
        severity: model.severity,
        description: model.description,
        payload: model.payload,
        status: model.status,
        acked_at: model.acked_at,
        closed_at: model.closed_at,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }))
}

/// `PUT /api/ai/alarms/status` — acknowledge or close an alarm event.
#[instrument(name = "ai-alarm-event-status", skip_all)]
pub async fn change_alarm_event_status(
    body: web::Json<ChangeAlarmEventStatus>,
) -> WebResult<WebResponse<AlarmEventInfo>> {
    let model = AlarmEventRepository::update_status(body.id, body.status)
        .await
        .map_err(WebError::from)?;

    Ok(WebResponse::ok(AlarmEventInfo {
        id: model.id,
        channel_id: model.channel_id,
        pipeline_id: model.pipeline_id,
        alarm_type: model.alarm_type,
        severity: model.severity,
        description: model.description,
        payload: model.payload,
        status: model.status,
        acked_at: model.acked_at,
        closed_at: model.closed_at,
        created_at: model.created_at,
        updated_at: model.updated_at,
    }))
}
