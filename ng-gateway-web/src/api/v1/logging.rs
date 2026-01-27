use crate::rbac::{has_any_role, has_scope};
use actix_web::{
    http::{header, Method, StatusCode},
    web::{self, ServiceConfig},
    HttpResponse,
};
use bytes::Bytes;
use ng_gateway_common::{casbin::NGPermChecker, log::control, NGAppContext};
use ng_gateway_error::{rbac::RBACError, web::WebError, NGResult, WebResult};
use ng_gateway_models::{
    constants::{LOG_DIR, SYSTEM_ADMIN_ROLE_CODE},
    domain::prelude::{
        DownloadLogFilesRequest, GlobalLogLevelView, LogFileInfo, LogFilesListResponse, LogLevel,
        SetGlobalLogLevelRequest, TtlRange,
    },
    rbac::PermRule,
    web::WebResponse,
    PermChecker,
};
use ng_gateway_utils::{log_files, zip_stream};
use std::{collections::HashSet, io, path::PathBuf};
use tokio::sync::mpsc;
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tracing::instrument;

pub(super) const ROUTER_PREFIX: &str = "/logging";

pub(crate) fn configure_routes(cfg: &mut ServiceConfig) {
    cfg.route("/level", web::get().to(get_global_level))
        .route("/level", web::put().to(set_global_level))
        .route("/files", web::get().to(list_log_files))
        .route("/download", web::post().to(download_log_files));
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

    perm_checker
        .register(
            Method::GET,
            format!("{router_prefix}{ROUTER_PREFIX}/files"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?.or(has_scope("system:logging")?),
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/download"),
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

/// List available log files.
///
/// # Endpoint
/// `GET /logging/files`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role or `system:logging` scope
///
/// # Returns
/// - `WebResult<WebResponse<LogFilesListResponse>>`: List of available log files
#[instrument(name = "list-log-files", skip_all)]
pub async fn list_log_files() -> WebResult<WebResponse<LogFilesListResponse>> {
    let log_dir = PathBuf::from(LOG_DIR);
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

    // Sort: newest first, then name for stability.
    files.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(WebResponse::ok(LogFilesListResponse { files }))
}

/// Download selected log files as a ZIP archive.
///
/// # Endpoint
/// `POST /logging/download`
///
/// # Authorization
/// Requires `SYSTEM_ADMIN` role or `system:logging` scope
///
/// # Performance
/// - Uses streaming ZIP creation for large files
/// - Processes files in parallel where possible
///
/// # Returns
/// - `WebResult<HttpResponse>`: ZIP file download
#[instrument(name = "download-log-files", skip_all)]
pub async fn download_log_files(
    req: web::Json<DownloadLogFilesRequest>,
) -> WebResult<HttpResponse> {
    let requested = req.into_inner().files;
    if requested.is_empty() {
        return Err(WebError::BadRequest("files cannot be empty".to_string()));
    }

    let log_dir = PathBuf::from(LOG_DIR);
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

    // True streaming ZIP:
    // - Produce ZIP bytes in a blocking task (zip writer is sync + CPU heavy)
    // - Push chunks into a bounded channel for backpressure
    // - Return an actix streaming body that pulls from the channel
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
        // Drop sender to close stream.
        drop(tx);
    });

    let stream =
        ReceiverStream::new(rx).map(|r| r.map_err(actix_web::error::ErrorInternalServerError));
    Ok(HttpResponse::build(StatusCode::OK)
        .insert_header((header::CONTENT_TYPE, "application/zip"))
        // Do not apply additional transformations (e.g. HTTP compression) on binary ZIP stream.
        // This avoids edge cases where middleware tries to (re)compress an already compressed format.
        .insert_header((header::CONTENT_ENCODING, "identity"))
        .insert_header((header::CACHE_CONTROL, "no-transform"))
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{zip_filename}\""),
        ))
        .streaming(stream))
}
