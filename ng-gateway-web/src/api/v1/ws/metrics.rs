//! Aggregated gateway metrics WebSocket endpoint.
//!
//! Path: `GET /api/ws/metrics`
//!
//! This endpoint is designed for the gateway UI to render a **low-cardinality**
//! realtime overview (global/channel/app) without rebuilding Grafana.
//!
//! # Design notes
//! - The server performs **coalescing**: it sends at a fixed interval even if the
//!   underlying values change more frequently.
//! - Payloads are **aggregated snapshots**. Device/point-level details must use
//!   `/api/ws/monitor` or dedicated diagnostic APIs.

use std::sync::Arc;

use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::{Message as WsMessage, Session};
use futures::StreamExt;
use ng_gateway_core::gateway::NGGateway;
use ng_gateway_error::web::WebError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{interval, Duration, MissedTickBehavior};
use tracing::{debug, error, instrument, warn};

use crate::AppState;

/// Subscription scope for aggregated metrics stream.
///
/// # Compatibility
/// This enum is serialized/deserialized as lowercase strings:
/// - `global`
/// - `channel`
/// - `app`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum MetricsScope {
    Global,
    Channel,
    App,
}

impl MetricsScope {
    /// Whether this scope requires a numeric `id`.
    #[inline]
    fn requires_id(self) -> bool {
        matches!(self, MetricsScope::Channel | MetricsScope::App)
    }
}

/// Client message model for `/api/ws/metrics`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum MetricsClientMessage {
    /// Subscribe to a scope.
    ///
    /// Supported scopes:
    /// - `global`
    /// - `channel` (requires `id`)
    /// - `app` (requires `id`)
    Subscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
        scope: MetricsScope,
        #[serde(default)]
        id: Option<i32>,
        /// Optional client hint for update interval (milliseconds).
        ///
        /// The server may ignore this to protect itself.
        #[serde(default, alias = "intervalMs")]
        interval_ms: Option<u64>,
    },
    /// Unsubscribe from current scope but keep the connection open.
    Unsubscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
    },
    /// Heartbeat ping.
    Ping { ts: i64 },
}

/// Server message model for `/api/ws/metrics`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum MetricsServerMessage {
    /// Initial snapshot after `subscribe`.
    Snapshot {
        #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
        request_id: Option<String>,
        scope: MetricsScope,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<i32>,
        #[serde(rename = "ts")]
        ts: i64,
        data: Value,
    },
    /// Periodic update frame (coalesced).
    Update {
        scope: MetricsScope,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<i32>,
        #[serde(rename = "ts")]
        ts: i64,
        data: Value,
    },
    /// Acknowledge current subscription (always sent on subscribe/unsubscribe).
    Subscribed {
        #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
        request_id: Option<String>,
        scope: Option<MetricsScope>,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<i32>,
    },
    /// Error response.
    Error {
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    /// Heartbeat pong.
    Pong { ts: i64 },
}

#[derive(Debug, Clone)]
struct Subscription {
    scope: MetricsScope,
    id: Option<i32>,
    interval_ms: u64,
}

/// Handle WebSocket upgrades for `/api/ws/metrics`.
#[instrument(skip_all)]
pub async fn metrics_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, ActixError> {
    let (res, session, msg_stream) = actix_ws::handle(&req, body)?;
    let state: Arc<AppState> = state.get_ref().clone();

    actix_rt::spawn(async move {
        if let Err(e) = metrics_ws_loop(state, session, msg_stream).await {
            error!("Metrics WS loop error: {}", e);
        }
    });

    Ok(res)
}

