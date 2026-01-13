//! System branding management API endpoints (SYSTEM_ADMIN only).
//!
//! Branding is a **global** configuration and affects all users (including unauthenticated pages).
//! This module provides authenticated endpoints for updating:
//! - title
//! - logo
//! - favicon
//!
//! Public read endpoints are implemented separately in `crate::api::public`.

use crate::{middleware::RequestContext, rbac::has_any_role};
use actix_multipart::Multipart;
use actix_web::{http::Method, web};
use futures::StreamExt;
use ng_gateway_common::casbin::NGPermChecker;
use ng_gateway_error::{rbac::RBACError, web::WebError, WebResult};
use ng_gateway_models::web::WebResponse;
use ng_gateway_models::{
    constants::SYSTEM_ADMIN_ROLE_CODE, domain::prelude::UpdateBrandingTitle, PermChecker,
};
use ng_gateway_repository::BrandingRepository;
use tracing::{info, instrument};

pub(super) const ROUTER_PREFIX: &str = "/branding";

/// Maximum allowed logo size in bytes.
///
/// This limit is enforced server-side as the final safety line.
const MAX_LOGO_BYTES: usize = 10 * 1024 * 1024;

/// Maximum allowed favicon size in bytes.
const MAX_FAVICON_BYTES: usize = 256 * 1024;

/// Configure branding management routes.
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/title", web::put().to(update_title))
        .route("/logo", web::post().to(upload_logo))
        .route("/favicon", web::post().to(upload_favicon));
}

/// Initialize RBAC rules for branding module.
#[inline]
#[instrument(name = "init-branding-rbac", skip(router_prefix, perm_checker))]
pub(crate) async fn init_rbac_rules(
    router_prefix: &str,
    perm_checker: &NGPermChecker,
) -> WebResult<(), RBACError> {
    info!("Initializing branding module RBAC rules...");

    perm_checker
        .register(
            Method::PUT,
            format!("{router_prefix}{ROUTER_PREFIX}/title"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/logo"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    perm_checker
        .register(
            Method::POST,
            format!("{router_prefix}{ROUTER_PREFIX}/favicon"),
            has_any_role(&[SYSTEM_ADMIN_ROLE_CODE])?,
        )
        .await?;

    info!("Branding module RBAC rules initialized successfully");
    Ok(())
}

async fn update_title(
    _ctx: RequestContext,
    payload: actix_web_validator::Json<UpdateBrandingTitle>,
) -> WebResult<WebResponse<()>> {
    let payload = payload.into_inner();
    BrandingRepository::update_title(payload.title).await?;
    Ok(WebResponse::<()>::ok_empty())
}

async fn upload_logo(_ctx: RequestContext, mut multipart: Multipart) -> WebResult<WebResponse<()>> {
    let (mime, bytes) =
        read_single_file_multipart(&mut multipart, MAX_LOGO_BYTES, AssetKind::Logo).await?;
    BrandingRepository::update_logo(mime, bytes).await?;
    Ok(WebResponse::<()>::ok_empty())
}

async fn upload_favicon(
    _ctx: RequestContext,
    mut multipart: Multipart,
) -> WebResult<WebResponse<()>> {
    let (mime, bytes) =
        read_single_file_multipart(&mut multipart, MAX_FAVICON_BYTES, AssetKind::Favicon).await?;
    BrandingRepository::update_favicon(mime, bytes).await?;
    Ok(WebResponse::<()>::ok_empty())
}

#[derive(Debug, Clone, Copy)]
enum AssetKind {
    Logo,
    Favicon,
}

impl AssetKind {
    #[inline]
    fn allowed_mimes(&self) -> &'static [&'static str] {
        match self {
            AssetKind::Logo => &["image/png", "image/webp", "image/jpeg"],
            AssetKind::Favicon => &["image/x-icon", "image/vnd.microsoft.icon", "image/png"],
        }
    }
}

/// Read a single-file multipart upload into memory with a strict size limit.
///
/// # Errors
/// - Returns 400 when no file is provided, MIME is not allowed, or size exceeds the limit.
async fn read_single_file_multipart(
    multipart: &mut Multipart,
    max_bytes: usize,
    kind: AssetKind,
) -> WebResult<(String, Vec<u8>)> {
    let mut field = multipart
        .next()
        .await
        .ok_or(WebError::BadRequest("No file uploaded".to_string()))??;

    // Determine MIME type.
    let mime = field
        .content_type()
        .map(|m| m.to_string())
        .unwrap_or_default();

    if mime.is_empty() || !kind.allowed_mimes().iter().any(|allowed| *allowed == mime) {
        return Err(WebError::BadRequest(format!(
            "Invalid content-type: `{}` (allowed: {:?})",
            if mime.is_empty() {
                "<empty>"
            } else {
                mime.as_str()
            },
            kind.allowed_mimes()
        )));
    }

    // Read bytes with a hard limit to avoid memory abuse.
    let mut buf: Vec<u8> = Vec::new();
    let mut total: usize = 0;
    while let Some(chunk) = field.next().await {
        let data = chunk?;
        total = total.saturating_add(data.len());
        if total > max_bytes {
            return Err(WebError::BadRequest(format!(
                "File too large: {} bytes (max {})",
                total, max_bytes
            )));
        }
        buf.extend_from_slice(&data);
    }

    if buf.is_empty() {
        return Err(WebError::BadRequest("Empty file uploaded".to_string()));
    }

    Ok((mime, buf))
}
