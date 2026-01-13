//! Realtime southward device data monitor WebSocket endpoint.
//!
//! Path: `GET /api/ws/monitor`
//!
//! This endpoint provides a real-time view of device telemetry and attributes
//! for monitoring purposes. It:
//! - Authenticates using the standard `Authentication` middleware
//! - Accepts subscribe/unsubscribe messages from the client
//! - Sends an initial snapshot per device, followed by incremental updates
//!   driven by the northward routing pipeline.

use std::collections::HashMap;
use std::sync::Arc;

use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::{Message as WsMessage, Session};
use futures::StreamExt;
use ng_gateway_core::southward::NGSouthwardManager;
use ng_gateway_error::web::WebError;
use ng_gateway_models::RealtimeMonitorHub;
use ng_gateway_sdk::{NGValue, NGValueJsonOptions, NorthwardData};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::time::{interval, Duration, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};

use crate::AppState;

/// Incoming WebSocket messages from client.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum MonitorClientMessage {
    /// Subscribe to one or more devices on a channel.
    ///
    /// - If `device_id` is present, it is treated as a single-device subscription.
    /// - If `device_ids` is present, it replaces the subscription set with the
    ///   provided list of device ids.
    /// - If both are present, `device_ids` takes precedence.
    Subscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
        #[allow(unused)]
        #[serde(default, alias = "channelId")]
        channel_id: Option<i32>,
        #[serde(default, alias = "deviceId")]
        device_id: Option<i32>,
        #[serde(default, alias = "deviceIds")]
        device_ids: Option<Vec<i32>>,
    },
    /// Unsubscribe from all devices but keep the WebSocket connection open.
    Unsubscribe {
        #[serde(default, alias = "requestId")]
        request_id: Option<String>,
    },
    /// Heartbeat ping from client.
    Ping { ts: i64 },
}

/// Outgoing WebSocket messages to client.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum MonitorServerMessage {
    /// Acknowledge subscription update.
    Subscribed {
        #[serde(skip_serializing_if = "Option::is_none", rename = "requestId")]
        request_id: Option<String>,
        /// Currently subscribed device ids for this connection.
        #[serde(rename = "deviceIds")]
        device_ids: Vec<i32>,
    },
    /// Initial device snapshot.
    Snapshot {
        #[serde(rename = "device")]
        device: SnapshotDeviceInfo,
        #[serde(rename = "telemetry")]
        telemetry: HashMap<String, serde_json::Value>,
        #[serde(rename = "attributes")]
        attributes: SnapshotAttributes,
        #[serde(rename = "lastUpdate")]
        last_update: String,
    },
    /// Incremental update based on `NorthwardData`.
    Update {
        #[serde(rename = "deviceId")]
        device_id: i32,
        #[serde(rename = "dataType")]
        data_type: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "scope")]
        scope: Option<String>,
        #[serde(rename = "timestamp")]
        timestamp: String,
        #[serde(rename = "values")]
        values: serde_json::Value,
    },
    /// Error message.
    Error {
        #[serde(rename = "code")]
        code: String,
        #[serde(rename = "message")]
        message: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "details")]
        details: Option<serde_json::Value>,
    },
    /// Heartbeat pong.
    Pong {
        #[serde(rename = "ts")]
        ts: i64,
    },
}

/// Basic device info included in snapshot messages.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotDeviceInfo {
    id: i32,
    channel_id: i32,
    device_name: String,
}

/// Attribute snapshot payload.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotAttributes {
    #[serde(default)]
    client: HashMap<String, serde_json::Value>,
    #[serde(default)]
    shared: HashMap<String, serde_json::Value>,
    #[serde(default)]
    server: HashMap<String, serde_json::Value>,
}

/// Per-connection subscription manager.
///
/// Maintains a set of device-specific tasks, each consuming from the
/// `RealtimeMonitorHub` broadcast channels and forwarding transformed
/// updates to the WebSocket session.
struct DeviceSubscription {
    cancel: CancellationToken,
}

struct ConnectionSubscriptions {
    tasks: HashMap<i32, DeviceSubscription>,
}

