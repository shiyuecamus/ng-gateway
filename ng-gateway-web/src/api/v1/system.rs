use crate::rbac::{has_any_role, has_scope};
use actix_web::{
    http::{header, Method, StatusCode},
    web::{self, ServiceConfig},
    HttpResponse,
};
use actix_web_validator::Json;
use bytes::Bytes;
use ng_gateway_common::NGAppContext;
use ng_gateway_common::{
    casbin::NGPermChecker, log::control, settings::control as settings_control,
};
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE,
    domain::prelude::{
        ApplySystemSettingsResult, CleanupLogFilesRequest, CleanupLogFilesResponse,
        CollectorSettingsView, DownloadLogFilesRequest, GlobalLogLevelView, LogFileInfo,
        LogFilesListResponse, LogLevel, LoggingCleanupSettingsView, LoggingControlSettingsView,
        LoggingOutputSettingsView, NorthwardSettingsView, PatchCollectorSettingsRequest,
        PatchLoggingCleanupSettingsRequest, PatchLoggingControlSettingsRequest,
        PatchLoggingOutputSettingsRequest, PatchNorthwardSettingsRequest,
        PatchSouthwardSettingsRequest, SetGlobalLogLevelRequest, SouthwardSettingsView,
        SystemSettingsOverviewView, TtlRange,
    },
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_utils::{log_files, zip_stream};
use once_cell::sync::Lazy;
use std::{collections::HashSet, io, path::PathBuf};
use tokio::sync::{mpsc, Mutex};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

pub(super) const ROUTER_PREFIX: &str = "/system";

/// Serialize system settings PATCH operations without blocking unrelated requests.
///
/// # Why this exists
/// Previously, PATCH handlers used the global `NGAppContext` write lock to serialize apply+persist.
/// If a PATCH performs blocking I/O (e.g. config file persistence), the write lock can stall *all*
/// other API requests that need a read lock, causing the UI to appear “fully stuck”.
///
/// This mutex keeps the "only one PATCH at a time" guarantee while allowing other APIs to keep
/// serving (they only need a read lock on the global context).
static SYSTEM_SETTINGS_PATCH_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg.service(
        web::scope("/settings")
            .route("", web::get().to(get_system_settings_overview))
            .route("/collector", web::get().to(get_collector_settings))
            .route("/collector", web::patch().to(patch_collector_settings))
            .route("/northward", web::get().to(get_northward_settings))
            .route("/northward", web::patch().to(patch_northward_settings))
            .route("/southward", web::get().to(get_southward_settings))
            .route("/southward", web::patch().to(patch_southward_settings))
            .route("/logging_runtime", web::get().to(get_logging_runtime))
            .route("/logging_runtime", web::patch().to(patch_logging_runtime))
            .route(
                "/logging_control",
                web::get().to(get_logging_control_settings),
            )
            .route(
                "/logging_control",
                web::patch().to(patch_logging_control_settings),
            )
            .route(
                "/logging_output",
                web::get().to(get_logging_output_settings),
            )
            .route(
                "/logging_output",
                web::patch().to(patch_logging_output_settings),
            )
            .route(
                "/logging_cleanup",
                web::get().to(get_logging_cleanup_settings),
            )
            .route(
                "/logging_cleanup",
                web::patch().to(patch_logging_cleanup_settings),
            )
            .route("/logging_files", web::get().to(list_log_files))
            .route(
                "/logging_files/download",
                web::post().to(download_log_files),
            )
            .route("/logging_files/cleanup", web::post().to(cleanup_log_files))
            .route("/metrics", web::get().to(not_implemented))
            .route("/metrics", web::patch().to(not_implemented)),
    );
}

#[inline]
#[instrument(name = "init-system-settings-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> NGResult<(), RBACError> {
    let rules: [(Method, String, Box<dyn PermRule>); 20] = [
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/collector"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/collector"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/northward"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/northward"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/southward"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/southward"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_runtime"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_runtime"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_control"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_control"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_output"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_output"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_cleanup"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_cleanup"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_files"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_files/download"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/logging_files/cleanup"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/metrics"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::PATCH,
            format!("{router_prefix}{ROUTER_PREFIX}/settings/metrics"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
        (
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/settings"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:settings")?),
        ),
    ];

    for (method, path, rule) in rules {
        perm_checker.register(method, path, rule).await?;
    }

    Ok(())
}

