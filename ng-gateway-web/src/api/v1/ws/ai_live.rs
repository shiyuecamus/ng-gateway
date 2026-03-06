//! WebRTC signaling WebSocket endpoint for AI live preview.
//!
//! Path: `GET /api/ws/ai/channels/{channel_id}/live`
//!
//! Handles the SDP offer/answer exchange and ICE candidate trickling
//! required to establish a WebRTC connection between the browser and
//! the gateway's GStreamer-based encode pipeline.
//!
//! Server-generated ICE candidates are pushed to the client via a
//! broadcast subscription; the client must add them to its RTCPeerConnection.

use crate::AppState;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::{Message as WsMessage, Session};
use futures::{stream, StreamExt};
use ng_gateway_models::domain::prelude::WebRtcSignaling;
use std::sync::Arc;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tracing::{debug, error, info, warn};

/// Path parameter for the channel ID.
#[derive(serde::Deserialize)]
pub struct LivePath {
    channel_id: i32,
}

/// `GET /api/ws/ai/channels/{channel_id}/live`
///
/// Upgrades the HTTP connection to a WebSocket for WebRTC signaling.
pub async fn ai_live_ws(
    req: HttpRequest,
    body: web::Payload,
    path: web::Path<LivePath>,
    state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, ActixError> {
    let channel_id = path.channel_id;
    let state_arc: Arc<AppState> = state.get_ref().clone();

    let (res, session, msg_stream) = actix_ws::handle(&req, body)?;

    actix_rt::spawn(async move {
        if let Err(e) = ai_live_ws_loop(state_arc, session, msg_stream, channel_id).await {
            error!(channel_id, error = %e, "AI live WebSocket loop error");
        }
    });

    Ok(res)
}

/// Core WebSocket loop for WebRTC signaling.
async fn ai_live_ws_loop(
    state: Arc<AppState>,
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
    channel_id: i32,
) -> Result<(), ActixError> {
    let engine = match state.gateway.ai_engine() {
        Some(e) => e,
        None => {
            let err = WebRtcSignaling::Error {
                message: "AI engine is not enabled".to_string(),
            };
            let _ = session
                .text(serde_json::to_string(&err).unwrap_or_default())
                .await;
            let _ = session.close(None).await;
            return Ok(());
        }
    };

    if !engine.runtime().is_webrtc_enabled() {
        let err = WebRtcSignaling::Error {
            message: "WebRTC live preview is disabled".to_string(),
        };
        let _ = session
            .text(serde_json::to_string(&err).unwrap_or_default())
            .await;
        let _ = session.close(None).await;
        return Ok(());
    }

    info!(channel_id, "AI live preview WebSocket connected");

    // Register this peer so we track viewer count; create publisher if needed.
    engine.runtime().webrtc_add_peer(channel_id);

    // Subscribe to server-generated ICE candidates for push to client.
    // When no subscription (None), use a never-yielding stream so we only process client messages.
    let mut ice_stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = Result<WebRtcSignaling, BroadcastStreamRecvError>> + Send>,
    > = match engine.runtime().subscribe_webrtc_server_ice(channel_id) {
        Some(rx) => Box::pin(BroadcastStream::new(rx)),
        None => Box::pin(stream::pending()),
    };

    loop {
        tokio::select! {
            // Server ICE candidates → push to client.
            Some(Ok(ice_msg)) = ice_stream.next() => {
                let json = serde_json::to_string(&ice_msg).unwrap_or_default();
                debug!(channel_id, "signaling → (server ICE)");
                if session.text(json).await.is_err() {
                    break;
                }
            }

            msg = msg_stream.next() => {
                let msg = match msg {
                    Some(m) => m,
                    None => break,
                };
                match msg {
                    Ok(WsMessage::Text(text)) => {
                        let msg: WebRtcSignaling = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(channel_id, error = %e, "invalid signaling message");
                                let err = WebRtcSignaling::Error {
                                    message: format!("invalid JSON: {e}"),
                                };
                                let _ = session
                                    .text(serde_json::to_string(&err).unwrap_or_default())
                                    .await;
                                continue;
                            }
                        };

                        debug!(channel_id, msg_type = %text.chars().take(60).collect::<String>(), "signaling ←");

                        match engine
                            .runtime()
                            .handle_webrtc_signaling(channel_id, msg)
                            .await
                        {
                            Ok(Some(response)) => {
                                let json = serde_json::to_string(&response).unwrap_or_default();
                                debug!(channel_id, "signaling →");
                                if session.text(json).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {
                                // No response needed (e.g., client ICE candidate was just added).
                            }
                            Err(e) => {
                                warn!(channel_id, error = %e, "signaling handler error");
                                let err = WebRtcSignaling::Error {
                                    message: e.to_string(),
                                };
                                let _ = session
                                    .text(serde_json::to_string(&err).unwrap_or_default())
                                    .await;
                            }
                        }
                    }

                    Ok(WsMessage::Ping(data)) => {
                        if session.pong(&data).await.is_err() {
                            break;
                        }
                    }

                    Ok(WsMessage::Close(_)) => {
                        info!(channel_id, "AI live preview WebSocket disconnected");
                        break;
                    }

                    Ok(_) => {
                        // Ignore binary / continuation / pong frames.
                    }

                    Err(e) => {
                        error!(channel_id, error = %e, "WebSocket protocol error");
                        break;
                    }
                }
            }
        }
    }

    // Unregister peer; tear down publisher when last viewer disconnects.
    engine.runtime().webrtc_remove_peer(channel_id);

    let _ = session.close(None).await;
    Ok(())
}
