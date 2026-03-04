use crate::AppState;
use ng_gateway_error::web::WebError;
use ng_gateway_models::AiEngineApi;
use std::sync::Arc;

/// Extract the AI engine from gateway state or return 503.
#[inline]
pub(super) fn require_ai_engine(state: &AppState) -> Result<Arc<dyn AiEngineApi>, WebError> {
    state
        .gateway
        .ai_engine()
        .ok_or(WebError::InternalError("AI engine is not enabled".into()))
}
