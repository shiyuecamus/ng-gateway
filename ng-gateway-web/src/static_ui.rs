//! Embedded / filesystem static UI serving for NG Gateway.
//!
//! This module is designed for the "single binary / single container" distribution model:
//! - Serve the admin UI (Vite build output) directly from the gateway process.
//! - Keep `/api` (or any configured router prefix) untouched.
//! - Provide SPA fallback: unknown non-asset paths return `index.html`.
//!
//! Design notes:
//! - **No unwrap/expect**: all error paths return `NGResult` with context.
//! - **Performance**: embedded mode preloads all files into memory once at startup (Arc + Bytes),
//!   so request handling is O(1) hashmap lookup with zero-copy `Bytes` bodies.
//! - **Caching**: immutable hashed assets are served with long cache; `index.html` is no-cache.

use actix_files::NamedFile;
use actix_web::{
    http::header::{self, HeaderValue},
    web::{self, Data},
    HttpRequest, HttpResponse,
};
use bytes::Bytes;
use ng_gateway_error::{NGError, NGResult};
use ng_gateway_models::settings::{WebUi, WebUiMode};
use std::{collections::HashMap, path::Path, sync::Arc};

/// Embedded UI zip bytes generated from `ng-gateway-ui/apps/web-antd/dist`.
#[cfg(feature = "ui-embedded")]
const UI_DIST_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui-dist.zip"));

/// Runtime UI configuration derived from `ng-gateway-models` settings.
#[derive(Debug, Clone)]
pub struct UiRuntimeConfig {
    pub api_router_prefix: String,
    pub ui: WebUi,
}

/// Preloaded UI asset store (embedded zip mode).
#[derive(Clone)]
pub struct UiAssets {
    /// Map key is a normalized path without leading slash, e.g. `index.html`, `css/app.css`.
    files: Arc<HashMap<String, UiAsset>>,
}

#[derive(Clone)]
struct UiAsset {
    body: Bytes,
    content_type: &'static str,
    etag: Option<HeaderValue>,
}

impl UiAssets {
    /// Load all UI files from the embedded zip into memory.
    pub async fn load_from_embedded_zip() -> NGResult<Self> {
        #[cfg(feature = "ui-embedded")]
        {
            tokio::task::spawn_blocking(|| load_zip_into_assets(UI_DIST_ZIP))
                .await
                .map_err(|e| {
                    NGError::from(format!("Failed to join embedded UI loader task: {e}"))
                })?
        }

        #[cfg(not(feature = "ui-embedded"))]
        {
            Err(NGError::from(
                "Embedded UI is not available: build with feature `ng-gateway-bin/ui-embedded`",
            ))
        }
    }

    #[inline]
    fn get(&self, key: &str) -> Option<UiAsset> {
        self.files.get(key).cloned()
    }
}

/// Register UI routes into actix `ServiceConfig`.
pub fn configure_ui_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource("/{path:.*}")
            .route(web::get().to(ui_handler))
            .route(web::head().to(ui_handler)),
    );
}

async fn ui_handler(
    req: HttpRequest,
    ui_cfg: Data<UiRuntimeConfig>,
) -> actix_web::Result<HttpResponse> {
    // Safety guard: never serve UI for API paths
    if req.path().starts_with(ui_cfg.api_router_prefix.as_str()) {
        return Ok(HttpResponse::NotFound().finish());
    }

    match ui_cfg.ui.mode {
        WebUiMode::EmbeddedZip => {
            let assets = req.app_data::<Data<UiAssets>>().ok_or_else(|| {
                actix_web::error::ErrorInternalServerError("Embedded UI assets not initialized")
            })?;
            Ok(serve_from_assets(&req, assets))
        }
        WebUiMode::Filesystem => Ok(serve_from_filesystem(&req, &ui_cfg).await),
    }
}