async fn metrics_ws_loop(
    state: Arc<AppState>,
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
) -> Result<(), ActixError> {
    // We need concrete gateway methods for aggregated snapshots.
    let gateway = state
        .gateway
        .clone()
        .downcast_arc::<NGGateway>()
        .map_err(|_| {
            ActixError::from(WebError::InternalError(
                "Failed to downcast Gateway implementation".into(),
            ))
        })?;

    let mut subscription: Option<Subscription> = None;
    let mut ticker = interval(Duration::from_millis(1000));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Some(sub) = subscription.clone() else { continue; };
                // Enforce server-side bounds to protect from overly aggressive clients.
                let interval_ms = sub.interval_ms.clamp(200, 5_000);
                ticker = interval(Duration::from_millis(interval_ms));
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

                let ts = chrono::Utc::now().timestamp_millis();
                match build_scope_snapshot(&gateway, sub.scope, sub.id).await {
                    Ok(data) => {
                        let msg = MetricsServerMessage::Update {
                            scope: sub.scope,
                            id: sub.id,
                            ts,
                            data,
                        };
                        let text = match serde_json::to_string(&msg) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(error=%e, "Failed to serialize metrics update");
                                continue;
                            }
                        };
                        if let Err(e) = session.text(text).await {
                            debug!("Metrics WS send error, closing: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        let msg = MetricsServerMessage::Error {
                            code: "InternalError".into(),
                            message: "Failed to build metrics snapshot".into(),
                            details: Some(json!({ "reason": e.to_string() })),
                        };
                        let text = match serde_json::to_string(&msg) {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(error=%e, "Failed to serialize metrics error");
                                continue;
                            }
                        };
                        let _ = session.text(text).await;
                    }
                }
            }
            item = msg_stream.next() => {
                let Some(item) = item else { break; };
                let msg = match item {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Metrics WS stream error: {}", e);
                        break;
                    }
                };
                match msg {
                    WsMessage::Text(text) => {
                        match serde_json::from_str::<MetricsClientMessage>(&text) {
                            Ok(MetricsClientMessage::Subscribe { request_id, scope, id, interval_ms }) => {
                                let interval_ms = interval_ms.unwrap_or(1000);

                                // Validate scope + id pairing.
                                if scope.requires_id() && id.is_none() {
                                    let err = MetricsServerMessage::Error {
                                        code: "BadRequest".into(),
                                        message: "Invalid subscription scope".into(),
                                        details: Some(json!({ "scope": scope, "id": id })),
                                    };
                                    if let Ok(t) = serde_json::to_string(&err) {
                                        let _ = session.text(t).await;
                                    }
                                    continue;
                                }

                                // Build and send an immediate snapshot.
                                let ts = chrono::Utc::now().timestamp_millis();
                                match build_scope_snapshot(&gateway, scope, id).await {
                                    Ok(data) => {
                                        subscription = Some(Subscription {
                                            scope,
                                            id,
                                            interval_ms,
                                        });

                                        let ack = MetricsServerMessage::Subscribed {
                                            request_id: request_id.clone(),
                                            scope: Some(scope),
                                            id,
                                        };
                                        if let Ok(t) = serde_json::to_string(&ack) {
                                            if session.text(t).await.is_err() { break; }
                                        }

                                        let snap = MetricsServerMessage::Snapshot {
                                            request_id,
                                            scope,
                                            id,
                                            ts,
                                            data,
                                        };
                                        let text = match serde_json::to_string(&snap) {
                                            Ok(t) => t,
                                            Err(e) => {
                                                error!("Failed to serialize metrics snapshot: {}", e);
                                                continue;
                                            }
                                        };
                                        if session.text(text).await.is_err() { break; }
                                    }
                                    Err(e) => {
                                        let err = MetricsServerMessage::Error {
                                            code: "NotFound".into(),
                                            message: "Subscription target not found".into(),
                                            details: Some(json!({ "reason": e.to_string(), "scope": scope, "id": id })),
                                        };
                                        if let Ok(t) = serde_json::to_string(&err) {
                                            let _ = session.text(t).await;
                                        }
                                    }
                                }
                            }
                            Ok(MetricsClientMessage::Unsubscribe { request_id }) => {
                                subscription = None;
                                let ack = MetricsServerMessage::Subscribed { request_id, scope: None, id: None };
                                let text = match serde_json::to_string(&ack) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to serialize metrics unsubscribe ack: {}", e);
                                        continue;
                                    }
                                };
                                if session.text(text).await.is_err() { break; }
                            }
                            Ok(MetricsClientMessage::Ping { ts }) => {
                                let pong = MetricsServerMessage::Pong { ts };
                                let text = match serde_json::to_string(&pong) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to serialize metrics pong: {}", e);
                                        continue;
                                    }
                                };
                                if session.text(text).await.is_err() { break; }
                            }
                            Err(e) => {
                                let err = MetricsServerMessage::Error {
                                    code: "BadRequest".into(),
                                    message: "Invalid metrics websocket message format".into(),
                                    details: Some(json!({ "reason": e.to_string() })),
                                };
                                if let Ok(t) = serde_json::to_string(&err) {
                                    let _ = session.text(t).await;
                                }
                            }
                        }
                    }
                    WsMessage::Close(reason) => {
                        debug!("Metrics WS closed by client: {:?}", reason);
                        break;
                    }
                    WsMessage::Ping(bytes) => {
                        if session.pong(&bytes).await.is_err() { break; }
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Binary(_) => {
                        let err = MetricsServerMessage::Error {
                            code: "UnsupportedMediaType".into(),
                            message: "Binary frames are not supported on /api/ws/metrics".into(),
                            details: None,
                        };
                        if let Ok(t) = serde_json::to_string(&err) {
                            let _ = session.text(t).await;
                        }
                    }
                    WsMessage::Continuation(_) => {
                        warn!("Unexpected continuation frame on metrics WS, closing");
                        let _ = session.close(None).await;
                        break;
                    }
                    WsMessage::Nop => {}
                }
            }
        }
    }

    Ok(())
}

/// Build an aggregated snapshot for the requested scope.
async fn build_scope_snapshot(
    gateway: &Arc<NGGateway>,
    scope: MetricsScope,
    id: Option<i32>,
) -> Result<Value, ActixError> {
    match scope {
        MetricsScope::Global => {
            let status = gateway.get_status().await;
            let snapshot = status.get_snapshot();
            serde_json::to_value(&snapshot).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize gateway status snapshot: {e}"
                )))
            })
        }
        MetricsScope::App => {
            let app_id =
                id.ok_or_else(|| ActixError::from(WebError::BadRequest("missing app id".into())))?;
            let stats = gateway
                .get_northward_manager()
                .get_app_stats(app_id)
                .await
                .ok_or_else(|| ActixError::from(WebError::NotFound("app not found".into())))?;
            serde_json::to_value(&stats).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize northward app stats: {e}"
                )))
            })
        }
        MetricsScope::Channel => {
            let channel_id = id.ok_or_else(|| {
                ActixError::from(WebError::BadRequest("missing channel id".into()))
            })?;
            let stats = gateway
                .get_southward_manager()
                .get_channel_stats(channel_id)
                .ok_or_else(|| ActixError::from(WebError::NotFound("channel not found".into())))?;
            serde_json::to_value(&stats).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize southward channel stats: {e}"
                )))
            })
        }
    }
}
