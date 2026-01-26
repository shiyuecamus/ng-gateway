//! Realtime logs WebSocket endpoint.
//!
//! Path: `GET /api/ws/logs`
//!
//! This endpoint streams gateway logs in realtime (tail + follow) using the host-side LogHub.
//! See `document/realtime-logs-design.md` for the product-level design.

use super::common::ws_upgrade_and_spawn;
use crate::AppState;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::{Message as WsMessage, Session};
use futures::StreamExt;
use ng_gateway_common::log::{
    realtime::{
        hub::{LogEvent, LogHub, LogLevel},
        lease::{LogOverrideManager, LogOverrideScope},
    },
    runtime,
};
use ng_gateway_error::web::WebError;
use ng_gateway_models::settings::RealtimeLogs as RealtimeLogsSettings;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::{
    sync::broadcast::error::RecvError,
    time::{interval_at, Duration, Instant, MissedTickBehavior},
};
use tracing::{debug, error, instrument, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum LogsScope {
    Global,
    Channel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverrideRequest {
    level: LogLevel,
    #[serde(default)]
    ttl_ms: Option<u64>,
}

/// Client message model for `/api/ws/logs`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum LogsClientMessage {
    Subscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
        scope: LogsScope,
        #[serde(default, alias = "channelId")]
        channel_id: Option<i32>,
        #[serde(default)]
        tail: Option<usize>,
        #[serde(default, alias = "minLevel")]
        min_level: Option<LogLevel>,
        #[serde(default)]
        text: Option<String>,
        /// Optional server-side log level override (lease).
        #[serde(default, rename = "override")]
        r#override: Option<OverrideRequest>,
    },
    Unsubscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
        #[serde(default, alias = "subscriptionId")]
        subscription_id: Option<String>,
    },
    RenewOverride {
        #[serde(alias = "overrideId")]
        override_id: String,
        #[serde(default, alias = "ttlMs")]
        ttl_ms: Option<u64>,
    },
    ReleaseOverride {
        #[serde(alias = "overrideId")]
        override_id: String,
    },
    Ping {
        ts: i64,
    },
}

/// Server message model for `/api/ws/logs`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum LogsServerMessage {
    Subscribed {
        #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
        request_id: Option<String>,
        #[serde(rename = "subscriptionId")]
        subscription_id: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "overrideId")]
        override_id: Option<String>,
        #[serde(rename = "effectiveLevel")]
        effective_level: LogLevel,
    },
    LogBatch {
        #[serde(rename = "subscriptionId")]
        subscription_id: String,
        items: Vec<LogEvent>,
        dropped: u64,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
        request_id: Option<String>,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    Pong {
        ts: i64,
    },
}

#[derive(Debug, Clone)]
struct Subscription {
    id: String,
    scope: LogsScope,
    channel_id: Option<i32>,
    min_level: LogLevel,
    text: Option<String>,
    pending: VecDeque<LogEvent>,
    dropped: u64,
}

#[inline]
fn make_ticker(ms: u64) -> tokio::time::Interval {
    let mut t = interval_at(
        Instant::now() + Duration::from_millis(ms),
        Duration::from_millis(ms),
    );
    t.set_missed_tick_behavior(MissedTickBehavior::Skip);
    t
}