impl ConnectionSubscriptions {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
        }
    }

    /// Cancel all running subscription tasks.
    fn clear(&mut self) {
        for (_, sub) in self.tasks.drain() {
            sub.cancel.cancel();
        }
    }

    /// Replace current subscriptions with a new set of device ids.
    ///
    /// For each device id, a new task is spawned that:
    /// - Listens to broadcast updates from the realtime hub
    /// - Transforms `NorthwardData` into `MonitorServerMessage::Update`
    /// - Sends JSON text messages to the client session
    fn replace_with(
        &mut self,
        device_ids: Vec<i32>,
        hub: Arc<dyn RealtimeMonitorHub>,
        session: Session,
    ) {
        self.clear();

        for device_id in device_ids {
            let mut rx = hub.subscribe(device_id);
            let mut session_clone = session.clone();
            let cancel = CancellationToken::new();
            let child_token = cancel.child_token();

            tokio::spawn(async move {
                // Batch updates to avoid flooding the UI with per-point frames.
                //
                // DLT645 (and other half-duplex protocols) can produce hundreds of point updates
                // per second. The monitor UI only needs a "near realtime" view, so we coalesce
                // updates within a small window and flush at a fixed interval.
                let mut ticker = interval(Duration::from_millis(200));
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

                let opts = NGValueJsonOptions::default();
                let mut telemetry_buf: Map<String, Value> = Map::new();
                let mut telemetry_ts: Option<String> = None;

                let mut attr_client_buf: Map<String, Value> = Map::new();
                let mut attr_shared_buf: Map<String, Value> = Map::new();
                let mut attr_server_buf: Map<String, Value> = Map::new();
                let mut attr_ts: Option<String> = None;

                loop {
                    tokio::select! {
                        _ = child_token.cancelled() => {
                            debug!("Monitor subscription cancelled for device {}", device_id);
                            break;
                        }
                        _ = ticker.tick() => {
                            // Flush telemetry
                            if !telemetry_buf.is_empty() {
                                let ts = telemetry_ts.take().unwrap_or_default();

                                let msg = MonitorServerMessage::Update {
                                    device_id,
                                    data_type: "telemetry".to_string(),
                                    scope: None,
                                    timestamp: ts,
                                    values: Value::Object(std::mem::take(&mut telemetry_buf)),
                                };

                                let text = match serde_json::to_string(&msg) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to serialize monitor telemetry batch: {}", e);
                                        continue;
                                    }
                                };

                                if let Err(e) = session_clone.text(text).await {
                                    debug!(
                                        "Monitor WS send error, stopping subscription task: {}",
                                        e
                                    );
                                    break;
                                }
                            }

                            // Flush attributes
                            if !attr_client_buf.is_empty() || !attr_shared_buf.is_empty() || !attr_server_buf.is_empty() {
                                let ts = attr_ts.take().unwrap_or_default();

                                let values = Value::Object(Map::from_iter([
                                    ("client".to_string(), Value::Object(std::mem::take(&mut attr_client_buf))),
                                    ("shared".to_string(), Value::Object(std::mem::take(&mut attr_shared_buf))),
                                    ("server".to_string(), Value::Object(std::mem::take(&mut attr_server_buf))),
                                ]));

                                let msg = MonitorServerMessage::Update {
                                    device_id,
                                    data_type: "attributes".to_string(),
                                    scope: None,
                                    timestamp: ts,
                                    values,
                                };

                                let text = match serde_json::to_string(&msg) {
                                    Ok(t) => t,
                                    Err(e) => {
                                        error!("Failed to serialize monitor attributes batch: {}", e);
                                        continue;
                                    }
                                };

                                if let Err(e) = session_clone.text(text).await {
                                    debug!(
                                        "Monitor WS send error, stopping subscription task: {}",
                                        e
                                    );
                                    break;
                                }
                            }
                        }
                        result = rx.recv() => {
                            match result {
                                Ok(data) => {
                                    match data.as_ref() {
                                        NorthwardData::Telemetry(t) => {
                                            telemetry_ts = Some(t.timestamp.to_rfc3339());
                                            for pv in t.values.iter() {
                                                telemetry_buf.insert(
                                                    pv.point_key.as_ref().to_string(),
                                                    pv.value.to_json_value(opts),
                                                );
                                            }
                                        }
                                        NorthwardData::Attributes(a) => {
                                            attr_ts = Some(a.timestamp.to_rfc3339());
                                            for pv in a.client_attributes.iter() {
                                                attr_client_buf.insert(
                                                    pv.point_key.as_ref().to_string(),
                                                    pv.value.to_json_value(opts),
                                                );
                                            }
                                            for pv in a.shared_attributes.iter() {
                                                attr_shared_buf.insert(
                                                    pv.point_key.as_ref().to_string(),
                                                    pv.value.to_json_value(opts),
                                                );
                                            }
                                            for pv in a.server_attributes.iter() {
                                                attr_server_buf.insert(
                                                    pv.point_key.as_ref().to_string(),
                                                    pv.value.to_json_value(opts),
                                                );
                                            }
                                        }
                                        _ => {
                                            // Other data types are not exposed via monitor channel.
                                        }
                                    };
                                }
                                Err(broadcast_err) => match broadcast_err {
                                    tokio::sync::broadcast::error::RecvError::Lagged(n) => {
                                        warn!(
                                            "Monitor subscription for device {} lagged by {} messages",
                                            device_id, n
                                        );
                                        continue;
                                    }
                                    tokio::sync::broadcast::error::RecvError::Closed => {
                                        debug!(
                                            "Monitor subscription channel for device {} closed",
                                            device_id
                                        );
                                        break;
                                    }
                                },
                            }
                        }
                    }
                }
            });

            self.tasks.insert(device_id, DeviceSubscription { cancel });
        }
    }
}