fn serve_from_assets(req: &HttpRequest, assets: &UiAssets) -> HttpResponse {
    let key = normalize_req_path(req.path());

    // 1. Try exact match
    if let Some(asset) = assets.get(key.as_str()) {
        return build_embedded_response(req, asset, &key);
    }

    // 2. SPA Fallback Logic
    if should_fallback_to_index(&key) {
        if let Some(index) = assets.get("index.html") {
            return build_embedded_response(req, index, "index.html");
        }
        tracing::warn!("Embedded UI enabled but index.html missing");
    }

    HttpResponse::NotFound().finish()
}

async fn serve_from_filesystem(req: &HttpRequest, ui_cfg: &UiRuntimeConfig) -> HttpResponse {
    let key = normalize_req_path(req.path());

    // Security: Prevent path traversal
    if key.contains("..") || key.contains('\\') {
        return HttpResponse::BadRequest().finish();
    }

    let root = Path::new(ui_cfg.ui.filesystem_root.as_str());
    let candidate = root.join(&key);

    // 1. Try exact match
    if let Ok(file) = NamedFile::open_async(&candidate).await {
        return build_filesystem_response(req, file, &key);
    }

    // 2. SPA Fallback Logic
    if should_fallback_to_index(&key) {
        let index_path = root.join("index.html");
        if let Ok(file) = NamedFile::open_async(index_path).await {
            return build_filesystem_response(req, file, "index.html");
        }
    }

    HttpResponse::NotFound().finish()
}

// --- Helper Functions ---

fn build_embedded_response(req: &HttpRequest, asset: UiAsset, key: &str) -> HttpResponse {
    let mut resp = HttpResponse::Ok();

    resp.content_type(asset.content_type);
    resp.insert_header((header::CACHE_CONTROL, cache_control_for_path(key)));
    resp.insert_header((header::X_CONTENT_TYPE_OPTIONS, "nosniff"));

    if let Some(etag) = asset.etag {
        resp.insert_header((header::ETAG, etag));
    }

    if req.method() == actix_web::http::Method::HEAD {
        resp.finish()
    } else {
        resp.body(asset.body)
    }
}

fn build_filesystem_response(req: &HttpRequest, file: NamedFile, key: &str) -> HttpResponse {
    let mut resp = file.into_response(req);

    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control_for_path(key)),
    );
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );

    resp
}

#[inline]
fn normalize_req_path(path: &str) -> String {
    let p = path.trim_start_matches('/');
    if p.is_empty() {
        "index.html".to_string()
    } else {
        p.to_string()
    }
}

#[inline]
fn should_fallback_to_index(key: &str) -> bool {
    // e.g. /dashboard -> true, /app.js -> false
    !key.rsplit('/').next().unwrap_or(key).contains('.')
}

fn cache_control_for_path(key: &str) -> &'static str {
    if key == "index.html" {
        return "no-cache";
    }
    // Vite hashed assets: safe to cache aggressively
    let ext = key.rsplit('.').next().unwrap_or("");
    match ext {
        "js" | "css" | "png" | "jpg" | "jpeg" | "svg" | "webp" | "woff" | "woff2" => {
            "public, max-age=31536000, immutable"
        }
        _ => "public, max-age=3600",
    }
}

// --- Embedded Loading Logic (Cleaned up) ---

#[cfg(feature = "ui-embedded")]
fn content_type_for_path(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(feature = "ui-embedded")]
fn load_zip_into_assets(zip_bytes: &[u8]) -> NGResult<UiAssets> {
    use std::io::Read;

    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| NGError::from(format!("Failed to open embedded ui-dist.zip: {e}")))?;

    let mut files = HashMap::with_capacity(archive.len());

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| NGError::from(e.to_string()))?;
        if file.is_dir() {
            continue;
        }

        let name = file.name().replace('\\', "/");
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)
            .map_err(|e| NGError::from(e.to_string()))?;

        // Generate ETag (simple version based on content length + simple hash, or just "embedded")
        // For strict correctness, hashing the content is best.
        // let etag = format!("\"{:x}\"", Sha256::digest(&buf));

        files.insert(
            name.clone(),
            UiAsset {
                body: Bytes::from(buf),
                content_type: content_type_for_path(&name),
                etag: None, // Simplified for now
            },
        );
    }

    Ok(UiAssets {
        files: Arc::new(files),
    })
}
