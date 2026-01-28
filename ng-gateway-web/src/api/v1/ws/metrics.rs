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

use super::common::ws_upgrade_and_spawn;
use crate::AppState;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::{Message as WsMessage, Session};
use futures::StreamExt;
use ng_gateway_core::northward::NGNorthwardManager;
use ng_gateway_core::southward::NGSouthwardManager;
use ng_gateway_error::web::WebError;
use ng_gateway_models::core::metrics::DeviceStatsSnapshot;
use ng_gateway_models::Gateway;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::time::{interval_at, Duration, Instant, MissedTickBehavior};
use tracing::{debug, error, instrument, warn};

/// Subscription scope for aggregated metrics stream.
///
/// # Compatibility
/// This enum is serialized/deserialized as lowercase strings:
/// - `global`
/// - `channel`
/// - `app`
/// - `device`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum MetricsScope {
    Global,
    Channel,
    App,
    /// Southward channel per-device metrics stream (device stats list).
    Device,
}

impl MetricsScope {
    /// Whether this scope requires a numeric `id`.
    #[inline]
    fn requires_id(self) -> bool {
        matches!(
            self,
            MetricsScope::Channel | MetricsScope::App | MetricsScope::Device
        )
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
    /// Unsubscribe from subscriptions.
    ///
    /// - If `scope` is omitted: unsubscribe from **all** scopes.
    /// - If `scope` is provided: unsubscribe from that scope (and its `id` if applicable).
    Unsubscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
        #[serde(default)]
        scope: Option<MetricsScope>,
        #[serde(default)]
        id: Option<i32>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SubKey {
    scope: MetricsScope,
    id: Option<i32>,
}

#[derive(Debug, Clone)]
struct Subscription {
    scope: MetricsScope,
    id: Option<i32>,
    interval_ms: u64,
}

#[inline]
fn make_ticker(interval_ms: u64) -> tokio::time::Interval {
    let mut t = interval_at(
        Instant::now() + Duration::from_millis(interval_ms),
        Duration::from_millis(interval_ms),
    );
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

#[inline]
fn recompute_connection_interval_ms(subs: &HashMap<SubKey, Subscription>, default_ms: u64) -> u64 {
    subs.values()
        .map(|s| s.interval_ms)
        .min()
        .unwrap_or(default_ms)
}

/// Handle WebSocket upgrades for `/api/ws/metrics`.
#[instrument(skip_all)]
pub async fn metrics_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, ActixError> {
    ws_upgrade_and_spawn(req, body, state, metrics_ws_loop).await
}

#[derive(Clone)]
struct MetricsWsDeps {
    gateway: Arc<dyn Gateway>,
    southward: Arc<NGSouthwardManager>,
    northward: Arc<NGNorthwardManager>,
}

#[inline]
async fn send_json_text(
    session: &mut Session,
    msg: &MetricsServerMessage,
) -> Result<(), ActixError> {
    let text = serde_json::to_string(msg).map_err(|e| {
        warn!(error=%e, "Failed to serialize metrics WS message");
        ActixError::from(WebError::InternalError(
            "Failed to serialize websocket message".into(),
        ))
    })?;
    session.text(text).await.map_err(|_| {
        ActixError::from(WebError::InternalError(
            "Websocket connection closed".into(),
        ))
    })?;
    Ok(())
}

#[inline]
fn clamp_interval_ms(interval_ms: Option<u64>) -> u64 {
    interval_ms.unwrap_or(1000).clamp(200, 5_000)
}

#[inline]
fn update_connection_ticker(
    subscriptions: &HashMap<SubKey, Subscription>,
    current_interval_ms: &mut u64,
    ticker: &mut tokio::time::Interval,
) {
    let next_ms = recompute_connection_interval_ms(subscriptions, 1000);
    if next_ms != *current_interval_ms {
        *current_interval_ms = next_ms;
        *ticker = make_ticker(*current_interval_ms);
    }
}

async fn send_device_update(
    deps: &MetricsWsDeps,
    session: &mut Session,
    channel_id: i32,
    ts: i64,
) -> Result<(), ActixError> {
    let rows: Vec<DeviceStatsSnapshot> = deps.southward.get_channel_device_snapshots(channel_id);
    let payload = json!({ "rows": rows });

    let msg = MetricsServerMessage::Update {
        scope: MetricsScope::Device,
        id: Some(channel_id),
        ts,
        data: payload,
    };
    send_json_text(session, &msg).await
}

async fn handle_tick(
    deps: &MetricsWsDeps,
    session: &mut Session,
    subscriptions: &HashMap<SubKey, Subscription>,
) -> Result<(), ActixError> {
    if subscriptions.is_empty() {
        return Ok(());
    }

    let ts = chrono::Utc::now().timestamp_millis();
    let subs: Vec<Subscription> = subscriptions.values().cloned().collect();

    for sub in subs.iter() {
        if sub.scope == MetricsScope::Device {
            let Some(channel_id) = sub.id else { continue };
            // Device scope can be large; always send full rows to simplify client logic.
            send_device_update(deps, session, channel_id, ts).await?;
            continue;
        }

        match build_scope_snapshot(
            &deps.gateway,
            &deps.southward,
            &deps.northward,
            sub.scope,
            sub.id,
        )
        .await
        {
            Ok(data) => {
                let msg = MetricsServerMessage::Update {
                    scope: sub.scope,
                    id: sub.id,
                    ts,
                    data,
                };
                send_json_text(session, &msg).await?;
            }
            Err(e) => {
                let msg = MetricsServerMessage::Error {
                    code: "InternalError".into(),
                    message: "Failed to build metrics snapshot".into(),
                    details: Some(
                        json!({ "reason": e.to_string(), "scope": sub.scope, "id": sub.id }),
                    ),
                };
                // best-effort
                let _ = send_json_text(session, &msg).await;
            }
        }
    }

    Ok(())
}

async fn handle_client_text(
    deps: &MetricsWsDeps,
    session: &mut Session,
    text: &str,
    subscriptions: &mut HashMap<SubKey, Subscription>,
    current_interval_ms: &mut u64,
    ticker: &mut tokio::time::Interval,
) -> Result<(), ActixError> {
    match serde_json::from_str::<MetricsClientMessage>(text) {
        Ok(MetricsClientMessage::Subscribe {
            request_id,
            scope,
            id,
            interval_ms,
        }) => {
            let interval_ms = clamp_interval_ms(interval_ms);

            // Validate scope + id pairing.
            if scope.requires_id() && id.is_none() {
                let err = MetricsServerMessage::Error {
                    code: "BadRequest".into(),
                    message: "Invalid subscription scope".into(),
                    details: Some(json!({ "scope": scope, "id": id })),
                };
                let _ = send_json_text(session, &err).await;
                return Ok(());
            }

            // Build and send an immediate snapshot.
            let ts = chrono::Utc::now().timestamp_millis();
            match build_scope_snapshot(&deps.gateway, &deps.southward, &deps.northward, scope, id)
                .await
            {
                Ok(data) => {
                    // Track subscription in a set (multi-scope).
                    let key = SubKey { scope, id };
                    subscriptions.insert(
                        key,
                        Subscription {
                            scope,
                            id,
                            interval_ms,
                        },
                    );

                    update_connection_ticker(subscriptions, current_interval_ms, ticker);

                    let ack = MetricsServerMessage::Subscribed {
                        request_id: request_id.clone(),
                        scope: Some(scope),
                        id,
                    };
                    send_json_text(session, &ack).await?;

                    let snap = MetricsServerMessage::Snapshot {
                        request_id,
                        scope,
                        id,
                        ts,
                        data,
                    };
                    send_json_text(session, &snap).await?;
                }
                Err(e) => {
                    let err = MetricsServerMessage::Error {
                        code: "NotFound".into(),
                        message: "Subscription target not found".into(),
                        details: Some(json!({ "reason": e.to_string(), "scope": scope, "id": id })),
                    };
                    let _ = send_json_text(session, &err).await;
                }
            }
        }
        Ok(MetricsClientMessage::Unsubscribe {
            request_id,
            scope,
            id,
        }) => match scope {
            None => {
                subscriptions.clear();
                update_connection_ticker(subscriptions, current_interval_ms, ticker);
                let ack = MetricsServerMessage::Subscribed {
                    request_id,
                    scope: None,
                    id: None,
                };
                send_json_text(session, &ack).await?;
            }
            Some(scope) => {
                let key = SubKey { scope, id };
                subscriptions.remove(&key);
                update_connection_ticker(subscriptions, current_interval_ms, ticker);
                let ack = MetricsServerMessage::Subscribed {
                    request_id,
                    scope: Some(scope),
                    id,
                };
                send_json_text(session, &ack).await?;
            }
        },
        Ok(MetricsClientMessage::Ping { ts }) => {
            let pong = MetricsServerMessage::Pong { ts };
            send_json_text(session, &pong).await?;
        }
        Err(e) => {
            let err = MetricsServerMessage::Error {
                code: "BadRequest".into(),
                message: "Invalid metrics websocket message format".into(),
                details: Some(json!({ "reason": e.to_string() })),
            };
            let _ = send_json_text(session, &err).await;
        }
    }
    Ok(())
}

async fn metrics_ws_loop(
    state: Arc<AppState>,
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
) -> Result<(), ActixError> {
    // Prefer trait object access; only downcast *managers* when we need richer snapshot methods.
    let gateway = Arc::clone(&state.gateway);

    let southward_dyn = gateway.southward_manager();
    let northward_dyn = gateway.northward_manager();

    let southward = southward_dyn
        .downcast_arc::<NGSouthwardManager>()
        .map_err(|_| {
            ActixError::from(WebError::InternalError(
                "Failed to downcast SouthwardManager".into(),
            ))
        })?;
    let northward = northward_dyn
        .downcast_arc::<NGNorthwardManager>()
        .map_err(|_| {
            ActixError::from(WebError::InternalError(
                "Failed to downcast NorthwardManager".into(),
            ))
        })?;

    let deps = MetricsWsDeps {
        gateway: Arc::clone(&gateway),
        southward: Arc::clone(&southward),
        northward: Arc::clone(&northward),
    };

    // Multi-subscription: one WS connection can subscribe to multiple scopes simultaneously.
    let mut subscriptions: HashMap<SubKey, Subscription> = HashMap::new();

    // One connection-level coalescing ticker (interval = min(sub.interval_ms)).
    let mut current_interval_ms: u64 = 1000;
    let mut ticker = make_ticker(current_interval_ms);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = handle_tick(&deps, &mut session, &subscriptions).await {
                    debug!("Metrics WS tick loop error, closing: {}", e);
                    break;
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
                        if let Err(e) = handle_client_text(
                            &deps,
                            &mut session,
                            &text,
                            &mut subscriptions,
                            &mut current_interval_ms,
                            &mut ticker,
                        ).await {
                            debug!("Metrics WS client msg error, closing: {}", e);
                            break;
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
    gateway: &Arc<dyn Gateway>,
    southward: &Arc<NGSouthwardManager>,
    northward: &Arc<NGNorthwardManager>,
    scope: MetricsScope,
    id: Option<i32>,
) -> Result<Value, ActixError> {
    match scope {
        MetricsScope::Global => {
            let snapshot = gateway.get_snapshot().await;
            serde_json::to_value(&snapshot).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize gateway status snapshot: {e}"
                )))
            })
        }
        MetricsScope::App => {
            let app_id = id.ok_or(ActixError::from(WebError::BadRequest(
                "missing app id".into(),
            )))?;
            let stats = northward
                .get_app_snapshot(app_id)
                .await
                .ok_or(ActixError::from(WebError::NotFound("app not found".into())))?;
            serde_json::to_value(&stats).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize northward app stats: {e}"
                )))
            })
        }
        MetricsScope::Channel => {
            let channel_id = id.ok_or(ActixError::from(WebError::BadRequest(
                "missing channel id".into(),
            )))?;
            let stats = southward
                .get_channel_snapshot(channel_id)
                .ok_or(ActixError::from(WebError::NotFound(
                    "channel not found".into(),
                )))?;
            serde_json::to_value(&stats).map_err(|e| {
                ActixError::from(WebError::InternalError(format!(
                    "Failed to serialize southward channel stats: {e}"
                )))
            })
        }
        MetricsScope::Device => {
            let channel_id = id.ok_or(WebError::BadRequest("Missing channel id".into()))?;
            let rows: Vec<DeviceStatsSnapshot> = southward.get_channel_device_snapshots(channel_id);
            Ok(json!({ "rows": rows }))
        }
    }
}