/// Handle WebSocket upgrades for `/api/ws/monitor`.
#[instrument(skip_all)]
pub async fn monitor_ws(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, ActixError> {
    let (res, session, msg_stream) = actix_ws::handle(&req, body)?;

    // Extract the inner `Arc<AppState>` from `web::Data<Arc<AppState>>`.
    let state: Arc<AppState> = state.get_ref().clone();

    actix_rt::spawn(async move {
        if let Err(e) = monitor_ws_loop(state, session, msg_stream).await {
            error!("Monitor WS loop error: {}", e);
        }
    });

    Ok(res)
}

/// Core monitor WebSocket loop.
async fn monitor_ws_loop(
    state: Arc<AppState>,
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
) -> Result<(), ActixError> {
    // Downcast southward manager and concrete gateway from trait object.
    let southward_dyn = state.gateway.southward_manager();
    let monitor_hub = state.gateway.realtime_monitor_hub();

    let southward = southward_dyn
        .downcast_arc::<NGSouthwardManager>()
        .map_err(|_| {
            ActixError::from(WebError::InternalError(
                "Failed to downcast SouthwardManager".into(),
            ))
        })?;

    let mut subscriptions = ConnectionSubscriptions::new();
    let mut subscribed_devices: Vec<i32> = Vec::new();

    // Auto-reconnect is handled on the client; server simply processes messages
    // until the stream ends or a fatal error occurs.
    while let Some(item) = msg_stream.next().await {
        let msg = match item {
            Ok(m) => m,
            Err(e) => {
                error!("Monitor WS stream error: {}", e);
                break;
            }
        };
        match msg {
            WsMessage::Text(text) => {
                match serde_json::from_str::<MonitorClientMessage>(&text) {
                    Ok(MonitorClientMessage::Subscribe {
                        request_id,
                        channel_id: _,
                        device_id,
                        device_ids,
                    }) => {
                        let mut next_devices: Vec<i32> =
                            device_ids.unwrap_or_else(|| device_id.into_iter().collect());

                        // Deduplicate and normalize
                        next_devices.sort_unstable();
                        next_devices.dedup();

                        // Replace subscriptions
                        subscriptions.replace_with(
                            next_devices.clone(),
                            Arc::clone(&monitor_hub),
                            session.clone(),
                        );
                        subscribed_devices = next_devices.clone();

                        // Send snapshot for each device (best-effort)
                        for dev_id in &subscribed_devices {
                            send_device_snapshot(*dev_id, Arc::clone(&southward), &mut session)
                                .await;
                        }

                        let ack = MonitorServerMessage::Subscribed {
                            request_id,
                            device_ids: subscribed_devices.clone(),
                        };
                        let text = match serde_json::to_string(&ack) {
                            Ok(t) => t,
                            Err(e) => {
                                error!("Failed to serialize subscribed ack: {}", e);
                                break;
                            }
                        };
                        if let Err(e) = session.text(text).await {
                            error!("Failed to send subscribed ack: {}", e);
                            break;
                        }
                    }
                    Ok(MonitorClientMessage::Unsubscribe { request_id }) => {
                        subscriptions.clear();
                        subscribed_devices.clear();
                        let ack = MonitorServerMessage::Subscribed {
                            request_id,
                            device_ids: vec![],
                        };
                        let text = match serde_json::to_string(&ack) {
                            Ok(t) => t,
                            Err(e) => {
                                error!("Failed to serialize unsubscribe ack: {}", e);
                                break;
                            }
                        };
                        if let Err(e) = session.text(text).await {
                            error!("Failed to send unsubscribe ack: {}", e);
                            break;
                        }
                    }
                    Ok(MonitorClientMessage::Ping { ts }) => {
                        let pong = MonitorServerMessage::Pong { ts };
                        let text = match serde_json::to_string(&pong) {
                            Ok(t) => t,
                            Err(e) => {
                                error!("Failed to serialize pong: {}", e);
                                break;
                            }
                        };
                        if let Err(e) = session.text(text).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Invalid monitor WS message: {}", e);
                        let err = MonitorServerMessage::Error {
                            code: "BadRequest".into(),
                            message: "Invalid monitor websocket message format".into(),
                            details: Some(json!({ "reason": e.to_string() })),
                        };
                        let text = match serde_json::to_string(&err) {
                            Ok(t) => t,
                            Err(e) => {
                                error!("Failed to serialize error message: {}", e);
                                break;
                            }
                        };
                        if let Err(e) = session.text(text).await {
                            error!("Failed to send error message: {}", e);
                            break;
                        }
                    }
                }
            }
            WsMessage::Close(reason) => {
                info!("Monitor WS closed by client: {:?}", reason);
                subscriptions.clear();
                break;
            }
            WsMessage::Ping(bytes) => {
                if let Err(e) = session.pong(&bytes).await {
                    error!("Failed to reply WS ping: {}", e);
                    break;
                }
            }
            WsMessage::Pong(_) => {
                // Client pong; no-op.
            }
            WsMessage::Binary(_) => {
                // Binary is not supported for this endpoint.
                let err = MonitorServerMessage::Error {
                    code: "UnsupportedMediaType".into(),
                    message: "Binary frames are not supported on /api/ws/monitor".into(),
                    details: None,
                };
                let text = match serde_json::to_string(&err) {
                    Ok(t) => t,
                    Err(e) => {
                        error!("Failed to serialize binary error message: {}", e);
                        break;
                    }
                };
                if let Err(e) = session.text(text).await {
                    error!("Failed to send binary error message: {}", e);
                    break;
                }
            }
            WsMessage::Continuation(_) => {
                // We don't expect continuation frames; close gracefully.
                warn!("Unexpected continuation frame on monitor WS, closing");
                session.close(None).await.ok();
                break;
            }
            WsMessage::Nop => {
                // keep-alive
            }
        }
    }

    // Ensure all subscription tasks are cancelled when loop ends.
    subscriptions.clear();

    Ok(())
}

/// Send a best-effort snapshot for a single device, if available.
async fn send_device_snapshot(
    device_id: i32,
    southward: Arc<NGSouthwardManager>,
    session: &mut Session,
) {
    // Snapshot is optional – device may not have reported any data yet.
    let snapshot = match southward.get_device_snapshot(device_id) {
        Some(s) => s,
        None => {
            debug!(
                "No snapshot available for device {}, skipping initial snapshot",
                device_id
            );
            return;
        }
    };

    // Try to derive channel id from runtime index; fall back to 0 if not found.
    let channel_id = southward.get_device_channel_id(device_id).unwrap_or(0);

    let msg = MonitorServerMessage::Snapshot {
        device: SnapshotDeviceInfo {
            id: snapshot.device_id,
            channel_id,
            device_name: snapshot.device_name.to_string(),
        },
        telemetry: point_map_to_json(&snapshot.point_key_by_id, &snapshot.telemetry),
        attributes: SnapshotAttributes {
            client: point_map_to_json(&snapshot.point_key_by_id, &snapshot.client_attributes),
            shared: point_map_to_json(&snapshot.point_key_by_id, &snapshot.shared_attributes),
            server: point_map_to_json(&snapshot.point_key_by_id, &snapshot.server_attributes),
        },
        last_update: snapshot.last_update.to_rfc3339(),
    };

    let text = match serde_json::to_string(&msg) {
        Ok(t) => t,
        Err(e) => {
            error!(
                "Failed to serialize snapshot for device {}: {}",
                device_id, e
            );
            return;
        }
    };

    if let Err(e) = session.text(text).await {
        error!(
            "Failed to send snapshot for device {} over monitor WS: {}",
            device_id, e
        );
    }
}

/// Convert an internal point-id keyed map into a JSON map keyed by `point_key` (fallback: `point_id`).
///
/// This is used by the monitor WebSocket snapshot payload which is intentionally JSON-friendly.
#[inline]
fn point_map_to_json(
    point_key_by_id: &HashMap<i32, Arc<str>>,
    input: &HashMap<i32, NGValue>,
) -> HashMap<String, serde_json::Value> {
    let mut out = HashMap::with_capacity(input.len().saturating_mul(2));
    let opts = NGValueJsonOptions::default();

    for (point_id, value) in input.iter() {
        let key = point_key_by_id
            .get(point_id)
            .map(|k| k.as_ref().to_string())
            .unwrap_or_else(|| point_id.to_string());
        out.insert(key, value.to_json_value(opts));
    }

    out
}
