use crate::rbac::{has_any_role, has_scope};
use actix_web::{
    http::Method,
    web::{self, ServiceConfig},
};
use ng_gateway_common::{casbin::NGPermChecker, log::control, NGAppContext};
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{GlobalLogLevelView, LogLevel, SetGlobalLogLevelRequest, TtlRange},
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use tracing::instrument;

pub(super) const ROUTER_PREFIX: &str = "/logging";

pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg.route("/level", web::get().to(get_global_level))
        .route("/level", web::put().to(set_global_level));
}

#[inline]
#[instrument(name = "init-logging-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    // Global log level control is a sensitive operation: system admin OR explicit scope.
    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:logging")?),
        )
        .await?;

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/level"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:logging")?),
        )
        .await?;

    Ok(())
}

#[instrument(name = "get-global-log-level", skip_all)]
pub async fn get_global_level() -> WebResult<WebResponse<GlobalLogLevelView>> {
    let ctx = NGAppContext::instance().await;
    let baseline = LogLevel::from(ctx.log_level());

    let rt = control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let effective = rt.overrides().effective_global_level();
    let s = rt.settings();

    Ok(WebResponse::ok(GlobalLogLevelView {
        baseline,
        effective,
        channel_override_ttl: TtlRange {
            min_ms: s.channel_override_min_ttl_ms,
            max_ms: s.channel_override_max_ttl_ms,
            default_ms: s.channel_override_default_ttl_ms,
        },
    }))
}

#[instrument(name = "set-global-log-level", skip_all)]
pub async fn set_global_level(
    req: web::Json<SetGlobalLogLevelRequest>,
) -> WebResult<WebResponse<GlobalLogLevelView>> {
    let level = req.into_inner().level;

    // Apply baseline level to host logger (also syncs override manager + driver sink best-effort).
    let ctx = NGAppContext::instance().await;
    ctx.change_log_level(level.into());

    // Return updated view.
    get_global_level().await
}
