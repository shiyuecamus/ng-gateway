//! Public branding endpoints (no authentication required).
//!
//! These endpoints are designed for:
//! - Login page rendering (before authentication)
//! - Browser favicon fetching
//! - Frontend bootstrap (runtime title/logo/favicon)
//!
//! # Routes
//! - GET `/branding.json`
//! - GET `/branding/logo`
//! - GET `/branding/favicon.ico`
//! - GET `/favicon.ico` (compat alias)

use actix_web::{
    http::header::{self, HeaderValue},
    web, HttpRequest, HttpResponse,
};
use ng_gateway_error::{web::WebError, WebResult};
use ng_gateway_models::domain::prelude::BrandingPublicConfig;
use ng_gateway_repository::BrandingRepository;
use sea_orm::prelude::DateTimeUtc;

/// Default URLs returned by `/branding.json`.
const BRANDING_LOGO_URL: &str = "/branding/logo";
const BRANDING_FAVICON_URL: &str = "/branding/favicon.ico";

/// Configure public branding routes.
pub fn configure_public_branding_routes(cfg: &mut web::ServiceConfig) {
    cfg.route("/branding.json", web::get().to(get_branding_json))
        .route(BRANDING_LOGO_URL, web::get().to(get_branding_logo))
        .route(BRANDING_FAVICON_URL, web::get().to(get_branding_favicon))
        // Compatibility route used by current `index.html`.
        .route("/favicon.ico", web::get().to(get_branding_favicon));
}

async fn get_branding_json(_req: HttpRequest) -> WebResult<HttpResponse> {
    let branding = BrandingRepository::get()
        .await?
        .ok_or(WebError::InternalError(
            "Branding is not initialized".to_string(),
        ))?;

    let body = BrandingPublicConfig::from_model(
        &branding,
        BRANDING_LOGO_URL.to_string(),
        BRANDING_FAVICON_URL.to_string(),
    );

    Ok(HttpResponse::Ok()
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .json(body))
}

async fn get_branding_logo(req: HttpRequest) -> WebResult<HttpResponse> {
    let branding = BrandingRepository::get()
        .await?
        .ok_or(WebError::InternalError(
            "Branding is not initialized".to_string(),
        ))?;

    let etag = build_weak_etag(branding.updated_at, branding.logo_bytes.len());
    if is_etag_match(&req, &etag) {
        return Ok(HttpResponse::NotModified()
            .insert_header((header::ETAG, etag))
            .finish());
    }

    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, branding.logo_mime))
        .insert_header((header::CACHE_CONTROL, "public, max-age=3600"))
        .insert_header((header::ETAG, etag))
        .body(branding.logo_bytes))
}

async fn get_branding_favicon(req: HttpRequest) -> WebResult<HttpResponse> {
    let branding = BrandingRepository::get()
        .await?
        .ok_or(WebError::InternalError(
            "Branding is not initialized".to_string(),
        ))?;

    let etag = build_weak_etag(branding.updated_at, branding.favicon_bytes.len());
    if is_etag_match(&req, &etag) {
        return Ok(HttpResponse::NotModified()
            .insert_header((header::ETAG, etag))
            .finish());
    }

    // Browser favicon caching can be aggressive; force revalidation.
    Ok(HttpResponse::Ok()
        .insert_header((header::CONTENT_TYPE, branding.favicon_mime))
        .insert_header((header::CACHE_CONTROL, "no-cache"))
        .insert_header((header::ETAG, etag))
        .body(branding.favicon_bytes))
}

#[inline]
fn build_weak_etag(updated_at: Option<DateTimeUtc>, len: usize) -> HeaderValue {
    let ts = updated_at.map(|t| t.timestamp_millis()).unwrap_or(0);
    // Weak ETag is sufficient for immutable binary replacement.
    let value = format!("W/\"{}-{}\"", ts, len);
    HeaderValue::from_str(value.as_str()).unwrap_or_else(|_| HeaderValue::from_static("W/\"0-0\""))
}

#[inline]
fn is_etag_match(req: &HttpRequest, etag: &HeaderValue) -> bool {
    req.headers()
        .get(header::IF_NONE_MATCH)
        .is_some_and(|v| v == etag)
}