#[inline]
async fn send_json_text(session: &mut Session, msg: &LogsServerMessage) -> Result<(), ActixError> {
    let text = serde_json::to_string(msg).map_err(|e| {
        warn!(error=%e, "Failed to serialize logs WS message");
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

#[instrument(skip_all)]
pub async fn logs_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, ActixError> {
    ws_upgrade_and_spawn(req, body, state, logs_ws_loop).await
}

async fn logs_ws_loop(
    _state: Arc<AppState>,
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
) -> Result<(), ActixError> {
    let Some(rt) = runtime::global() else {
        let msg = LogsServerMessage::Error {
            request_id: None,
            message: "Realtime logs are not enabled on this gateway".into(),
            details: None,
        };
        let _ = send_json_text(&mut session, &msg).await;
        let _ = session.close(None).await;
        return Ok(());
    };

    let rt = Arc::clone(rt);
    let cfg = rt.settings();
    let Some(hub) = rt.hub() else {
        let msg = LogsServerMessage::Error {
            request_id: None,
            message: "Realtime logs are not enabled on this gateway".into(),
            details: None,
        };
        let _ = send_json_text(&mut session, &msg).await;
        let _ = session.close(None).await;
        return Ok(());
    };
    let overrides = rt.overrides();

    let mut hub_rx = hub.subscribe();
    let mut shutdown_rx = rt.subscribe_shutdown();

    let mut subs: HashMap<String, Subscription> = HashMap::new();
    let mut owned_overrides: Vec<Uuid> = Vec::new();

    let mut ticker = make_ticker(cfg.ws_tick_ms);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = flush_pending(&mut session, &cfg, &mut subs).await {
                    debug!("Logs WS flush error, closing: {}", e);
                    break;
                }
            }
            notice = shutdown_rx.recv() => {
                // Realtime logs disabled => close all sessions immediately (per desired semantics).
                let message = match notice {
                    Ok(n) => format!("Realtime logs disabled: {}", n.reason),
                    Err(_) => "Realtime logs disabled".to_string(),
                };
                let msg = LogsServerMessage::Error {
                    request_id: None,
                    message,
                    details: None,
                };
                let _ = send_json_text(&mut session, &msg).await;
                let _ = session.close(None).await;
                break;
            }
            recv = hub_rx.recv() => {
                match recv {
                    Ok(ev) => {
                        on_log_event(&cfg, &ev, &mut subs);
                    }
                    Err(RecvError::Lagged(n)) => {
                        // Attribute lag to all active subscriptions (connection-level receiver).
                        for sub in subs.values_mut() {
                            sub.dropped = sub.dropped.saturating_add(n);
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
            item = msg_stream.next() => {
                let Some(item) = item else { break; };
                let msg = match item {
                    Ok(m) => m,
                    Err(e) => {
                        error!("Logs WS stream error: {}", e);
                        break;
                    }
                };

                match msg {
                    WsMessage::Text(text) => {
                        if let Err(e) = handle_client_text(
                            &hub,
                            &overrides,
                            &cfg,
                            &mut session,
                            &text,
                            &mut subs,
                            &mut owned_overrides,
                        ).await {
                            debug!("Logs WS client msg error, closing: {}", e);
                            break;
                        }
                    }
                    WsMessage::Close(reason) => {
                        debug!("Logs WS closed by client: {:?}", reason);
                        break;
                    }
                    WsMessage::Ping(bytes) => {
                        if session.pong(&bytes).await.is_err() { break; }
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Binary(_) => {
                        let err = LogsServerMessage::Error {
                            request_id: None,
                            message: "Binary frames are not supported on /api/ws/logs".into(),
                            details: None,
                        };
                        if let Ok(t) = serde_json::to_string(&err) {
                            let _ = session.text(t).await;
                        }
                    }
                    WsMessage::Continuation(_) => {
                        warn!("Unexpected continuation frame on logs WS, closing");
                        let _ = session.close(None).await;
                        break;
                    }
                    WsMessage::Nop => {}
                }
            }
        }
    }

    // Best-effort cleanup: release overrides created by this connection.
    for id in owned_overrides {
        let _ = overrides.release_lease(id);
    }

    Ok(())
}

fn on_log_event(
    cfg: &RealtimeLogsSettings,
    ev: &LogEvent,
    subs: &mut HashMap<String, Subscription>,
) {
    for sub in subs.values_mut() {
        if !matches_scope(ev, sub.scope, sub.channel_id) {
            continue;
        }
        if ev.level > sub.min_level {
            continue;
        }
        if let Some(ref t) = sub.text {
            if !ev.message.contains(t) && !ev.target.contains(t) {
                continue;
            }
        }
        push_pending(cfg, sub, ev.clone());
    }
}

#[inline]
fn matches_scope(ev: &LogEvent, scope: LogsScope, channel_id: Option<i32>) -> bool {
    match scope {
        LogsScope::Global => true,
        LogsScope::Channel => channel_id.is_some() && ev.channel_id == channel_id,
    }
}

#[inline]
fn push_pending(cfg: &RealtimeLogsSettings, sub: &mut Subscription, ev: LogEvent) {
    sub.pending.push_back(ev);
    while sub.pending.len() > cfg.ws_pending_max {
        let _ = sub.pending.pop_front();
        sub.dropped = sub.dropped.saturating_add(1);
    }
}

async fn flush_pending(
    session: &mut Session,
    cfg: &RealtimeLogsSettings,
    subs: &mut HashMap<String, Subscription>,
) -> Result<(), ActixError> {
    if subs.is_empty() {
        return Ok(());
    }

    // Snapshot keys to avoid borrow conflicts.
    let keys: Vec<String> = subs.keys().cloned().collect();
    for id in keys {
        let Some(sub) = subs.get_mut(&id) else {
            continue;
        };
        if sub.pending.is_empty() && sub.dropped == 0 {
            continue;
        }

        let mut items: Vec<LogEvent> = Vec::new();
        while items.len() < cfg.ws_batch_max {
            let Some(ev) = sub.pending.pop_front() else {
                break;
            };
            items.push(ev);
        }
        let dropped = sub.dropped;
        sub.dropped = 0;

        let msg = LogsServerMessage::LogBatch {
            subscription_id: sub.id.clone(),
            items,
            dropped,
        };
        send_json_text(session, &msg).await?;
    }
    Ok(())
}

async fn handle_client_text(
    hub: &Arc<LogHub>,
    overrides: &Arc<LogOverrideManager>,
    cfg: &RealtimeLogsSettings,
    session: &mut Session,
    text: &str,
    subs: &mut HashMap<String, Subscription>,
    owned_overrides: &mut Vec<Uuid>,
) -> Result<(), ActixError> {
    match serde_json::from_str::<LogsClientMessage>(text) {
        Ok(LogsClientMessage::Subscribe {
            request_id,
            scope,
            channel_id,
            tail,
            min_level,
            text,
            r#override,
        }) => {
            if scope == LogsScope::Channel && channel_id.is_none() {
                let err = LogsServerMessage::Error {
                    request_id,
                    message: "channelId is required for scope=channel".into(),
                    details: None,
                };
                let _ = send_json_text(session, &err).await;
                return Ok(());
            }

            let tail = tail.unwrap_or(200).clamp(0, 10_000);
            let min_level = min_level.unwrap_or(LogLevel::Info);

            let subscription_id = Uuid::new_v4().to_string();
            let text_filter = text.clone();

            // Optionally create an override lease bound to this connection.
            let override_id = r#override.as_ref().map(|ovr| {
                let ttl = ovr
                    .ttl_ms
                    .unwrap_or(cfg.lease_default_ttl_ms)
                    .clamp(1, cfg.lease_max_ttl_ms);
                let scope = match scope {
                    LogsScope::Global => LogOverrideScope::Global,
                    LogsScope::Channel => LogOverrideScope::Channel(channel_id.unwrap_or_default()),
                };
                let id = overrides.create_lease(scope, ovr.level, ttl);
                owned_overrides.push(id);
                id
            });

            // Build ack.
            let effective = match scope {
                LogsScope::Global => overrides.effective_global_level(),
                LogsScope::Channel => {
                    overrides.effective_channel_level(channel_id.unwrap_or_default())
                }
            };
            let ack = LogsServerMessage::Subscribed {
                request_id,
                subscription_id: subscription_id.clone(),
                override_id: override_id.map(|id| id.to_string()),
                effective_level: effective,
            };
            send_json_text(session, &ack).await?;

            // Register subscription.
            subs.insert(
                subscription_id.clone(),
                Subscription {
                    id: subscription_id.clone(),
                    scope,
                    channel_id,
                    min_level,
                    text,
                    pending: VecDeque::new(),
                    dropped: 0,
                },
            );

            // Send tail (immediate batch) then follow.
            if tail > 0 {
                let tail_items = match scope {
                    LogsScope::Global => hub.tail_global(tail),
                    LogsScope::Channel => hub.tail_channel(channel_id.unwrap_or_default(), tail),
                };
                let items: Vec<LogEvent> = tail_items
                    .into_iter()
                    .filter(|ev| matches_scope(ev, scope, channel_id))
                    .filter(|ev| ev.level <= min_level)
                    .filter(|ev| {
                        if let Some(ref t) = text_filter {
                            ev.message.contains(t) || ev.target.contains(t)
                        } else {
                            true
                        }
                    })
                    .collect();

                let msg = LogsServerMessage::LogBatch {
                    subscription_id,
                    items,
                    dropped: 0,
                };
                send_json_text(session, &msg).await?;
            }
        }
        Ok(LogsClientMessage::Unsubscribe {
            request_id,
            subscription_id,
        }) => match subscription_id {
            None => {
                subs.clear();
                let ack = LogsServerMessage::Subscribed {
                    request_id,
                    subscription_id: "".into(),
                    override_id: None,
                    effective_level: overrides.effective_global_level(),
                };
                let _ = send_json_text(session, &ack).await;
            }
            Some(id) => {
                subs.remove(&id);
                let ack = LogsServerMessage::Subscribed {
                    request_id,
                    subscription_id: id,
                    override_id: None,
                    effective_level: overrides.effective_global_level(),
                };
                let _ = send_json_text(session, &ack).await;
            }
        },
        Ok(LogsClientMessage::RenewOverride {
            override_id,
            ttl_ms,
        }) => {
            let id = match Uuid::parse_str(&override_id) {
                Ok(id) => id,
                Err(_) => {
                    let err = LogsServerMessage::Error {
                        request_id: None,
                        message: "Invalid overrideId".into(),
                        details: Some(json!({ "overrideId": override_id })),
                    };
                    let _ = send_json_text(session, &err).await;
                    return Ok(());
                }
            };
            let ttl = ttl_ms
                .unwrap_or(cfg.lease_default_ttl_ms)
                .clamp(1, cfg.lease_max_ttl_ms);
            if !overrides.renew_lease(id, ttl) {
                let err = LogsServerMessage::Error {
                    request_id: None,
                    message: "Override not found".into(),
                    details: Some(json!({ "overrideId": override_id })),
                };
                let _ = send_json_text(session, &err).await;
            }
        }
        Ok(LogsClientMessage::ReleaseOverride { override_id }) => {
            let id = match Uuid::parse_str(&override_id) {
                Ok(id) => id,
                Err(_) => {
                    let err = LogsServerMessage::Error {
                        request_id: None,
                        message: "Invalid overrideId".into(),
                        details: Some(json!({ "overrideId": override_id })),
                    };
                    let _ = send_json_text(session, &err).await;
                    return Ok(());
                }
            };
            let _ = overrides.release_lease(id);
        }
        Ok(LogsClientMessage::Ping { ts }) => {
            let pong = LogsServerMessage::Pong { ts };
            send_json_text(session, &pong).await?;
        }
        Err(e) => {
            let err = LogsServerMessage::Error {
                request_id: None,
                message: "Invalid logs websocket message format".into(),
                details: Some(json!({ "reason": e.to_string() })),
            };
            let _ = send_json_text(session, &err).await;
        }
    }
    Ok(())
}