#[instrument(name = "get-system-settings-overview", skip_all)]
pub async fn get_system_settings_overview() -> WebResult<WebResponse<SystemSettingsOverviewView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;

    let collector = settings_control::build_collector_view(settings)?;
    let northward = settings_control::build_northward_view(settings)?;
    let southward = settings_control::build_southward_view(settings)?;
    let logging_output = settings_control::build_logging_output_view(settings)?;
    let logging_cleanup = settings_control::build_logging_cleanup_view(settings)?;

    let baseline = LogLevel::from(ctx.log_level());
    let rt = control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let effective = rt.overrides().effective_global_level();
    let s = rt.settings();
    let logging_runtime = GlobalLogLevelView {
        baseline,
        effective,
        channel_override_ttl: TtlRange {
            min_ms: s.channel_override_min_ttl_ms,
            max_ms: s.channel_override_max_ttl_ms,
            default_ms: s.channel_override_default_ttl_ms,
        },
    };
    let logging_control =
        settings_control::build_logging_control_view(settings, s).map_err(|e| {
            WebError::InternalError(format!("Failed to build logging_control view: {e}"))
        })?;

    Ok(WebResponse::ok(SystemSettingsOverviewView {
        collector,
        northward,
        southward,
        logging_runtime,
        logging_control,
        logging_output,
        logging_cleanup,
    }))
}

#[instrument(name = "not-implemented", skip_all)]
async fn not_implemented() -> WebResult<WebResponse<()>> {
    Err(WebError::BadRequest("not implemented".to_string()))
}

#[instrument(name = "get-collector-settings", skip_all)]
pub async fn get_collector_settings() -> WebResult<WebResponse<CollectorSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let view = settings_control::build_collector_view(settings)?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "patch-collector-settings", skip_all)]
pub async fn patch_collector_settings(
    req: Json<PatchCollectorSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();

    // Serialize apply + persist without blocking unrelated requests.
    let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;

    let (gateway, max_concurrency, outbound_capacity, mut result) = {
        let ctx = NGAppContext::instance().await;
        let gateway = ctx.gateway()?;
        let settings = ctx.settings()?;
        let result = settings_control::apply_collector_settings(settings, req)
            .map_err(|e| WebError::InternalError(e.to_string()))?;
        let max_concurrency = settings.general.collector.max_concurrent_collections();
        let outbound_capacity = settings.general.collector.outbound_queue_capacity();
        (gateway, max_concurrency, outbound_capacity, result)
    };

    // Apply runtime effects outside the global write lock to avoid lock inversion/deadlocks.
    if !result.changed_keys.is_empty() {
        if let Err(e) = gateway
            .apply_collector_runtime_tuning(
                &result.changed_keys,
                max_concurrency,
                outbound_capacity,
            )
            .await
        {
            result.runtime_warning = Some(format!(
                "Settings applied/persisted, but runtime effect hook failed (some changes may require a component restart to take effect): {e}"
            ));
        }
    }

    Ok(WebResponse::ok(result))
}

#[instrument(name = "get-northward-settings", skip_all)]
pub async fn get_northward_settings() -> WebResult<WebResponse<NorthwardSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let view = settings_control::build_northward_view(settings)?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "get-southward-settings", skip_all)]
pub async fn get_southward_settings() -> WebResult<WebResponse<SouthwardSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let view = settings_control::build_southward_view(settings)?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "patch-northward-settings", skip_all)]
pub async fn patch_northward_settings(
    req: Json<PatchNorthwardSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();
    let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;
    let (gateway, queue_capacity, mut result) = {
        let ctx = NGAppContext::instance().await;
        let gateway = ctx.gateway()?;
        let settings = ctx.settings()?;
        let result = settings_control::apply_northward_settings(settings, req)
            .map_err(|e| WebError::InternalError(e.to_string()))?;
        let queue_capacity = settings.general.northward.queue_capacity();
        (gateway, queue_capacity, result)
    };

    // Apply runtime effects outside the global write lock.
    if !result.changed_keys.is_empty() {
        if let Err(e) = gateway
            .apply_northward_runtime_tuning(&result.changed_keys, queue_capacity)
            .await
        {
            result.runtime_warning = Some(format!(
                "Settings applied/persisted, but runtime effect hook failed (some changes may require a component restart to take effect): {e}"
            ));
        }
    }

    Ok(WebResponse::ok(result))
}

