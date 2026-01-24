use crate::AppState;
use actix_web::{web, Error as ActixError, HttpRequest, HttpResponse};
use actix_ws::Session;
use futures::Future;
use std::sync::Arc;
use tracing::error;

/// Unified WebSocket upgrade + spawn wrapper for all v1 ws endpoints.
///
/// This keeps the handler shape consistent across modules.
pub async fn ws_upgrade_and_spawn<F, Fut>(
    req: HttpRequest,
    body: web::Payload,
    state: web::Data<Arc<AppState>>,
    loop_fn: F,
) -> Result<HttpResponse, ActixError>
where
    // NOTE: Actix WS message stream is !Send, so we intentionally do NOT require Send here.
    F: FnOnce(Arc<AppState>, Session, actix_ws::MessageStream) -> Fut + 'static,
    Fut: Future<Output = Result<(), ActixError>> + 'static,
{
    let (res, session, msg_stream) = actix_ws::handle(&req, body)?;
    let state: Arc<AppState> = state.get_ref().clone();

    actix_rt::spawn(async move {
        if let Err(e) = loop_fn(state, session, msg_stream).await {
            error!("WS loop error: {}", e);
        }
    });

    Ok(res)
}