#[instrument(name = "patch-southward-settings", skip_all)]
pub async fn patch_southward_settings(
    req: Json<PatchSouthwardSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();
    let result = {
        let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;
        settings_control::apply_southward_settings(settings, req)
            .map_err(|e| WebError::InternalError(e.to_string()))?
    };
    Ok(WebResponse::ok(result))
}

#[instrument(name = "get-logging-runtime-settings", skip_all)]
pub async fn get_logging_runtime() -> WebResult<WebResponse<GlobalLogLevelView>> {
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

#[instrument(name = "patch-logging-runtime-settings", skip_all)]
pub async fn patch_logging_runtime(
    req: web::Json<SetGlobalLogLevelRequest>,
) -> WebResult<WebResponse<GlobalLogLevelView>> {
    let level = req.into_inner().level;
    let ctx = NGAppContext::instance().await;
    ctx.change_log_level(level.into());
    get_logging_runtime().await
}

#[instrument(name = "get-logging-control-settings", skip_all)]
pub async fn get_logging_control_settings() -> WebResult<WebResponse<LoggingControlSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let rt = control::global().ok_or(WebError::InternalError(
        "Log control runtime is not initialized".to_string(),
    ))?;
    let s = rt.settings();
    let view = settings_control::build_logging_control_view(settings, s)
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "patch-logging-control-settings", skip_all)]
pub async fn patch_logging_control_settings(
    req: Json<PatchLoggingControlSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();
    let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let result = settings_control::apply_logging_control_settings(settings, req)
        .map_err(|e| WebError::InternalError(e.to_string()))?;
    Ok(WebResponse::ok(result))
}

#[instrument(name = "get-logging-output-settings", skip_all)]
pub async fn get_logging_output_settings() -> WebResult<WebResponse<LoggingOutputSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let view = settings_control::build_logging_output_view(settings)?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "patch-logging-output-settings", skip_all)]
pub async fn patch_logging_output_settings(
    req: Json<PatchLoggingOutputSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();

    let (output, mut result) = {
        let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;
        let result = settings_control::apply_logging_output_settings(settings, req)
            .map_err(|e| WebError::InternalError(e.to_string()))?;
        let output = settings.logging.output.get();
        (output, result)
    };

    // Reload logging output layer outside the global write lock.
    let ctx = NGAppContext::instance().await;
    if let Err(e) = ctx.logger().reload_output(&output) {
        result.runtime_warning = Some(format!("Runtime applied, but failed to reload logger: {e}"));
    }

    Ok(WebResponse::ok(result))
}

#[instrument(name = "get-logging-cleanup-settings", skip_all)]
pub async fn get_logging_cleanup_settings() -> WebResult<WebResponse<LoggingCleanupSettingsView>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let view = settings_control::build_logging_cleanup_view(settings)?;
    Ok(WebResponse::ok(view))
}

#[instrument(name = "patch-logging-cleanup-settings", skip_all)]
pub async fn patch_logging_cleanup_settings(
    req: Json<PatchLoggingCleanupSettingsRequest>,
) -> WebResult<WebResponse<ApplySystemSettingsResult>> {
    let req = req.into_inner();

    let result = {
        let _patch_guard = SYSTEM_SETTINGS_PATCH_LOCK.lock().await;
        let ctx = NGAppContext::instance().await;
        let settings = ctx.settings()?;
        settings_control::apply_logging_cleanup_settings(settings, req)
            .map_err(|e| WebError::InternalError(e.to_string()))?
    };
    Ok(WebResponse::ok(result))
}

#[instrument(name = "list-log-files", skip_all)]
pub async fn list_log_files() -> WebResult<WebResponse<LogFilesListResponse>> {
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let output = settings.logging.output.get();
    let log_dir = PathBuf::from(output.file.dir);

    let scan = log_files::scan_log_dir(&log_dir)
        .map_err(|e| WebError::InternalError(format!("Failed to scan log directory: {e}")))?;

    let mut files: Vec<LogFileInfo> = scan
        .files
        .into_iter()
        .map(|f| LogFileInfo {
            name: f.name,
            size: f.size,
            modified_at: f.modified_at_ms,
        })
        .collect();

    files.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(WebResponse::ok(LogFilesListResponse { files }))
}

#[instrument(name = "download-log-files", skip_all)]
pub async fn download_log_files(
    req: web::Json<DownloadLogFilesRequest>,
) -> WebResult<HttpResponse> {
    let requested = req.into_inner().files;
    if requested.is_empty() {
        return Err(WebError::BadRequest("files cannot be empty".to_string()));
    }

    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let output = settings.logging.output.get();
    let log_dir = PathBuf::from(output.file.dir);

    let allowed_map = log_files::build_allowed_map(&log_dir)
        .map_err(|e| WebError::InternalError(format!("Failed to scan log directory: {e}")))?;
    if allowed_map.is_empty() {
        return Err(WebError::NotFound("No log files found".to_string()));
    }

    // Security: validate requested names are safe and exist in log dir scan.
    let mut unique: Vec<String> = Vec::with_capacity(requested.len());
    let mut seen: HashSet<String> = HashSet::with_capacity(requested.len());
    for name in requested.into_iter() {
        let name = name.trim().to_string();
        log_files::validate_safe_file_name(&name)
            .map_err(|e| WebError::BadRequest(e.to_string()))?;
        if seen.insert(name.clone()) {
            unique.push(name);
        }
    }

    // Resolve to absolute paths (under logs dir) using allowed set only.
    let mut entries: Vec<(String, PathBuf)> = Vec::with_capacity(unique.len());
    for name in unique.iter() {
        let Some(path) = allowed_map.get(name) else {
            return Err(WebError::NotFound(format!("Log file not found: {name}")));
        };
        entries.push((name.clone(), path.clone()));
    }

    // Generate filename with timestamp
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let zip_filename = format!("ng-gateway-logs_{timestamp}.zip");

    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(32);
    tokio::task::spawn_blocking(move || {
        match zip_stream::stream_zip_stored(entries, tx.clone()) {
            Ok(()) => {
                tracing::debug!("Log ZIP streaming finished successfully");
            }
            Err(e) => {
                tracing::error!(error=%e, "Log ZIP streaming failed");
                let _ = tx.blocking_send(Err(e));
            }
        }
        drop(tx);
    });

    let stream =
        ReceiverStream::new(rx).map(|r| r.map_err(actix_web::error::ErrorInternalServerError));
    Ok(HttpResponse::build(StatusCode::OK)
        .insert_header((header::CONTENT_TYPE, "application/zip"))
        .insert_header((header::CONTENT_ENCODING, "identity"))
        .insert_header((header::CACHE_CONTROL, "no-transform"))
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_filename}\""),
        ))
        .streaming(stream))
}

#[instrument(name = "cleanup-log-files", skip_all)]
pub async fn cleanup_log_files(
    req: web::Json<CleanupLogFilesRequest>,
) -> WebResult<WebResponse<CleanupLogFilesResponse>> {
    let dry_run = req.into_inner().dry_run;
    let ctx = NGAppContext::instance().await;
    let settings = ctx.settings()?;
    let report = ng_gateway_common::log::cleanup_logs_once(settings, dry_run)
        .map_err(|e| WebError::InternalError(e.to_string()))?;

    let deleted: Vec<LogFileInfo> = report
        .deleted
        .into_iter()
        .map(|f| LogFileInfo {
            name: f.name,
            size: f.size,
            modified_at: f.modified_at_ms,
        })
        .collect();

    Ok(WebResponse::ok(CleanupLogFilesResponse {
        deleted,
        freed_bytes: report.freed_bytes,
        protected_active: report.protected_active,
    }))
}
